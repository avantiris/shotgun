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
