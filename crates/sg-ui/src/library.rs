//! The library screen: a grid of projects with type-to-filter.

use std::path::PathBuf;

use egui::{Align2, Color32, FontId, Key, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use sg_project::ProjectEntry;

use crate::theme;

const TILE: egui::Vec2 = vec2(220.0, 172.0);
const POSTER_H: f32 = 124.0;
const GAP: f32 = 16.0;

pub enum LibraryRequest {
    Open(PathBuf),
    Create(String),
}

#[derive(Default)]
pub struct Library {
    entries: Vec<ProjectEntry>,
    loaded: bool,
    pub filter: String,
    selected: usize,
    /// Shown instead of accepting input while a create/open job runs.
    pub pending: Option<String>,
    /// The projects' pictures: asked for when a tile is first shown;
    /// `None` while unknown or if the project has none.
    posters: std::collections::HashMap<PathBuf, Option<egui::TextureHandle>>,
    /// Projects whose picture should be read; taken by the shell.
    pub poster_requests: Vec<PathBuf>,
}

impl Library {
    pub fn set_entries(&mut self, entries: Vec<ProjectEntry>) {
        self.entries = entries;
        self.loaded = true;
        self.selected = 0;
        // A project that was just closed may have a new picture.
        self.posters.clear();
    }

    /// The picture of the project at `path` was read (or it has none).
    pub fn set_poster(&mut self, ctx: &egui::Context, path: PathBuf, rgba: Option<Vec<u8>>) {
        let size = crate::assets::POSTER_SIZE;
        let texture = rgba.filter(|b| b.len() == size[0] * size[1] * 4).map(|bytes| {
            let image = egui::ColorImage::from_rgba_unmultiplied(size, &bytes);
            ctx.load_texture(format!("project-{}", path.display()), image, egui::TextureOptions::LINEAR)
        });
        self.posters.insert(path, texture);
    }

    pub fn loaded(&self) -> bool {
        self.loaded
    }

    pub fn find_slug(&self, slug: &str) -> Option<&ProjectEntry> {
        self.entries.iter().find(|e| e.slug == slug)
    }

    /// Indices of the entries matching the filter, in display order.
    fn matches(&self) -> Vec<usize> {
        let needle = self.filter.trim().to_lowercase();
        (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                needle.is_empty() || e.name.to_lowercase().contains(&needle) || e.slug.contains(&needle)
            })
            .collect()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, now_unix: u64) -> Option<LibraryRequest> {
        let frame = egui::Frame::new().fill(theme::BG).inner_margin(28.0);
        egui::CentralPanel::default().frame(frame).show(ui, |ui| self.contents(ui, now_unix)).inner
    }

    fn contents(&mut self, ui: &mut egui::Ui, now_unix: u64) -> Option<LibraryRequest> {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("SHOTGUN").size(13.0).strong().color(theme::ACCENT));
            ui.add_space(12.0);
            let field = egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Search, or type a name for a new project")
                .font(FontId::proportional(18.0))
                .desired_width(380.0);
            let response = ui.add(field);
            // The filter is the only text field here: typing always lands in it.
            if !response.has_focus() {
                response.request_focus();
            }
            if response.changed() {
                self.selected = 0;
            }
            ui.add_space(12.0);
            let hint = self.pending.as_deref().unwrap_or("Enter: open    Ctrl+N: new project    Arrows: select");
            ui.label(egui::RichText::new(hint).color(theme::DIM));
        });
        ui.add_space(20.0);

        let matches = self.matches();
        // One extra tile at the end creates a project.
        let tiles = matches.len() + 1;
        self.selected = self.selected.min(tiles - 1);
        let columns = (((ui.available_width() + GAP) / (TILE.x + GAP)).floor() as usize).max(1);
        let accepts_input = self.pending.is_none();
        let mut activated = None;
        if accepts_input {
            let step = ui.input(|i| {
                let pressed = |key| i.key_pressed(key) as isize;
                (pressed(Key::ArrowRight) - pressed(Key::ArrowLeft))
                    + columns as isize * (pressed(Key::ArrowDown) - pressed(Key::ArrowUp))
            });
            self.selected = (self.selected as isize + step).clamp(0, tiles as isize - 1) as usize;
            if ui.input(|i| i.key_pressed(Key::Enter)) {
                activated = Some(self.selected);
            }
        }

        if !self.loaded {
            ui.label(egui::RichText::new("Reading library...").color(theme::DIM));
            return None;
        }
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            // The scroll area clips at its edge: keep the tiles clear of it.
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for (slot, &index) in matches.iter().enumerate() {
                    let entry = &self.entries[index];
                    let (subtitle, colour) = match &entry.error {
                        Some(error) => (error.clone(), theme::ERROR),
                        None => (opened_ago(entry.last_opened, now_unix), theme::DIM),
                    };
                    let poster = poster_colour(entry.id.map_or(0, |id| id.0 as u64));
                    if entry.error.is_none() && !self.posters.contains_key(&entry.path) {
                        self.posters.insert(entry.path.clone(), None);
                        self.poster_requests.push(entry.path.clone());
                    }
                    let picture = self.posters.get(&entry.path).and_then(Option::as_ref);
                    let response = tile(
                        ui,
                        slot == self.selected,
                        poster,
                        picture,
                        &initials(&entry.name),
                        &entry.name,
                        &subtitle,
                        colour,
                    );
                    if response.clicked() && accepts_input {
                        activated = Some(slot);
                    }
                }
                let name = self.filter.trim();
                let title = if name.is_empty() { "New project".to_owned() } else { format!("New: {name}") };
                let response =
                    tile(ui, self.selected == tiles - 1, theme::RAISED, None, "+", &title, "Ctrl+N", theme::DIM);
                if response.clicked() && accepts_input {
                    activated = Some(tiles - 1);
                }
            });
        });

        let slot = activated?;
        match matches.get(slot) {
            Some(&index) => {
                let entry = &self.entries[index];
                // A project whose project.toml does not parse cannot be opened.
                entry.error.is_none().then(|| LibraryRequest::Open(entry.path.clone()))
            }
            None => Some(LibraryRequest::Create(self.filter.trim().to_owned())),
        }
    }
}

fn tile(
    ui: &mut egui::Ui,
    selected: bool,
    poster: Color32,
    picture: Option<&egui::TextureHandle>,
    glyph: &str,
    title: &str,
    subtitle: &str,
    subtitle_colour: Color32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(TILE, Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter().with_clip_rect(rect.expand(3.0));
        let poster_rect = Rect::from_min_size(rect.min, vec2(TILE.x, POSTER_H));
        painter.rect_filled(poster_rect, theme::RADIUS, poster);
        match picture {
            // A frame of the project, as wide as the tile (both are 16:9 or nearly).
            Some(texture) => {
                egui::Image::new(texture).corner_radius(theme::RADIUS).paint_at(ui, poster_rect);
            }
            None => {
                painter.text(
                    poster_rect.center(),
                    Align2::CENTER_CENTER,
                    glyph,
                    FontId::proportional(36.0),
                    Color32::from_white_alpha(210),
                );
            }
        }
        let text = ui.painter().with_clip_rect(rect);
        text.text(
            pos2(rect.left() + 2.0, poster_rect.bottom() + 8.0),
            Align2::LEFT_TOP,
            title,
            FontId::proportional(14.0),
            theme::TEXT,
        );
        text.text(
            pos2(rect.left() + 2.0, poster_rect.bottom() + 28.0),
            Align2::LEFT_TOP,
            subtitle,
            FontId::proportional(11.0),
            subtitle_colour,
        );
        let stroke = if selected {
            Some(Stroke::new(2.0, theme::ACCENT))
        } else if response.hovered() {
            Some(Stroke::new(1.0, theme::DIM))
        } else {
            None
        };
        if let Some(stroke) = stroke {
            painter.rect_stroke(poster_rect, theme::RADIUS, stroke, StrokeKind::Inside);
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A stable, muted colour per project, shown until its picture is read and for projects without one.
fn poster_colour(seed: u64) -> Color32 {
    let hue = (seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) as f32 / (1u64 << 24) as f32;
    egui::ecolor::Hsva::new(hue, 0.6, 0.1, 1.0).into()
}

fn initials(name: &str) -> String {
    name.split_whitespace().filter_map(|w| w.chars().next()).take(2).flat_map(char::to_uppercase).collect()
}

fn opened_ago(last_opened: u64, now: u64) -> String {
    if last_opened == 0 {
        return "never opened".to_owned();
    }
    let secs = now.saturating_sub(last_opened);
    match secs {
        0..60 => "opened just now".to_owned(),
        60..3600 => format!("opened {} min ago", secs / 60),
        3600..86_400 => format!("opened {} h ago", secs / 3600),
        _ => format!("opened {} d ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, slug: &str) -> ProjectEntry {
        ProjectEntry {
            path: PathBuf::from(slug),
            slug: slug.into(),
            name: name.into(),
            id: None,
            last_opened: 0,
            error: None,
        }
    }

    #[test]
    fn filter_matches_name_or_folder_ignoring_case() {
        let mut lib = Library::default();
        lib.set_entries(vec![entry("Züri Fäscht", "zueri-faescht"), entry("Meme 12", "meme-12")]);
        assert_eq!(lib.matches(), [0, 1]);
        lib.filter = "ZÜRI".into();
        assert_eq!(lib.matches(), [0]);
        lib.filter = "faescht".into();
        assert_eq!(lib.matches(), [0]);
        lib.filter = "nothing".into();
        assert!(lib.matches().is_empty());
    }

    #[test]
    fn labels() {
        assert_eq!(initials("my first video"), "MF");
        assert_eq!(initials("x"), "X");
        assert_eq!(opened_ago(0, 100), "never opened");
        assert_eq!(opened_ago(100, 130), "opened just now");
        assert_eq!(opened_ago(100, 100 + 7200), "opened 2 h ago");
        assert_eq!(opened_ago(100, 100 + 3 * 86_400), "opened 3 d ago");
    }
}
