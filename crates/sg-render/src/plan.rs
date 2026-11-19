//! Which pictures make up a frame: pure lookup in the snapshot.

use std::sync::Arc;

use sg_core::{
    AssetId, Clip, ClipId, ClipProps, Grade, Project, Sequence, Track, TrackKind, Transition, TransitionKind,
};

use crate::compositor::{LayerDraw, Motion};

/// Where a layer's picture comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// Frame `frame` of the asset, at the asset's own frame rate.
    Asset { asset: AssetId, frame: i64 },
    /// Text, drawn from the clip's properties.
    Title,
}

/// One picture of a frame.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanLayer {
    pub clip: ClipId,
    pub source: Source,
    /// Sequence frames since the clip's start: the time its keyframes use.
    pub at: i64,
    pub props: Option<Arc<ClipProps>>,
    /// Colour treatment of the picture, from the scenario of its asset.
    pub grade: Option<Grade>,
    /// Length of the clip in frames, for its fades.
    pub len: i64,
    /// Sequence frame this layer was planned for.
    pub frame: i64,
    /// Movement during this frame, when it lies inside a transition.
    pub motion: Option<Motion>,
}

impl PlanLayer {
    /// How the compositor draws this layer with the given picture. The one
    /// place where properties become a drawing, for preview and export alike.
    pub fn draw<'a>(
        &'a self,
        view: &'a wgpu::TextureView,
        size: (u32, u32),
        rotation: i32,
        natural_size: bool,
    ) -> LayerDraw<'a> {
        let default = ClipProps::default();
        let props = self.props.as_deref().unwrap_or(&default);
        LayerDraw {
            view,
            size,
            rotation,
            // The fades of a clip fade its picture as they fade its sound.
            opacity: props.opacity.at(self.at) * ClipProps::fade_gain(self.at, self.len, props.fade_in, props.fade_out),
            x: props.x.at(self.at),
            y: props.y.at(self.at),
            scale: props.scale.at(self.at),
            turn_degrees: props.rotation.at(self.at),
            crop: props.crop,
            natural_size,
            motion: self.motion,
            grade: self.grade.as_ref(),
            // Any number that is new each frame and the same in preview and export.
            seed: (self.frame.rem_euclid(4093)) as f32,
        }
    }
}

/// The layers visible at `frame` of `sequence`, bottom first. Costs a few
/// O(log n) lookups per video track.
pub fn plan(project: &Project, sequence: &Sequence, frame: i64) -> Vec<PlanLayer> {
    let mut layers = Vec::new();
    // Tracks are stored top first; draw order is bottom first.
    // Hidden tracks hold camera angles for the programme track to draw on.
    for track in sequence.tracks.iter().rev().filter(|t| t.kind == TrackKind::Video && !t.mix.hidden) {
        plan_transitions(project, sequence, track, frame, &mut layers);
    }
    layers
}

/// What one video track shows at `frame` as a plain picture, hidden or
/// not and without transitions: for the camera-angle monitor.
pub fn plan_track(project: &Project, sequence: &Sequence, track: &Track, frame: i64) -> Option<PlanLayer> {
    layer(project, sequence, track, &track.clips.at(frame)?, frame, None)
}

/// The picture of `clip` at `frame`, which may lie a little before its
/// start or after its end (a slide shows both clips on both sides of the
/// cut): the source then runs on if there is more of it, and holds if not.
fn layer(
    project: &Project,
    sequence: &Sequence,
    track: &Track,
    clip: &Clip,
    frame: i64,
    motion: Option<Motion>,
) -> Option<PlanLayer> {
    let props = track.props_of(clip.id).cloned();
    let elapsed = frame - clip.start;
    // Keyframes are read inside the clip.
    let at = elapsed.clamp(0, (clip.len - 1).max(0));
    if props.as_ref().is_some_and(|p| p.title.is_some()) {
        return Some(PlanLayer {
            clip: clip.id,
            source: Source::Title,
            at,
            props,
            motion,
            grade: None,
            frame,
            len: clip.len,
        });
    }
    let video = project.assets.get(&clip.asset).and_then(|a| a.video.as_ref())?;
    let source_frame = if video.is_still() {
        0
    } else {
        // Position in the source in sequence frames (speed applied),
        // converted to the asset's rate: floor(n * asset_rate / sequence_rate).
        let n = (clip.src_in + props.as_ref().map_or(elapsed, |p| p.source_offset(elapsed))) as i128;
        let num = n * video.rate.num() as i128 * sequence.rate.den() as i128;
        let den = video.rate.den() as i128 * sequence.rate.num() as i128;
        (num.div_euclid(den) as i64).clamp(0, video.frames - 1)
    };
    // The scenario's match and look, then the clip's own trim on top of the match.
    let mut grade = project.grade_of(clip.asset);
    if let Some(trim) = props.as_ref().and_then(|p| p.trim_gain()) {
        let graded = grade.get_or_insert(Grade { correction: sg_core::Correction::NONE, look: Vec::new() });
        for (gain, by) in graded.correction.gain.iter_mut().zip(trim) {
            *gain *= by;
        }
    }
    Some(PlanLayer {
        clip: clip.id,
        source: Source::Asset { asset: clip.asset, frame: source_frame },
        at,
        props,
        motion,
        grade,
        frame,
        len: clip.len,
    })
}

/// The transition at the start of `clip`, if it has one.
fn transition_of(track: &Track, clip: &Clip) -> Option<Transition> {
    track.props_of(clip.id)?.transition.map(Transition::clamped)
}

/// What a shown track contributes at `frame`: its clip, moved if the frame
/// lies in a transition; for a slide the clip on the other side of the cut
/// as well.
fn plan_transitions(project: &Project, sequence: &Sequence, track: &Track, frame: i64, out: &mut Vec<PlanLayer>) {
    let Some(clip) = track.clips.at(frame) else { return };
    // The cut this frame belongs to: the one at the clip's start if it has
    // a transition that still runs, else the one at its end.
    let arriving =
        transition_of(track, &clip).filter(|t| frame < clip.start + t.frames as i64 / 2).map(|t| (t, clip.start));
    let cut = arriving.or_else(|| {
        let next = track.clips.at(clip.end()).filter(|next| next.start == clip.end())?;
        let t = transition_of(track, &next)?;
        (frame >= next.start - t.frames as i64 / 2).then_some((t, next.start))
    });
    let Some((transition, at)) = cut else {
        out.extend(layer(project, sequence, track, &clip, frame, None));
        return;
    };
    let length = transition.frames as f32;
    let progress = (frame - (at - transition.frames as i64 / 2)) as f32 / length;
    // The shutter stays open for `blur` of a frame.
    let closes = progress + transition.blur / length;
    let motion = |incoming: bool| {
        Some(Motion {
            from: transition.shift(progress, incoming),
            to: transition.shift(closes, incoming),
            mirror: transition.kind != TransitionKind::Slide,
        })
    };
    let incoming = frame >= at;
    if transition.kind == TransitionKind::Slide {
        // Both pictures, side by side, all the way through.
        let (before, after) = (track.clips.at(at - 1), track.clips.at(at));
        out.extend(before.and_then(|c| layer(project, sequence, track, &c, frame, motion(false))));
        out.extend(after.and_then(|c| layer(project, sequence, track, &c, frame, motion(true))));
    } else {
        out.extend(layer(project, sequence, track, &clip, frame, motion(incoming)));
    }
}

/// Length of the sequence in frames: the end of its last clip. Audio
/// tracks count in samples and are converted.
pub fn sequence_frames(sequence: &Sequence, sample_rate: u32) -> i64 {
    sequence
        .tracks
        .iter()
        .map(|t| match t.kind {
            TrackKind::Video => t.clips.end(),
            TrackKind::Audio => {
                let samples = t.clips.end();
                // Round up so the last partial frame of audio is inside.
                let frames = sequence.rate.samples_to_frames(samples, sample_rate);
                if sequence.rate.frames_to_samples(frames, sample_rate) < samples { frames + 1 } else { frames }
            }
        })
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use sg_core::{Asset, AssetKind, IdGen, Op, Rate, SequenceHeader, TrackHeader, TrackId, VideoInfo, apply};

    use super::*;

    const TRACK: TrackId = TrackId(1);

    /// Two takes of 300 frames each, cut together at frame 100: the first
    /// shows its frames 0..100, the second its frames 50..150 from there.
    fn cut(transition: Option<Transition>) -> Arc<Project> {
        let mut ids = IdGen::new(1);
        let mut project = Arc::new(Project::new(ids.project(), "t"));
        let seq = ids.sequence();
        let header = SequenceHeader { id: seq, name: "s".into(), rate: Rate::FPS_60, width: 1920, height: 1080 };
        let mut ops = vec![
            Op::AddSequence { index: 0, sequence: header },
            Op::AddTrack { seq, index: 0, track: TrackHeader { id: TRACK, kind: TrackKind::Video, name: "V".into() } },
        ];
        for (id, start, src_in) in [(1u128, 0i64, 0i64), (2, 100, 50)] {
            let video = VideoInfo {
                codec: "h264".into(),
                width: 1920,
                height: 1080,
                rate: Rate::FPS_60,
                frames: 300,
                pix_fmt: "yuv420p".into(),
                bit_depth: 8,
                rotation: 0,
                alpha: false,
                header_vfr: false,
                color_primaries: None,
                color_transfer: None,
                color_space: None,
                color_range: None,
            };
            let asset = Asset {
                id: AssetId(id),
                blake3: format!("{id:064x}"),
                size: 1,
                file: format!("assets/video/{id}.mp4"),
                kind: AssetKind::Video,
                label: format!("take {id}"),
                tags: vec![],
                source: sg_core::Source { origin: "test".into(), imported: 0, uploader: None },
                video: Some(video),
                audio: None,
            };
            ops.push(Op::AddAsset { asset: Box::new(asset) });
            let clip = Clip { id: ClipId(id as u64), asset: AssetId(id), start, len: 100, src_in };
            ops.push(Op::InsertClip { seq, track: TRACK, clip });
        }
        if let Some(transition) = transition {
            let props = ClipProps { transition: Some(transition), ..ClipProps::default() };
            ops.push(Op::SetClipProps { seq, track: TRACK, clip: ClipId(2), props: Some(Box::new(props)) });
        }
        for op in ops {
            project = apply(&project, &op).unwrap().0;
        }
        project
    }

    fn sources(project: &Project, frame: i64) -> Vec<(u128, i64)> {
        plan(project, &project.sequences[0], frame)
            .iter()
            .map(|l| match l.source {
                Source::Asset { asset, frame } => (asset.0, frame),
                Source::Title => (0, 0),
            })
            .collect()
    }

    #[test]
    fn a_whip_shows_one_picture_per_frame_and_moves_it() {
        let plain = cut(None);
        let whip = cut(Some(Transition::default()));
        let seq = &whip.sequences[0];
        // The same pictures as a plain cut, on every frame.
        for frame in [0, 97, 98, 99, 100, 101, 102, 150] {
            assert_eq!(sources(&whip, frame), sources(&plain, frame), "frame {frame}");
        }
        // Outside the four frames around the cut nothing moves.
        assert_eq!(plan(&whip, seq, 97)[0].motion, None);
        assert_eq!(plan(&whip, seq, 102)[0].motion, None);
        // Inside: out to the left before the cut, in from the right after
        // it, each frame starting where the one before stopped.
        let motions: Vec<Motion> = (98..102).map(|f| plan(&whip, seq, f)[0].motion.unwrap()).collect();
        assert_eq!(motions[0].from.x, 0.0);
        assert!(motions[0].to.x < 0.0 && motions[1].to.x < motions[0].to.x);
        assert_eq!(motions[0].to, motions[1].from);
        assert!((motions[1].to.x + 1.5).abs() < 1e-4, "{:?}", motions[1]);
        assert!((motions[2].from.x - 1.5).abs() < 1e-4 && motions[3].to.x == 0.0, "{:?}", motions[2]);
        assert!(motions.iter().all(|m| m.mirror));
    }

    #[test]
    fn a_slide_shows_both_pictures_and_runs_them_past_the_cut() {
        let slide = cut(Some(Transition::of(TransitionKind::Slide)));
        let seq = &slide.sequences[0];
        // Eight frames, 96..104. Before the cut the second take is shown
        // from before its in-point; after it the first take runs on.
        assert_eq!(sources(&slide, 95), vec![(1, 95)]);
        assert_eq!(sources(&slide, 96), vec![(1, 96), (2, 46)]);
        assert_eq!(sources(&slide, 103), vec![(1, 103), (2, 53)]);
        assert_eq!(sources(&slide, 104), vec![(2, 54)]);
        let layers = plan(&slide, seq, 100);
        let (out, arriving) = (layers[0].motion.unwrap(), layers[1].motion.unwrap());
        assert!(!out.mirror && (arriving.from.x - out.from.x - 1.0).abs() < 1e-5);
        // No transition without a clip before it to come from.
        let lonely = apply(&slide, &Op::RemoveClip { seq: seq.id, track: TRACK, start: 0 }).unwrap().0;
        assert_eq!(sources(&lonely, 100), vec![(2, 50)]);
    }

    #[test]
    fn a_clip_trim_becomes_a_grade_even_without_a_scenario() {
        let plain = cut(None);
        let seq = plain.sequences[0].id;
        assert_eq!(plan(&plain, &plain.sequences[0], 150)[0].grade, None);
        let props = ClipProps { exposure: 1.0, warmth: 1.0, ..ClipProps::default() };
        let op = Op::SetClipProps { seq, track: TRACK, clip: ClipId(2), props: Some(Box::new(props)) };
        let trimmed = apply(&plain, &op).unwrap().0;
        let gain = plan(&trimmed, &trimmed.sequences[0], 150)[0].grade.as_ref().unwrap().correction.gain;
        // One stop brighter, and warmer: more red than green, less blue.
        assert!((gain[1] - 2.0).abs() < 1e-5 && gain[0] > 2.5 && gain[2] < 1.6, "{gain:?}");
        // The other clip is untouched.
        assert_eq!(plan(&trimmed, &trimmed.sequences[0], 50)[0].grade, None);
    }
}
