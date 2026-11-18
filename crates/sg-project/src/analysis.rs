//! Analysis jobs on the conformed sound of assets: the beats of a song and
//! the position of a take in it. Blocking; run on a job thread.

use std::path::Path;

use sg_core::AssetId;
use sg_media::analysis::{self, Beats, Offset};
use sg_media::peaks::read_mono;
use sg_media::tool::{Cancel, ToolError};

use crate::layout::cache;

fn sound(root: &Path, id: AssetId, cancel: &Cancel) -> Result<Vec<f32>, ToolError> {
    let path = cache::audio(root, id);
    let (mono, rate) = read_mono(&path, cancel)
        .map_err(|e| ToolError::other(format!("its sound is not ready yet or it has none ({e})")))?;
    if rate as usize != analysis::RATE {
        return Err(ToolError::other(format!("conformed sound at {rate} Hz instead of 48 kHz")));
    }
    Ok(mono)
}

/// The beats of an asset's sound: read from the cache, or found and cached.
pub fn beats(root: &Path, id: AssetId, cancel: &Cancel) -> Result<Beats, ToolError> {
    let file = cache::beats(root, id);
    if let Some(known) = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
        return Ok(known);
    }
    let found = analysis::find_beats(&sound(root, id, cancel)?)
        .ok_or_else(|| ToolError::other("no steady rhythm was found in it"))?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Written under another name first: a reader never sees half a file.
    let temporary = file.with_extension("beats.tmp");
    std::fs::write(&temporary, serde_json::to_vec(&found).map_err(|e| ToolError::other(e.to_string()))?)?;
    std::fs::rename(&temporary, &file)?;
    Ok(found)
}

/// Where the sound of `take` sits in the sound of `song`.
pub fn sync(root: &Path, song: AssetId, take: AssetId, cancel: &Cancel) -> Result<Offset, ToolError> {
    let song = sound(root, song, cancel)?;
    let take = sound(root, take, cancel)?;
    analysis::find_offset(&song, &take).ok_or_else(|| ToolError::other("there is too little sound to compare"))
}
