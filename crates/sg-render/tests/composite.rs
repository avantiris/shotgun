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
