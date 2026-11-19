//! Title text as a picture: rasterised once per text, size and colour, then
//! composited like any other layer.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};

/// Straight-alpha RGBA, rows top to bottom.
pub struct TextImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The fonts of a project as they are read from `assets/font/`, kept for
/// the next title that uses them. For the preview and export threads: it
/// reads files.
#[derive(Default)]
pub struct Fonts {
    read: std::collections::HashMap<String, Option<std::sync::Arc<Vec<u8>>>>,
}

impl Fonts {
    /// The bytes of font `name`, or `None` for the built-in font (also
    /// when the file is missing: the title is then drawn with the built-in
    /// one rather than not at all, and that is reported once).
    pub fn get(&mut self, root: &std::path::Path, name: &str) -> Option<std::sync::Arc<Vec<u8>>> {
        // A plain file name: nothing that could lead out of the folder.
        if name.is_empty() || name.contains(['/', '\\']) || name == ".." {
            return None;
        }
        if let Some(known) = self.read.get(name) {
            return known.clone();
        }
        let bytes = match std::fs::read(root.join("assets/font").join(name)) {
            Ok(bytes) if FontRef::try_from_slice(&bytes).is_ok() => Some(std::sync::Arc::new(bytes)),
            Ok(_) => {
                log::warn!("font {name} is not a font this program can read; using the built-in one");
                None
            }
            Err(e) => {
                log::warn!("font {name}: {e}; using the built-in one");
                None
            }
        };
        self.read.insert(name.to_owned(), bytes.clone());
        bytes
    }

    /// Forgets what was read (another project, or a font file was replaced).
    pub fn clear(&mut self) {
        self.read.clear();
    }
}

/// Draws `text` (lines separated by `\n`, centred) with letters about
/// `px` pixels high, in `font` (a TrueType or OpenType file) or the
/// built-in font. Empty text gives a 1x1 transparent image.
pub fn rasterize(text: &str, px: f32, color: [u8; 4], font_file: Option<&[u8]>) -> TextImage {
    let empty = || TextImage { width: 1, height: 1, rgba: vec![0; 4] };
    let chosen = font_file.and_then(|bytes| FontRef::try_from_slice(bytes).ok());
    let font = match chosen {
        Some(font) => font,
        None => match FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT) {
            Ok(font) => font,
            Err(_) => return empty(),
        },
    };
    let px = px.clamp(4.0, 2000.0);
    let scaled = font.as_scaled(PxScale::from(px));
    let line_height = (scaled.ascent() - scaled.descent() + scaled.line_gap()).ceil();
    let margin = (px * 0.1).ceil();

    // Lay the glyphs out line by line.
    let mut lines: Vec<(f32, Vec<ab_glyph::Glyph>)> = Vec::new();
    for (row, line) in text.lines().enumerate() {
        let baseline = margin + scaled.ascent() + row as f32 * line_height;
        let mut x = 0.0f32;
        let mut previous = None;
        let mut glyphs = Vec::new();
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(prev) = previous {
                x += scaled.kern(prev, id);
            }
            glyphs.push(id.with_scale_and_position(PxScale::from(px), point(x, baseline)));
            x += scaled.h_advance(id);
            previous = Some(id);
        }
        lines.push((x, glyphs));
    }
    let widest = lines.iter().map(|(w, _)| *w).fold(0.0f32, f32::max);
    if lines.is_empty() || widest <= 0.0 {
        return empty();
    }
    let width = (widest + 2.0 * margin).ceil() as u32;
    let height = (lines.len() as f32 * line_height + 2.0 * margin).ceil() as u32;
    // Keep textures within what any GPU takes.
    if width > 8192 || height > 8192 {
        return rasterize(text, px * 0.5, color, font_file);
    }
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for (line_width, glyphs) in lines {
        let offset = margin + (widest - line_width) / 2.0;
        for mut glyph in glyphs {
            glyph.position.x += offset;
            let Some(outline) = font.outline_glyph(glyph) else { continue };
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                let (x, y) = (bounds.min.x as i32 + gx as i32, bounds.min.y as i32 + gy as i32);
                if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
                    return;
                }
                let i = ((y as u32 * width + x as u32) * 4) as usize;
                let alpha = (coverage.clamp(0.0, 1.0) * color[3] as f32) as u8;
                // Glyphs of one line can touch: keep the stronger coverage.
                if alpha > rgba[i + 3] {
                    rgba[i..i + 4].copy_from_slice(&[color[0], color[1], color[2], alpha]);
                }
            });
        }
    }
    TextImage { width, height, rgba }
}

/// Puts a soft dark shadow under the letters of `image`, a little down
/// and to the right, so that a title stays readable on a bright or busy
/// picture. `px` is the letter height the image was drawn with; `strength`
/// (0 to 1) is how dark the shadow is. It fits in the margin every text
/// image has.
pub fn with_shadow(mut image: TextImage, px: f32, strength: f32) -> TextImage {
    let strength = strength.clamp(0.0, 1.0);
    let (w, h) = (image.width as usize, image.height as usize);
    if strength <= 0.0 || w < 4 || h < 4 {
        return image;
    }
    let shift = ((px * 0.035).round() as usize).max(1);
    let radius = ((px * 0.03).round() as usize).max(1);
    // The letters' coverage, moved down and to the right.
    let mut shade = vec![0f32; w * h];
    for y in shift..h {
        for x in shift..w {
            shade[y * w + x] = image.rgba[((y - shift) * w + (x - shift)) * 4 + 3] as f32 / 255.0;
        }
    }
    // Softened: a box blur along the rows and the columns, twice.
    let mut line = vec![0f32; w.max(h)];
    let mut blur = |values: &mut [f32], start: usize, step: usize, len: usize| {
        let mut sum = 0.0;
        for i in 0..len {
            line[i] = values[start + i * step];
        }
        let window = (2 * radius + 1) as f32;
        for i in 0..len + radius {
            if i < len {
                sum += line[i];
            }
            if i > 2 * radius {
                sum -= line[i - 2 * radius - 1];
            }
            if i >= radius {
                values[start + (i - radius) * step] = sum / window;
            }
        }
    };
    for _ in 0..2 {
        for y in 0..h {
            blur(&mut shade, y * w, 1, w);
        }
        for x in 0..w {
            blur(&mut shade, x, w, h);
        }
    }
    // The letters over the shadow (black), back to straight alpha.
    for (pixel, shade) in image.rgba.chunks_exact_mut(4).zip(shade) {
        let letter = pixel[3] as f32 / 255.0;
        let under = (shade * strength).clamp(0.0, 1.0) * (1.0 - letter);
        let alpha = letter + under;
        if alpha > 0.0 {
            for channel in &mut pixel[..3] {
                *channel = (*channel as f32 * letter / alpha).round() as u8;
            }
            pixel[3] = (alpha * 255.0).round() as u8;
        }
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shadow_darkens_below_and_right_of_the_letters_only() {
        let plain = rasterize("I", 100.0, [255; 4], None);
        let (w, h) = (plain.width as usize, plain.height as usize);
        let shadowed = with_shadow(rasterize("I", 100.0, [255; 4], None), 100.0, 0.8);
        assert_eq!((shadowed.width, shadowed.height), (plain.width, plain.height));
        let mut letters_kept = true;
        let (mut right, mut left) = (0u32, 0u32);
        for y in 0..h {
            for x in 0..w {
                let (a, b) = (&plain.rgba[(y * w + x) * 4..][..4], &shadowed.rgba[(y * w + x) * 4..][..4]);
                if a[3] == 255 {
                    // Where a letter is solid nothing changes.
                    letters_kept &= a == b;
                } else if a[3] == 0 && b[3] > 40 {
                    // New ink is dark, and lies to the right rather than the left.
                    assert!(b[0] < 30, "shadow pixel {b:?}");
                    if x > w / 2 { right += 1 } else { left += 1 }
                }
            }
        }
        assert!(letters_kept);
        assert!(right > 50 && right > left * 3, "{right} right, {left} left");
        // Off is off.
        assert_eq!(with_shadow(rasterize("I", 100.0, [255; 4], None), 100.0, 0.0).rgba, plain.rgba);
    }

    #[test]
    fn text_has_ink_in_its_colour_and_grows_with_size() {
        let small = rasterize("Hi", 20.0, [255, 0, 0, 255], None);
        let big = rasterize("Hi", 80.0, [255, 0, 0, 255], None);
        assert!(big.width > small.width * 3 && big.height > small.height * 3);
        let inked: Vec<&[u8]> = big.rgba.chunks_exact(4).filter(|p| p[3] > 200).collect();
        assert!(inked.len() > 100);
        assert!(inked.iter().all(|p| p[0] == 255 && p[1] == 0 && p[2] == 0));
        // The corners are margin: transparent.
        assert_eq!(big.rgba[3], 0);
    }

    #[test]
    fn lines_stack_and_empty_text_is_harmless() {
        let one = rasterize("Line", 40.0, [255; 4], None);
        let two = rasterize("Line\nLonger line", 40.0, [255; 4], None);
        assert!(two.height > one.height && two.width > one.width);
        let none = rasterize("", 40.0, [255; 4], None);
        assert_eq!((none.width, none.height), (1, 1));
        assert_eq!(rasterize("   ", 40.0, [255; 4], None).rgba.iter().filter(|b| **b != 0).count(), 0);
    }

    #[test]
    fn a_font_file_changes_the_letters_and_a_bad_one_falls_back() {
        let built_in = rasterize("Wide", 60.0, [255; 4], None);
        // Another face of the same family: the letters are heavier.
        let other = rasterize("Wide", 60.0, [255; 4], Some(epaint_default_fonts::HACK_REGULAR));
        let ink = |image: &TextImage| image.rgba.chunks_exact(4).filter(|p| p[3] > 128).count();
        assert!(other.width != built_in.width || ink(&other) != ink(&built_in));
        assert!(ink(&other) > 100);
        let fallback = rasterize("Wide", 60.0, [255; 4], Some(b"not a font"));
        assert_eq!((fallback.width, fallback.height), (built_in.width, built_in.height));
        assert_eq!(fallback.rgba, built_in.rgba);

        let dir = std::env::temp_dir().join(format!("sg-fonts-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("assets/font")).unwrap();
        std::fs::write(dir.join("assets/font/hack.ttf"), epaint_default_fonts::HACK_REGULAR).unwrap();
        std::fs::write(dir.join("assets/font/bad.ttf"), b"nothing").unwrap();
        let mut fonts = Fonts::default();
        assert!(fonts.get(&dir, "hack.ttf").is_some());
        assert!(fonts.get(&dir, "bad.ttf").is_none());
        assert!(fonts.get(&dir, "missing.ttf").is_none());
        assert!(fonts.get(&dir, "../font/hack.ttf").is_none());
        assert!(fonts.get(&dir, "").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
