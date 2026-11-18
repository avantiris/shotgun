//! Which pictures make up a frame: pure lookup in the snapshot.

use std::sync::Arc;

use sg_core::{AssetId, ClipId, ClipProps, Project, Sequence, TrackKind};

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
}

/// The layers visible at `frame` of `sequence`, bottom first. Costs one
/// O(log n) lookup per video track.
pub fn plan(project: &Project, sequence: &Sequence, frame: i64) -> Vec<PlanLayer> {
    let mut layers = Vec::new();
    // Tracks are stored top first; draw order is bottom first.
    for track in sequence.tracks.iter().rev().filter(|t| t.kind == TrackKind::Video) {
        let Some(clip) = track.clips.at(frame) else { continue };
        let props = track.props_of(clip.id).cloned();
        let at = frame - clip.start;
        if props.as_ref().is_some_and(|p| p.title.is_some()) {
            layers.push(PlanLayer { clip: clip.id, source: Source::Title, at, props });
            continue;
        }
        let Some(video) = project.assets.get(&clip.asset).and_then(|a| a.video.as_ref()) else { continue };
        let source_frame = if video.is_still() {
            0
        } else {
            // Position in the source in sequence frames (speed applied),
            // converted to the asset's rate: floor(n * asset_rate / sequence_rate).
            let n = (clip.src_in + props.as_ref().map_or(at, |p| p.source_offset(at))) as i128;
            let num = n * video.rate.num() as i128 * sequence.rate.den() as i128;
            let den = video.rate.den() as i128 * sequence.rate.num() as i128;
            (num.div_euclid(den) as i64).clamp(0, video.frames - 1)
        };
        layers.push(PlanLayer {
            clip: clip.id,
            source: Source::Asset { asset: clip.asset, frame: source_frame },
            at,
            props,
        });
    }
    layers
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
