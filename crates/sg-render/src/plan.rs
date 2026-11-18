//! Which pictures make up a frame: pure lookup in the snapshot.

use sg_core::{AssetId, ClipId, Project, Sequence, TrackKind};

/// One picture of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanLayer {
    pub clip: ClipId,
    pub asset: AssetId,
    /// Frame of the asset to show, at the asset's own frame rate.
    pub source_frame: i64,
    /// Degrees the stored picture is turned for display.
    pub rotation: i32,
}

/// The layers visible at `frame` of `sequence`, bottom first. Costs one
/// O(log n) lookup per video track.
pub fn plan(project: &Project, sequence: &Sequence, frame: i64) -> Vec<PlanLayer> {
    let mut layers = Vec::new();
    // Tracks are stored top first; draw order is bottom first.
    for track in sequence.tracks.iter().rev().filter(|t| t.kind == TrackKind::Video) {
        let Some(clip) = track.clips.at(frame) else { continue };
        let Some(video) = project.assets.get(&clip.asset).and_then(|a| a.video.as_ref()) else { continue };
        let source_frame = if video.is_still() {
            0
        } else {
            // Position inside the clip in sequence frames, converted to the
            // asset's rate: floor(n * asset_rate / sequence_rate).
            let n = (clip.src_in + frame - clip.start) as i128;
            let num = n * video.rate.num() as i128 * sequence.rate.den() as i128;
            let den = video.rate.den() as i128 * sequence.rate.num() as i128;
            (num.div_euclid(den) as i64).clamp(0, video.frames - 1)
        };
        layers.push(PlanLayer { clip: clip.id, asset: clip.asset, source_frame, rotation: video.rotation });
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
