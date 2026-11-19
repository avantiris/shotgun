//! The required bundle layout (docs/spec/03-project-bundle.md).

use std::path::{Path, PathBuf};

use crate::{ProjectError, at};

/// Every directory of a bundle. Created with the project and re-created on
/// open, because git does not track empty directories and caches may have
/// been left out of a zip.
const DIRS: &[&str] = &[
    "assets/video",
    "assets/audio",
    "assets/image",
    "assets/font",
    "assets/lut",
    "assets/index",
    "sequences",
    "scenarios",
    "looks",
    "exports",
    ".shotgun/journal",
    ".shotgun/cache/proxy",
    ".shotgun/cache/conform",
    ".shotgun/cache/thumb",
    ".shotgun/cache/peaks",
    ".shotgun/cache/render",
    ".shotgun/trash",
];

/// Media is ignored by directory and tracked through `assets/index` (spike S5).
pub const GITIGNORE: &str = "\
# Written by Shotgun. Media is tracked through assets/index/*.toml.
.shotgun/
assets/video/
assets/audio/
assets/image/
exports/
";

pub const GITATTRIBUTES: &str = "\
# Written by Shotgun.
*.sg text eol=lf
*.toml text eol=lf
";

pub fn ensure_dirs(root: &Path) -> Result<(), ProjectError> {
    for dir in DIRS {
        let path = root.join(dir);
        std::fs::create_dir_all(&path).map_err(at(&path))?;
    }
    Ok(())
}

pub fn journal_dir(root: &Path) -> PathBuf {
    root.join(".shotgun/journal")
}

pub fn journal_file(root: &Path) -> PathBuf {
    journal_dir(root).join("journal.log")
}

/// Per-machine state that is not part of the project (last opened time).
pub fn local_file(root: &Path) -> PathBuf {
    root.join(".shotgun/local.toml")
}

/// Where the files derived from an asset live. All of them can be deleted
/// and are rebuilt from the original.
pub mod cache {
    use std::path::{Path, PathBuf};

    use sg_core::AssetId;

    fn at(root: &Path, dir: &str, id: AssetId, ext: &str) -> PathBuf {
        root.join(".shotgun/cache").join(dir).join(format!("{id}.{ext}"))
    }

    /// Raw RGBA, `sg_media::derive::POSTER_W` x `POSTER_H`.
    pub fn poster(root: &Path, id: AssetId) -> PathBuf {
        at(root, "thumb", id, "rgba")
    }

    /// 48 kHz float WAV of the asset's audio.
    pub fn audio(root: &Path, id: AssetId) -> PathBuf {
        at(root, "conform", id, "wav")
    }

    pub fn peaks(root: &Path, id: AssetId) -> PathBuf {
        at(root, "peaks", id, "pk")
    }

    /// Result line of the frame-rate scan.
    pub fn scan(root: &Path, id: AssetId) -> PathBuf {
        at(root, "conform", id, "scan")
    }

    /// Constant-rate mezzanine, only present for assets that need one.
    pub fn conform(root: &Path, id: AssetId, alpha: bool) -> PathBuf {
        at(root, "conform", id, if alpha { "mkv" } else { "mp4" })
    }

    /// Tempo and beats of an asset's sound, as JSON.
    pub fn beats(root: &Path, id: AssetId) -> PathBuf {
        at(root, "analysis", id, "beats")
    }

    /// The asset with its camera shake taken out, and a proxy of that.
    pub fn stable(root: &Path, id: AssetId) -> PathBuf {
        at(root, "stable", id, "mp4")
    }

    pub fn stable_proxy(root: &Path, id: AssetId) -> PathBuf {
        at(root, "stable", id, "proxy.mp4")
    }

    pub fn proxy(root: &Path, id: AssetId) -> PathBuf {
        at(root, "proxy", id, "mp4")
    }
}
