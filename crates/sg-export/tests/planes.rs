//! The GPU conversion of decoded planes must give the picture the CPU
//! conversion gives: preview shows the one, export encodes the other.

#![allow(clippy::unwrap_used)]

use std::process::Command;

use sg_media::decode::VideoDecoder;
use sg_render::{Compositor, LayerDraw, YuvImporter, YuvPlanes};

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

#[test]
fn gpu_and_cpu_conversion_agree() {
    let Some((device, queue)) = device() else {
        eprintln!("no Vulkan device: skipping");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    // (size, extra encoder arguments): HD video range, SD (BT.601 by size), full range.
    let cases: [(&str, &[&str]); 3] = [
        ("1280x720", &["-pix_fmt", "yuv420p", "-colorspace", "bt709", "-color_range", "tv"]),
        ("320x180", &["-pix_fmt", "yuv420p"]),
        ("1280x720", &["-pix_fmt", "yuvj420p", "-colorspace", "bt709", "-color_range", "pc"]),
    ];
    for (n, (size, args)) in cases.iter().enumerate() {
        let file = dir.path().join(format!("{n}.mp4"));
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={size}:r=30:d=1")])
            .args(["-c:v", "libx264", "-crf", "12"])
            .args(*args)
            .arg(&file)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            eprintln!("ffmpeg cannot make the test clip: skipping");
            return;
        }
        let cpu = VideoDecoder::open(&file, 1).unwrap().frame(10).unwrap();
        let mut decoder = VideoDecoder::open(&file, 1).unwrap();
        decoder.set_planes(true);
        let gpu = decoder.frame(10).unwrap();
        let planes = gpu.planes.as_ref().expect("4:2:0 comes as planes");
        assert!(gpu.rgba.is_empty());
        let (kr, kb) = planes.matrix.kr_kb();
        let importer = YuvImporter::new(device.clone(), queue.clone());
        let imported = importer
            .import(&YuvPlanes {
                size: (gpu.width, gpu.height),
                chroma_size: planes.chroma_size,
                y: &planes.y,
                u: &planes.u,
                v: &planes.v,
                kr,
                kb,
                full_range: planes.full_range,
            })
            .unwrap();
        let (w, h) = (cpu.width, cpu.height);
        let mut compositor = Compositor::new(device.clone(), queue.clone(), w, h);
        let mut shown = |texture: &wgpu::Texture| {
            let view = texture.create_view(&Default::default());
            compositor.render(&[LayerDraw::fitted(&view, (w, h), 0)]);
            compositor.read_output().unwrap()
        };
        let a = shown(&Compositor::upload(&device, &queue, w, h, &cpu.rgba));
        let b = shown(&imported);
        let differences: Vec<u32> = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y) as u32).collect();
        let mean = differences.iter().sum::<u32>() as f64 / differences.len() as f64;
        // Chroma is interpolated a little differently at coloured edges; flat
        // areas must match to a level or two.
        let far = differences.iter().filter(|d| **d > 12).count() as f64 / differences.len() as f64;
        assert!(
            mean < 1.0 && far < 0.01,
            "case {n}: mean difference {mean:.2}, {:.2} % of values off by more than 12",
            far * 100.0
        );
    }
}
