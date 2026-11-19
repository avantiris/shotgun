//! The asset panel: what is in the project, what is being imported, and the
//! popover for adding a link or picking files.

use std::collections::{BTreeMap, HashMap, HashSet};

use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use sg_core::{Asset, AssetId, AssetKind, Project};

use crate::theme;

/// Posters are stored as raw RGBA of this size (`sg_media::derive`).
pub const POSTER_SIZE: [usize; 2] = [320, 180];

pub struct JobView {
    pub label: String,
    pub stage: &'static str,
    pub fraction: Option<f32>,
}

enum Poster {
    Requested,
    Ready(egui::TextureHandle),
    /// Not made yet or unreadable; asked for again when the import reports it.
    Absent,
}

pub enum AssetRequest {
    LoadPoster(AssetId),
    /// A link, and whether only its sound is wanted.
    ImportLink(String, bool),
    Browse,
    CancelJob(u64),
    /// Put the selected asset on the timeline (double-click), or only its
    /// picture or sound.
    Insert(Option<sg_core::TrackKind>),
    /// Make the selected asset the song of the sequence.
    UseAsSong,
    /// Find the selected take (`false`) or every take (`true`) in the song.
    Sync(bool),
    /// Put the selected asset into a scenario, or take it out.
    Scenario(crate::session::ScenarioChoice),
    /// Remove the selected asset from the project.
    Remove,
}

#[derive(Default)]
pub struct AssetsPanel {
    tab: Option<AssetKind>,
    search: String,
    pub selected: Option<AssetId>,
    posters: HashMap<AssetId, Poster>,
    pub jobs: BTreeMap<u64, JobView>,
    pub missing: HashSet<AssetId>,
    add_open: bool,
    focus_link: bool,
    link: String,
    /// Import only the sound of the link.
    sound_only: bool,
    /// A drag from another application is over the window.
    pub drag_hover: bool,
}

/// `m:ss` or `h:mm:ss`, for display.
fn clock(frames: i64, num: u32, den: u32) -> String {
    let secs = (frames as i128 * den as i128 / num.max(1) as i128) as i64;
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// One line under the tile: length and size.
pub fn summary(asset: &Asset) -> String {
    match (&asset.video, &asset.audio) {
        (Some(v), _) if v.is_still() => format!("{}x{}", v.display_size().0, v.display_size().1),
        (Some(v), _) => {
            let (w, h) = v.display_size();
            format!("{}  {w}x{h}", clock(v.frames, v.rate.num(), v.rate.den()))
        }
        (None, Some(a)) => clock(a.samples, a.sample_rate, 1),
        (None, None) => String::new(),
    }
}

impl AssetsPanel {
    pub fn open_add(&mut self) {
        self.add_open = true;
        self.focus_link = true;
    }

    /// Closes the popover. Returns whether it was open.
    pub fn close_add(&mut self) -> bool {
        std::mem::take(&mut self.add_open)
    }

    pub fn set_poster(&mut self, ctx: &egui::Context, id: AssetId, rgba: Option<Vec<u8>>) {
        let poster = match rgba.filter(|b| b.len() == POSTER_SIZE[0] * POSTER_SIZE[1] * 4) {
            Some(bytes) => {
                let image = egui::ColorImage::from_rgba_unmultiplied(POSTER_SIZE, &bytes);
                Poster::Ready(ctx.load_texture(format!("poster-{id}"), image, egui::TextureOptions::LINEAR))
            }
            None => Poster::Absent,
        };
        self.posters.insert(id, poster);
    }

    /// The poster file was (re)written: load it when the tile is next shown.
    pub fn poster_changed(&mut self, id: AssetId) {
        self.posters.remove(&id);
    }

    fn visible<'a>(&self, project: &'a Project) -> Vec<&'a Asset> {
        let needle = self.search.trim().to_lowercase();
        let mut assets: Vec<&Asset> = project
            .assets
            .values()
            .map(|a| &**a)
            .filter(|a| self.tab.is_none_or(|k| a.kind == k))
            .filter(|a| {
                needle.is_empty()
                    || a.label.to_lowercase().contains(&needle)
                    || a.tags.iter().any(|t| t.to_lowercase().contains(&needle))
            })
            .collect();
        // Newest first: what was just imported is what gets used next.
        assets.sort_by(|a, b| b.source.imported.cmp(&a.source.imported).then_with(|| a.label.cmp(&b.label)));
        assets
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, project: &Project, out: &mut Vec<AssetRequest>) {
        ui.take_available_width();
        ui.horizontal(|ui| {
            ui.label(RichText::new("ASSETS").size(11.0).strong().color(theme::DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Add")
                    .on_hover_text("Paste a link or pick files (Ctrl+Q). Dropping files anywhere works too.")
                    .clicked()
                {
                    self.open_add();
                }
            });
        });
        ui.horizontal(|ui| {
            for (label, kind) in [
                ("All", None),
                ("Video", Some(AssetKind::Video)),
                ("Audio", Some(AssetKind::Audio)),
                ("Image", Some(AssetKind::Image)),
            ] {
                if ui.selectable_label(self.tab == kind, label).clicked() {
                    self.tab = kind;
                }
            }
        });
        ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search").desired_width(f32::INFINITY));
        ui.add_space(4.0);

        let mut cancel = None;
        for (job, view) in &self.jobs {
            ui.horizontal(|ui| {
                if ui.button("x").on_hover_text("Cancel").clicked() {
                    cancel = Some(*job);
                }
                let text = format!("{}: {}", view.stage, view.label);
                let bar = egui::ProgressBar::new(view.fraction.unwrap_or(0.0))
                    .text(RichText::new(text).size(11.0))
                    .desired_height(16.0);
                // Unknown size: an animated bar would redraw forever, so show an empty one.
                ui.add(bar);
            });
        }
        if let Some(job) = cancel {
            out.push(AssetRequest::CancelJob(job));
        }

        let assets = self.visible(project);
        if assets.is_empty() {
            let text = if project.assets.is_empty() {
                "Drop files here or onto the timeline, paste a link with Ctrl+V, or press Ctrl+Q."
            } else {
                "Nothing matches."
            };
            ui.add_space(8.0);
            ui.label(RichText::new(text).color(theme::DIM));
            return;
        }
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            const GAP: f32 = 8.0;
            let width = ui.available_width();
            let columns = (((width + GAP) / (140.0 + GAP)).floor() as usize).max(1);
            let tile_w = ((width - GAP * (columns - 1) as f32) / columns as f32).floor();
            let tile = vec2(tile_w, tile_w * 9.0 / 16.0 + 34.0);
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                for asset in assets {
                    let (rect, response) = ui.allocate_exact_size(tile, Sense::click_and_drag());
                    // Dragged onto the timeline, the asset is placed where it is let go.
                    response.dnd_set_drag_payload(asset.id);
                    if response.drag_started() {
                        self.selected = Some(asset.id);
                    }
                    if response.clicked() {
                        self.selected = Some(asset.id);
                    }
                    if response.double_clicked() {
                        self.selected = Some(asset.id);
                        out.push(AssetRequest::Insert(None));
                    }
                    if response.secondary_clicked() {
                        self.selected = Some(asset.id);
                    }
                    response.context_menu(|ui| {
                        let both = asset.video.is_some() && asset.audio.is_some();
                        if ui.button("Place at the playhead (Enter)").clicked() {
                            out.push(AssetRequest::Insert(None));
                            ui.close();
                        }
                        if both && ui.button("Place only the sound").clicked() {
                            out.push(AssetRequest::Insert(Some(sg_core::TrackKind::Audio)));
                            ui.close();
                        }
                        if both && ui.button("Place only the picture").clicked() {
                            out.push(AssetRequest::Insert(Some(sg_core::TrackKind::Video)));
                            ui.close();
                        }
                        if asset.video.is_some() {
                            ui.menu_button("Scenario", |ui| {
                                use crate::session::ScenarioChoice;
                                let current = project.scenario_of(asset.id).map(|s| s.id);
                                for scenario in project.scenarios.values() {
                                    if ui.selectable_label(current == Some(scenario.id), &scenario.name).clicked() {
                                        out.push(AssetRequest::Scenario(ScenarioChoice::Existing(scenario.id)));
                                        ui.close();
                                    }
                                }
                                if ui.button("New scenario").clicked() {
                                    out.push(AssetRequest::Scenario(ScenarioChoice::New));
                                    ui.close();
                                }
                                if current.is_some() && ui.button("None").clicked() {
                                    out.push(AssetRequest::Scenario(ScenarioChoice::None));
                                    ui.close();
                                }
                            });
                        }
                        ui.separator();
                        if asset.audio.is_some()
                            && ui
                                .button("Use as the song")
                                .on_hover_text("Its beats become the grid; takes are synchronised to it.")
                                .clicked()
                        {
                            out.push(AssetRequest::UseAsSong);
                            ui.close();
                        }
                        if both
                            && ui
                                .button("Sync to the song")
                                .on_hover_text(
                                    "Finds this take in the song by its camera sound and lays it on a camera track.",
                                )
                                .clicked()
                        {
                            out.push(AssetRequest::Sync(false));
                            ui.close();
                        }
                        if ui.button("Sync all takes to the song").clicked() {
                            out.push(AssetRequest::Sync(true));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Remove from project (Del)").clicked() {
                            out.push(AssetRequest::Remove);
                            ui.close();
                        }
                    });
                    if ui.is_rect_visible(rect) {
                        if let std::collections::hash_map::Entry::Vacant(e) = self.posters.entry(asset.id) {
                            e.insert(Poster::Requested);
                            out.push(AssetRequest::LoadPoster(asset.id));
                        }
                        self.paint_tile(ui, rect, asset, response.hovered());
                    }
                }
            });
        });
        Self::paint_dragged(ui, project);
    }

    /// A small tag at the pointer naming the asset being dragged.
    fn paint_dragged(ui: &egui::Ui, project: &Project) {
        let Some(id) = egui::DragAndDrop::payload::<AssetId>(ui.ctx()) else { return };
        let (Some(asset), Some(pos)) = (project.assets.get(&*id), ui.ctx().pointer_interact_pos()) else { return };
        let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("dragged-asset"));
        let painter = ui.ctx().layer_painter(layer);
        let galley = painter.layout_no_wrap(asset.label.clone(), FontId::proportional(12.0), theme::TEXT);
        let tag = Rect::from_min_size(pos + vec2(12.0, 12.0), galley.size() + vec2(12.0, 8.0));
        painter.rect_filled(tag, 4.0, theme::RAISED);
        painter.rect_stroke(tag, 4.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
        painter.galley(tag.min + vec2(6.0, 4.0), galley, theme::TEXT);
    }

    fn paint_tile(&self, ui: &egui::Ui, rect: Rect, asset: &Asset, hovered: bool) {
        let painter = ui.painter().with_clip_rect(rect);
        let poster = Rect::from_min_size(rect.min, vec2(rect.width(), rect.width() * 9.0 / 16.0));
        painter.rect_filled(poster, 4.0, Color32::BLACK);
        let missing = self.missing.contains(&asset.id);
        match self.posters.get(&asset.id) {
            Some(Poster::Ready(texture)) if !missing => {
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                painter.image(texture.id(), poster, uv, Color32::WHITE);
            }
            _ => {
                let (text, colour) = if missing { ("MISSING", theme::ERROR) } else { (asset.kind.dir(), theme::DIM) };
                painter.text(poster.center(), Align2::CENTER_CENTER, text, FontId::proportional(12.0), colour);
            }
        }
        let accent = match asset.kind {
            AssetKind::Video => theme::VIDEO,
            AssetKind::Audio => theme::AUDIO,
            AssetKind::Image => theme::ACCENT,
        };
        painter.rect_filled(
            Rect::from_min_size(poster.left_bottom() - vec2(0.0, 2.0), vec2(poster.width(), 2.0)),
            0.0,
            accent,
        );
        painter.text(
            pos2(rect.left() + 1.0, poster.bottom() + 4.0),
            Align2::LEFT_TOP,
            &asset.label,
            FontId::proportional(12.0),
            theme::TEXT,
        );
        painter.text(
            pos2(rect.left() + 1.0, poster.bottom() + 19.0),
            Align2::LEFT_TOP,
            summary(asset),
            FontId::proportional(10.0),
            theme::DIM,
        );
        let stroke = if self.selected == Some(asset.id) {
            Some(Stroke::new(2.0, theme::ACCENT))
        } else if hovered {
            Some(Stroke::new(1.0, theme::DIM))
        } else {
            None
        };
        if let Some(stroke) = stroke {
            painter.rect_stroke(poster, 4.0, stroke, StrokeKind::Inside);
        }
    }

    /// The add-asset popover (Ctrl+Q): a link field and a file picker button.
    pub fn popover(&mut self, ctx: &egui::Context, out: &mut Vec<AssetRequest>) {
        if !self.add_open {
            return;
        }
        egui::Area::new(egui::Id::unique("add-asset"))
            .fade_in(false)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 80.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::RAISED)
                    .stroke(Stroke::new(1.0, theme::LINE))
                    .corner_radius(6.0)
                    .inner_margin(14.0)
                    .show(ui, |ui| {
                        ui.set_width(460.0);
                        ui.label(RichText::new("ADD ASSET").size(11.0).strong().color(theme::DIM));
                        let field = egui::TextEdit::singleline(&mut self.link)
                            .hint_text("Paste a link: YouTube, Giphy, Google Photos, any media URL")
                            .desired_width(f32::INFINITY);
                        let response = ui.add(field);
                        if std::mem::take(&mut self.focus_link) {
                            response.request_focus();
                        }
                        let entered = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.horizontal(|ui| {
                            let link = self.link.trim().to_owned();
                            if (ui.add_enabled(!link.is_empty(), egui::Button::new("Import link")).clicked() || entered)
                                && !link.is_empty()
                            {
                                out.push(AssetRequest::ImportLink(link, self.sound_only));
                                self.link.clear();
                                self.add_open = false;
                            }
                            if ui.button("Choose files...").clicked() {
                                out.push(AssetRequest::Browse);
                                self.add_open = false;
                            }
                            ui.checkbox(&mut self.sound_only, "Sound only")
                                .on_hover_text("Take only the sound of the link, for music from a video site.");
                            ui.label(RichText::new("Esc to close").color(theme::DIM));
                        });
                    });
            });
    }

    /// The poster texture of an asset, if it is loaded.
    pub fn poster_id(&self, id: AssetId) -> Option<egui::TextureId> {
        match self.posters.get(&id) {
            Some(Poster::Ready(texture)) => Some(texture.id()),
            _ => None,
        }
    }

    /// Shown while files are dragged over the window.
    pub fn drop_overlay(&self, ctx: &egui::Context) {
        if !self.drag_hover {
            return;
        }
        let rect = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop-overlay")));
        painter.rect_filled(rect, 0.0, theme::BG.gamma_multiply(0.75));
        painter.rect_stroke(rect.shrink(12.0), 8.0, Stroke::new(2.0, theme::ACCENT), StrokeKind::Inside);
        painter.text(rect.center(), Align2::CENTER_CENTER, "Drop to import", FontId::proportional(28.0), theme::TEXT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_format() {
        assert_eq!(clock(1799, 60_000, 1001), "0:30");
        assert_eq!(clock(60 * 3725, 60, 1), "1:02:05");
        assert_eq!(clock(48_000 * 75, 48_000, 1), "1:15");
    }
}
