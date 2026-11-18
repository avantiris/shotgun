//! Clip labels via glyphon (cosmic-text shaping + GPU atlas), drawn inside
//! egui's render pass through a paint callback. Buffers are cached per clip.

use std::collections::HashMap;
use std::time::Instant;

use egui_wgpu::wgpu;
use glyphon::{
    Attrs, Buffer, Cache, Color, ColorMode, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};

use crate::timeline::Project;

/// Label request for one frame; coordinates are physical px relative to the timeline rect.
pub struct Label {
    pub track: u32,
    pub clip: u32,
    pub x: f32,
    pub y: f32,
    pub right: f32,
    pub bottom: f32,
}

pub struct GlyphonLabels {
    font_system: FontSystem,
    swash: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    renderer: TextRenderer,
    buffers: HashMap<(u32, u32), (Buffer, u64)>,
    frame: u64,
    pub misses: u64,
}

impl GlyphonLabels {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let t = Instant::now();
        let font_system = FontSystem::new(); // scans system fonts (fontdb)
        println!("glyphon: FontSystem::new() took {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        // ColorMode::Web = blend in gamma space, same as egui on a non-sRGB target.
        let mut atlas = TextAtlas::with_color_mode(device, queue, &cache, format, ColorMode::Web);
        let renderer = TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        GlyphonLabels {
            font_system,
            swash: SwashCache::new(),
            viewport,
            atlas,
            renderer,
            buffers: HashMap::new(),
            frame: 0,
            misses: 0,
        }
    }

    pub fn prepare(
        mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        project: &Project,
        labels: &[Label],
        size_px: [u32; 2],
        ppp: f32,
    ) -> Self {
        self.frame += 1;
        self.viewport.update(queue, Resolution { width: size_px[0].max(1), height: size_px[1].max(1) });
        let line_h = (14.0 * ppp).round();
        for l in labels {
            let e = self.buffers.entry((l.track, l.clip)).or_insert_with(|| {
                self.misses += 1;
                let mut b = Buffer::new(&mut self.font_system, Metrics::new(11.0 * ppp, line_h));
                b.set_size(None, None);
                b.set_text(
                    &project.tracks[l.track as usize].names[l.clip as usize],
                    &Attrs::new().family(Family::SansSerif),
                    Shaping::Advanced,
                    None,
                );
                b.shape_until_scroll(&mut self.font_system, false);
                (b, 0)
            });
            e.1 = self.frame;
        }
        let buffers = &self.buffers;
        let areas = labels.iter().map(|l| TextArea {
            buffer: &buffers[&(l.track, l.clip)].0,
            left: l.x,
            top: l.y + ((l.bottom - l.y) - line_h) * 0.5,
            scale: 1.0,
            bounds: TextBounds {
                left: l.x as i32,
                top: l.y as i32,
                right: l.right as i32,
                bottom: l.bottom as i32,
            },
            default_color: Color::rgb(245, 245, 245),
            custom_glyphs: &[],
        });
        self.renderer
            .prepare(device, queue, &mut self.font_system, &mut self.atlas, &self.viewport, areas, &mut self.swash)
            .unwrap();
        if self.buffers.len() > 8192 {
            let f = self.frame;
            self.buffers.retain(|_, v| v.1 == f);
        }
        self
    }
}

pub struct GlyphonCallback {
    pub size_px: [u32; 2],
}

impl egui_wgpu::CallbackTrait for GlyphonCallback {
    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        res: &egui_wgpu::CallbackResources,
    ) {
        if let Some(t) = res.get::<GlyphonLabels>() {
            t.renderer.render(&t.atlas, &t.viewport, pass).unwrap();
        }
    }
}
