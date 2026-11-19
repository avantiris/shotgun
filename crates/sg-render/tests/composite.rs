//! Renders small known scenes on the real GPU and checks pixels.

#![allow(clippy::unwrap_used)]

use sg_render::{Compositor, LayerDraw};

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

fn solid(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            pixels.extend(f(x, y));
        }
    }
    pixels
}

fn pixel(out: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [out[i], out[i + 1], out[i + 2], out[i + 3]]
}

fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (*x as i32 - y as i32).abs() <= 2)
}

#[test]
fn fits_blends_and_turns() {
    let Some((device, queue)) = device() else {
        eprintln!("no Vulkan device: skipping");
        return;
    };
    let mut comp = Compositor::new(device.clone(), queue.clone(), 200, 200);

    // Nothing: black.
    comp.render(&[]);
    let out = comp.read_output().unwrap();
    assert_eq!(pixel(&out, 200, 100, 100), [0, 0, 0, 255]);

    // A 2:1 picture is letterboxed; its colour survives the linear round trip.
    let colour = [200, 90, 30, 255];
    let wide = Compositor::upload(&device, &queue, 100, 50, &solid(100, 50, |_, _| colour));
    let wide_view = wide.create_view(&Default::default());
    comp.render(&[LayerDraw::fitted(&wide_view, (100, 50), 0)]);
    let out = comp.read_output().unwrap();
    assert!(near(pixel(&out, 200, 100, 100), colour), "{:?}", pixel(&out, 200, 100, 100));
    assert_eq!(pixel(&out, 200, 100, 20), [0, 0, 0, 255], "bar above");
    assert_eq!(pixel(&out, 200, 100, 180), [0, 0, 0, 255], "bar below");
    assert!(near(pixel(&out, 200, 5, 60), colour) && near(pixel(&out, 200, 195, 140), colour));

    // Half-transparent white over it: blended in linear light, so the result
    // is brighter than the average of the encoded values.
    let white = Compositor::upload(&device, &queue, 4, 4, &solid(4, 4, |_, _| [255, 255, 255, 255]));
    let white_view = white.create_view(&Default::default());
    let black = Compositor::upload(&device, &queue, 4, 4, &solid(4, 4, |_, _| [0, 0, 0, 255]));
    let black_view = black.create_view(&Default::default());
    comp.render(&[
        LayerDraw::fitted(&black_view, (4, 4), 0),
        LayerDraw { opacity: 0.5, ..LayerDraw::fitted(&white_view, (4, 4), 0) },
    ]);
    let grey = pixel(&comp.read_output().unwrap(), 200, 100, 100);
    assert!((186..=189).contains(&grey[0]), "50 % white over black is {} in sRGB", grey[0]);

    // Straight alpha in the picture is honoured.
    let half = Compositor::upload(&device, &queue, 4, 4, &solid(4, 4, |_, _| [255, 255, 255, 0]));
    let half_view = half.create_view(&Default::default());
    comp.render(&[LayerDraw::fitted(&wide_view, (200, 200), 0), LayerDraw::fitted(&half_view, (4, 4), 0)]);
    assert!(near(pixel(&comp.read_output().unwrap(), 200, 100, 100), colour));

    // Rotation: a picture that is red on its left half and blue on its right.
    let two = Compositor::upload(
        &device,
        &queue,
        8,
        8,
        &solid(8, 8, |x, _| if x < 4 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }),
    );
    let two_view = two.create_view(&Default::default());
    let mut at = |rotation: i32, x: u32, y: u32| {
        comp.render(&[LayerDraw::fitted(&two_view, (8, 8), rotation)]);
        pixel(&comp.read_output().unwrap(), 200, x, y)
    };
    let (red, blue) = ([255, 0, 0, 255], [0, 0, 255, 255]);
    assert_eq!((at(0, 50, 100), at(0, 150, 100)), (red, blue));
    assert_eq!((at(180, 50, 100), at(180, 150, 100)), (blue, red));
    // Turned 90 degrees counter-clockwise the left half ends up at the bottom.
    assert_eq!((at(90, 100, 150), at(90, 100, 50)), (red, blue));
    assert_eq!((at(270, 100, 50), at(270, 100, 150)), (red, blue));
}

#[test]
fn resize_changes_the_output() {
    let Some((device, queue)) = device() else { return };
    let mut comp = Compositor::new(device, queue, 64, 64);
    comp.resize(320, 180);
    assert_eq!(comp.size(), (320, 180));
    comp.render(&[]);
    assert_eq!(comp.read_output().unwrap().len(), 320 * 180 * 4);
}

#[test]
fn placement_scale_crop_and_turn() {
    let Some((device, queue)) = device() else { return };
    let mut comp = Compositor::new(device.clone(), queue.clone(), 200, 100);
    // Left half red, right half blue; fills the 2:1 frame exactly.
    let two = Compositor::upload(
        &device,
        &queue,
        8,
        4,
        &solid(8, 4, |x, _| if x < 4 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }),
    );
    let view = two.create_view(&Default::default());
    let base = LayerDraw::fitted(&view, (8, 4), 0);
    let (red, blue, black) = ([255, 0, 0, 255], [0, 0, 255, 255], [0, 0, 0, 255]);
    let mut shot = |layer: LayerDraw<'_>| {
        comp.render(&[layer]);
        comp.read_output().unwrap()
    };

    // Half size: a 100x50 picture in the middle.
    let out = shot(LayerDraw { scale: 0.5, ..base });
    assert_eq!((pixel(&out, 200, 60, 50), pixel(&out, 200, 140, 50)), (red, blue));
    assert_eq!((pixel(&out, 200, 40, 50), pixel(&out, 200, 100, 15)), (black, black));

    // Moved a quarter frame to the right and down.
    let out = shot(LayerDraw { scale: 0.5, x: 0.25, y: 0.25, ..base });
    assert_eq!((pixel(&out, 200, 110, 75), pixel(&out, 200, 190, 75)), (red, blue));
    assert_eq!(pixel(&out, 200, 60, 30), black);

    // Cropping the left quarter away: what is left sits where it was.
    let out = shot(LayerDraw { crop: [0.25, 0.0, 0.0, 0.0], ..base });
    assert_eq!((pixel(&out, 200, 25, 50), pixel(&out, 200, 75, 50), pixel(&out, 200, 150, 50)), (black, red, blue));

    // A quarter turn clockwise: the left (red) half points up.
    let out = shot(LayerDraw { turn_degrees: 90.0, scale: 0.5, ..base });
    assert_eq!((pixel(&out, 200, 100, 30), pixel(&out, 200, 100, 70)), (red, blue));

    // Natural size: an 8x4 texture stays 8x4 pixels.
    let out = shot(LayerDraw { natural_size: true, ..base });
    assert_eq!((pixel(&out, 200, 98, 50), pixel(&out, 200, 102, 50), pixel(&out, 200, 90, 50)), (red, blue, black));
}

#[test]
fn nv12_delivery_matches_bt709() {
    let Some((device, queue)) = device() else { return };
    // 1920 is not a multiple of the row alignment: exercises the padding.
    for (w, h) in [(64u32, 36u32), (1920, 8)] {
        let mut comp = Compositor::new(device.clone(), queue.clone(), w, h);
        let cases = [
            ([255u8, 255, 255, 255], (235, 128, 128)),
            ([0, 0, 0, 255], (16, 128, 128)),
            ([255, 0, 0, 255], (63, 102, 240)),
            ([0, 0, 255, 255], (32, 240, 118)),
            ([128, 128, 128, 255], (126, 128, 128)),
        ];
        for (rgba, (y, cb, cr)) in cases {
            let tex = Compositor::upload(&device, &queue, 2, 2, &solid(2, 2, |_, _| rgba));
            let view = tex.create_view(&Default::default());
            comp.render(&[LayerDraw::fitted(&view, (w, h), 0)]);
            let nv12 = comp.read_output_nv12().unwrap();
            assert_eq!(nv12.len(), (w * h * 3 / 2) as usize);
            let luma = nv12[(h / 2 * w + w / 2) as usize] as i32;
            let chroma = (w * h + (h / 4) * w + (w / 4) * 2) as usize;
            let (u, v) = (nv12[chroma] as i32, nv12[chroma + 1] as i32);
            assert!(
                (luma - y).abs() <= 1 && (u - cb).abs() <= 1 && (v - cr).abs() <= 1,
                "{rgba:?} at {w}x{h}: ({luma}, {u}, {v}) instead of ({y}, {cb}, {cr})"
            );
        }
    }
}

#[test]
fn nv12_readback_is_complete_for_every_frame() {
    let Some((device, queue)) = device() else { return };
    let mut comp = Compositor::new(device.clone(), queue.clone(), 1920, 1080);
    let tex = Compositor::upload(&device, &queue, 2, 2, &solid(2, 2, |_, _| [200, 100, 50, 255]));
    let view = tex.create_view(&Default::default());
    for frame in 0..40 {
        comp.render(&[LayerDraw::fitted(&view, (1920, 1080), 0)]);
        let nv12 = comp.read_output_nv12().unwrap();
        assert_eq!(nv12.len(), 1920 * 1080 * 3 / 2, "frame {frame}");
    }
}

/// Left half red, right half blue, 64x36.
fn two_halves(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let pixels = solid(64, 36, |x, _| if x < 32 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
    Compositor::upload(device, queue, 64, 36, &pixels).create_view(&Default::default())
}

#[test]
fn a_whipped_picture_moves_mirrored_and_blurs_along_its_way() {
    use sg_core::Shift;
    use sg_render::Motion;
    let Some((device, queue)) = device() else { return };
    let view = two_halves(&device, &queue);
    let mut comp = Compositor::new(device.clone(), queue.clone(), 64, 36);
    let (red, blue) = ([255, 0, 0, 255], [0, 0, 255, 255]);
    let moved = |x: f32| Shift { x, ..Shift::REST };
    let shot = |comp: &mut Compositor, motion: Motion| {
        comp.render(&[LayerDraw { motion: Some(motion), ..LayerDraw::fitted(&view, (64, 36), 0) }]);
        comp.read_output().unwrap()
    };
    // At rest inside a transition: the picture as it is.
    let out = shot(&mut comp, Motion { from: Shift::REST, to: Shift::REST, mirror: true });
    assert!(near(pixel(&out, 64, 8, 18), red) && near(pixel(&out, 64, 56, 18), blue));
    // A quarter of the width to the left, no blur: the border is at 16, and
    // what comes in at the right is the picture's mirror image, not black.
    let out = shot(&mut comp, Motion { from: moved(-0.25), to: moved(-0.25), mirror: true });
    assert!(near(pixel(&out, 64, 8, 18), red), "{:?}", pixel(&out, 64, 8, 18));
    assert!(near(pixel(&out, 64, 24, 18), blue), "{:?}", pixel(&out, 64, 24, 18));
    assert!(near(pixel(&out, 64, 60, 18), blue), "{:?}", pixel(&out, 64, 60, 18));
    // One and a quarter widths: past the mirror image, into the next copy
    // (the border of the mirrored picture is where red returns).
    let out = shot(&mut comp, Motion { from: moved(-1.25), to: moved(-1.25), mirror: true });
    assert!(near(pixel(&out, 64, 8, 18), blue) && near(pixel(&out, 64, 40, 18), red), "{:?}", pixel(&out, 64, 40, 18));
    // Moving half a width while the shutter is open: a pixel that sees red
    // first and blue later is a mix of both; one that only ever sees blue stays blue.
    let out = shot(&mut comp, Motion { from: Shift::REST, to: moved(-0.5), mirror: true });
    let mixed = pixel(&out, 64, 10, 18);
    assert!(mixed[0] > 100 && mixed[2] > 60 && mixed[0] < 250, "{mixed:?}");
    assert!(near(pixel(&out, 64, 44, 18), blue), "{:?}", pixel(&out, 64, 44, 18));
    // A slide moves the picture as a whole: what it leaves behind is empty.
    let out = shot(&mut comp, Motion { from: moved(-0.5), to: moved(-0.5), mirror: false });
    assert!(near(pixel(&out, 64, 8, 18), blue) && near(pixel(&out, 64, 48, 18), [0, 0, 0, 255]));
    // Zooming in on the centre: the border stays in the middle, the picture
    // still fills the frame.
    let zoomed = Shift { zoom: 2.0, ..Shift::REST };
    let out = shot(&mut comp, Motion { from: zoomed, to: zoomed, mirror: true });
    assert!(near(pixel(&out, 64, 2, 2), red) && near(pixel(&out, 64, 61, 33), blue));
}

fn srgb_to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(v: f32) -> u8 {
    let x = v.clamp(0.0, 1.0);
    let c = if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0).round() as u8
}

#[test]
fn the_shader_matches_and_grades_as_the_model_says() {
    use sg_core::{Correction, Grade, Module, ModuleKind};
    let Some((device, queue)) = device() else { return };
    let mut comp = Compositor::new(device.clone(), queue.clone(), 64, 36);
    let source = [180u8, 120, 70, 255];
    let tex = Compositor::upload(&device, &queue, 2, 2, &solid(2, 2, |_, _| source));
    let view = tex.create_view(&Default::default());
    let linear = [srgb_to_linear(source[0]), srgb_to_linear(source[1]), srgb_to_linear(source[2])];
    let mut shot = |grade: &Grade, seed: f32, at: (u32, u32)| {
        comp.render(&[LayerDraw { grade: Some(grade), seed, ..LayerDraw::fitted(&view, (64, 36), 0) }]);
        pixel(&comp.read_output().unwrap(), 64, at.0, at.1)
    };
    let expect = |c: [f32; 3]| [linear_to_srgb(c[0]), linear_to_srgb(c[1]), linear_to_srgb(c[2]), 255];
    let plain = Grade { correction: Correction::NONE, look: vec![], lut: None };
    assert!(near(shot(&plain, 0.0, (32, 18)), source));

    // The match stage: the same numbers as the model's own arithmetic.
    for correction in [
        Correction { gain: [1.4, 1.1, 0.8], contrast: 1.0, pivot: 0.18, saturation: 1.0 },
        Correction { gain: [1.0; 3], contrast: 1.4, pivot: 0.25, saturation: 0.6 },
        Correction { gain: [0.7, 0.9, 1.3], contrast: 0.75, pivot: 0.12, saturation: 1.5 },
    ] {
        let got = shot(&Grade { correction, look: vec![], lut: None }, 0.0, (32, 18));
        assert!(
            near(got, expect(correction.apply(linear))),
            "{correction:?}: {got:?} instead of {:?}",
            expect(correction.apply(linear))
        );
    }

    let with = |module: Module| Grade { correction: Correction::NONE, look: vec![module], lut: None };
    let knobs = |kind: ModuleKind, values: [f32; 4]| Module { knobs: values, ..Module::new(kind) };
    // Exposure: one stop less halves the light.
    let got = shot(&with(knobs(ModuleKind::Balance, [0.0, 0.0, -1.0, 0.0])), 0.0, (32, 18));
    assert!(near(got, expect(linear.map(|v| v / 2.0))), "{got:?}");
    // Warmer: more red, less blue.
    let warm = shot(&with(knobs(ModuleKind::Balance, [0.8, 0.0, 0.0, 0.0])), 0.0, (32, 18));
    assert!(warm[0] > source[0] + 5 && warm[2] + 5 < source[2], "{warm:?}");
    // No saturation: grey of the same brightness.
    let grey = shot(&with(knobs(ModuleKind::Colour, [0.0, 0.0, 0.0, 0.0])), 0.0, (32, 18));
    let lum = 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
    assert!(near(grey, expect([lum; 3])), "{grey:?}");
    // Half mixed: half the way; switched to nothing: nothing.
    let half = shot(&with(Module { mix: 0.5, ..knobs(ModuleKind::Colour, [0.0, 0.0, 0.0, 0.0]) }), 0.0, (32, 18));
    assert!(near(half, expect([0, 1, 2].map(|k| (linear[k] + lum) / 2.0))), "{half:?}");
    // More contrast pushes this brighter-than-pivot colour up, less pulls it down.
    let harder = shot(&with(knobs(ModuleKind::Tone, [0.6, 0.0, 0.0, 0.18])), 0.0, (32, 18));
    let softer = shot(&with(knobs(ModuleKind::Tone, [-0.6, 0.0, 0.0, 0.18])), 0.0, (32, 18));
    assert!(harder[1] > source[1] + 4 && softer[1] + 4 < source[1], "{harder:?} {softer:?}");
    // A vignette darkens the corner and leaves the centre.
    let vignette = with(knobs(ModuleKind::Vignette, [0.8, 0.6, 0.3, 0.0]));
    assert!(near(shot(&vignette, 0.0, (32, 18)), source));
    assert!(shot(&vignette, 0.0, (1, 1))[0] + 40 < source[0]);
    // Split tone at full amount colours the picture towards the highlight hue (orange).
    let toned = shot(&with(knobs(ModuleKind::SplitTone, [220.0, 40.0, 1.0, -1.0])), 0.0, (32, 18));
    assert!(toned[0] > source[0] && toned[2] < source[2], "{toned:?}");
    // Grain differs from pixel to pixel and from frame to frame, around the picture's own colour.
    let grain = with(knobs(ModuleKind::Grain, [1.0, 1.0, 0.0, 0.0]));
    let (a, b, c) = (shot(&grain, 1.0, (10, 10)), shot(&grain, 1.0, (11, 10)), shot(&grain, 2.0, (10, 10)));
    assert!(a != b && a != c, "{a:?} {b:?} {c:?}");
    assert!((a[1] as i32 - source[1] as i32).abs() < 60);
    // The chain runs in order: darken then desaturate towards... is not the
    // same as contrast before exposure.
    let first = Grade {
        correction: Correction::NONE,
        look: vec![knobs(ModuleKind::Balance, [0.0, 0.0, 1.0, 0.0]), knobs(ModuleKind::Tone, [0.8, 0.0, 0.0, 0.18])],
        lut: None,
    };
    let second = Grade { correction: Correction::NONE, look: first.look.iter().rev().copied().collect(), lut: None };
    assert!(!near(shot(&first, 0.0, (32, 18)), shot(&second, 0.0, (32, 18))));
}

#[test]
fn p010_delivery_has_ten_real_bits() {
    let Some((device, queue)) = device() else { return };
    let (w, h) = (1024u32, 4u32);
    let mut comp = Compositor::new(device.clone(), queue.clone(), w, h);
    let sample =
        |bytes: &[u8], index: usize| (u16::from_le_bytes([bytes[index * 2], bytes[index * 2 + 1]]) >> 6) as i32;
    // Flat colours land on the standard's code values.
    for (rgba, (y, cb, cr)) in [
        ([255u8, 255, 255, 255], (940, 512, 512)),
        ([0, 0, 0, 255], (64, 512, 512)),
        ([255, 0, 0, 255], (250, 409, 960)),
    ] {
        let tex = Compositor::upload(&device, &queue, 2, 2, &solid(2, 2, |_, _| rgba));
        let view = tex.create_view(&Default::default());
        comp.render(&[LayerDraw::fitted(&view, (w, h), 0)]);
        let bytes = comp.read_output_p010().unwrap();
        assert_eq!(bytes.len(), (w * h * 3) as usize);
        let chroma = (w * h) as usize;
        let got = (sample(&bytes, 100), sample(&bytes, chroma + 20), sample(&bytes, chroma + 21));
        assert!((got.0 - y).abs() <= 1 && (got.1 - cb).abs() <= 1 && (got.2 - cr).abs() <= 1, "{rgba:?}: {got:?}");
    }
    // A ramp that is one 8-bit step from end to end, stretched over the
    // whole width (as a soft vignette does): 8 bits can show two levels of
    // it, 10 bits several in between.
    let pixels = solid(2, 1, |x, _| if x == 0 { [100, 100, 100, 255] } else { [101, 101, 101, 255] });
    let tex = Compositor::upload(&device, &queue, 2, 1, &pixels);
    let view = tex.create_view(&Default::default());
    comp.render(&[LayerDraw::fitted(&view, (w, h), 0)]);
    let ten = comp.read_output_p010().unwrap();
    let eight = comp.read_output_nv12().unwrap();
    let levels = |values: Vec<i32>| {
        let mut v = values;
        v.sort_unstable();
        v.dedup();
        v.len()
    };
    let ten_levels = levels((0..w as usize).map(|x| sample(&ten, x)).collect());
    let eight_levels = levels(eight[..w as usize].iter().map(|v| *v as i32).collect());
    assert!(eight_levels <= 2 && ten_levels >= 4, "{eight_levels} levels in 8 bits, {ten_levels} in 10");
}

#[test]
fn sharpen_and_glow_look_at_the_surroundings() {
    use sg_core::{Correction, Grade, Module, ModuleKind};
    let Some((device, queue)) = device() else { return };
    let mut comp = Compositor::new(device.clone(), queue.clone(), 64, 36);
    // Dark left half, mid-grey right half.
    let pixels = solid(64, 36, |x, _| if x < 32 { [60, 60, 60, 255] } else { [150, 150, 150, 255] });
    let view = Compositor::upload(&device, &queue, 64, 36, &pixels).create_view(&Default::default());
    let mut shot = |module: Option<Module>| {
        let grade = Grade { correction: Correction::NONE, look: module.into_iter().collect(), lut: None };
        comp.render(&[LayerDraw { grade: Some(&grade), ..LayerDraw::fitted(&view, (64, 36), 0) }]);
        comp.read_output().unwrap()
    };
    let plain = shot(None);
    let knobs = |kind: ModuleKind, values: [f32; 4]| Module { knobs: values, ..Module::new(kind) };
    // Sharpening makes the dark side of the edge darker and the bright side
    // brighter, and leaves flat areas alone.
    let sharp = shot(Some(knobs(ModuleKind::Sharpen, [1.5, 6.0, 0.0, 0.0])));
    assert!(pixel(&sharp, 64, 31, 18)[0] + 3 < pixel(&plain, 64, 31, 18)[0], "{:?}", pixel(&sharp, 64, 31, 18));
    assert!(pixel(&sharp, 64, 32, 18)[0] > pixel(&plain, 64, 32, 18)[0] + 3, "{:?}", pixel(&sharp, 64, 32, 18));
    assert!(
        near(pixel(&sharp, 64, 8, 18), pixel(&plain, 64, 8, 18))
            && near(pixel(&sharp, 64, 56, 18), pixel(&plain, 64, 56, 18))
    );
    // Softening does the opposite at the edge.
    let soft = shot(Some(knobs(ModuleKind::Sharpen, [-1.0, 6.0, 0.0, 0.0])));
    assert!(pixel(&soft, 64, 31, 18)[0] > pixel(&plain, 64, 31, 18)[0] + 3);
    // Glow: the bright half (above the threshold) spills light onto the
    // dark half near the edge, not far from it; warm glow is redder than blue.
    let glow = shot(Some(knobs(ModuleKind::Glow, [0.1, 8.0, 2.0, 1.0])));
    let (near_edge, far) = (pixel(&glow, 64, 30, 18), pixel(&glow, 64, 4, 18));
    assert!(near_edge[0] > pixel(&plain, 64, 30, 18)[0] + 4 && near_edge[0] > near_edge[2], "{near_edge:?}");
    assert!(near(far, pixel(&plain, 64, 4, 18)), "{far:?}");
}

#[test]
fn a_look_up_table_is_applied_where_its_module_sits() {
    use sg_core::{Correction, Grade, Lut, Module, ModuleKind};
    let Some((device, queue)) = device() else { return };
    let mut comp = Compositor::new(device.clone(), queue.clone(), 16, 16);
    let source = [200u8, 120, 60, 255];
    let view = Compositor::upload(&device, &queue, 2, 2, &solid(2, 2, |_, _| source)).create_view(&Default::default());
    let grade = |lut: Option<&str>| Grade {
        correction: Correction::NONE,
        look: vec![Module::new(ModuleKind::Lut)],
        lut: lut.map(str::to_owned),
    };
    let shot = |comp: &mut Compositor, grade: &Grade| {
        comp.render(&[LayerDraw { grade: Some(grade), ..LayerDraw::fitted(&view, (16, 16), 0) }]);
        pixel(&comp.read_output().unwrap(), 16, 8, 8)
    };
    // No table, or one that was never handed over: the picture as it is.
    assert!(near(shot(&mut comp, &grade(None)), source));
    assert!(near(shot(&mut comp, &grade(Some("missing.cube"))), source));
    // The neutral table of any size changes nothing.
    comp.set_lut("neutral.cube", &Lut::identity(17));
    assert!(
        near(shot(&mut comp, &grade(Some("neutral.cube"))), source),
        "{:?}",
        shot(&mut comp, &grade(Some("neutral.cube")))
    );
    // A table that swaps red and blue and halves green (in display values).
    let mut swap = Lut::identity(17);
    swap.data.iter_mut().for_each(|c| *c = [c[2], c[1] / 2.0, c[0]]);
    comp.set_lut("swap.cube", &swap);
    let got = shot(&mut comp, &grade(Some("swap.cube")));
    assert!(near(got, [60, 60, 200, 255]), "{got:?}");
    // At half mix: half the way, in linear light.
    let half = Grade { look: vec![Module { mix: 0.5, ..Module::new(ModuleKind::Lut) }], ..grade(Some("swap.cube")) };
    let mixed = shot(&mut comp, &half);
    assert!(mixed[0] < 200 && mixed[0] > 60 && mixed[2] > 60 && mixed[2] < 200, "{mixed:?}");
}
