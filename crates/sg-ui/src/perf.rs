//! Performance overlay: frame-time percentiles and journal latency, so that
//! bug reports can carry numbers.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;

use crate::session::Session;
use crate::theme;

const WINDOW: usize = 600;

#[derive(Default)]
pub struct Perf {
    pub show: bool,
    frames_ms: VecDeque<f32>,
    redraws: u64,
}

impl Perf {
    /// Records the CPU time of one UI frame (input to submit).
    pub fn push_frame(&mut self, ms: f32) {
        if self.frames_ms.len() == WINDOW {
            self.frames_ms.pop_front();
        }
        self.frames_ms.push_back(ms);
        self.redraws += 1;
    }

    /// (p50, p99, max) over the last frames, in ms.
    pub fn frame_stats(&self) -> (f32, f32, f32) {
        let mut sorted: Vec<f32> = self.frames_ms.iter().copied().collect();
        if sorted.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        sorted.sort_by(f32::total_cmp);
        let at = |q: f32| sorted[((sorted.len() - 1) as f32 * q).round() as usize];
        (at(0.5), at(0.99), sorted[sorted.len() - 1])
    }

    pub fn ui(&self, ctx: &egui::Context, session: Option<&Session>, viewer: &[String]) {
        if !self.show {
            return;
        }
        let (p50, p99, max) = self.frame_stats();
        let mut lines = vec![
            format!("frame cpu  p50 {p50:.2}  p99 {p99:.2}  max {max:.2} ms"),
            format!("budget     6.90 ms p99   ({} frames, {} redraws)", self.frames_ms.len(), self.redraws),
        ];
        if let Some(session) = session {
            let status = session.writer_status();
            let us = |ns: u64| ns as f64 / 1000.0;
            lines.push(format!(
                "edit apply last {:.1} us   max {:.1} us",
                us(session.apply_last_ns),
                us(session.apply_max_ns)
            ));
            lines.push(format!(
                "journal    last {:.1} us   max {:.1} us",
                us(status.append_last_ns.load(Ordering::Relaxed)),
                us(status.append_max_ns.load(Ordering::Relaxed))
            ));
            lines.push(format!("clips      {}", session.project().clip_count()));
            lines.extend(viewer.iter().cloned());
        }
        egui::Area::new(egui::Id::unique("perf-overlay"))
            .fade_in(false)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-10.0, 44.0))
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::BG.gamma_multiply(0.92))
                    .stroke(egui::Stroke::new(1.0, theme::LINE))
                    .corner_radius(4.0)
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        for line in lines {
                            ui.label(egui::RichText::new(line).monospace().size(11.0));
                        }
                    });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_over_a_sliding_window() {
        let mut perf = Perf::default();
        assert_eq!(perf.frame_stats(), (0.0, 0.0, 0.0));
        for i in 1..=100 {
            perf.push_frame(i as f32);
        }
        let (p50, p99, max) = perf.frame_stats();
        assert_eq!((p50, p99, max), (51.0, 99.0, 100.0));
        for _ in 0..WINDOW {
            perf.push_frame(1.0);
        }
        assert_eq!(perf.frame_stats(), (1.0, 1.0, 1.0));
    }
}
