//! The library: one directory whose direct subfolders with a `project.toml`
//! are the projects.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sg_core::ProjectId;
use sg_core::text::{PROJECT_FILE, project_header};

use crate::layout::local_file;
use crate::{ProjectError, at, unix_now};

/// `/disk/shotgun`, or `SHOTGUN_LIBRARY` for tests and experiments.
pub fn library_dir() -> PathBuf {
    std::env::var_os("SHOTGUN_LIBRARY").map_or_else(|| PathBuf::from("/disk/shotgun"), PathBuf::from)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectEntry {
    pub path: PathBuf,
    /// Folder name.
    pub slug: String,
    pub name: String,
    pub id: Option<ProjectId>,
    /// Unix seconds, 0 if never opened on this machine.
    pub last_opened: u64,
    /// Set if `project.toml` could not be read; the tile shows it.
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct Local {
    #[serde(default)]
    last_opened: u64,
}

/// Lists the projects, most recently opened first. Reads one or two small
/// files per project and nothing else.
pub fn scan(library: &Path) -> Result<Vec<ProjectEntry>, ProjectError> {
    std::fs::create_dir_all(library).map_err(at(library))?;
    let mut entries = Vec::new();
    for dir in std::fs::read_dir(library).map_err(at(library))?.flatten() {
        let path = dir.path();
        let Ok(toml) = std::fs::read_to_string(path.join(PROJECT_FILE)) else { continue };
        let slug = dir.file_name().to_string_lossy().into_owned();
        let last_opened = std::fs::read_to_string(local_file(&path))
            .ok()
            .and_then(|text| toml::from_str::<Local>(&text).ok())
            .map_or(0, |local| local.last_opened);
        let entry = match project_header(&toml) {
            Ok((id, name)) => ProjectEntry { path, slug, name, id: Some(id), last_opened, error: None },
            Err(e) => {
                ProjectEntry { path, name: slug.clone(), slug, id: None, last_opened, error: Some(e.to_string()) }
            }
        };
        entries.push(entry);
    }
    entries.sort_by(|a, b| b.last_opened.cmp(&a.last_opened).then_with(|| a.name.cmp(&b.name)));
    Ok(entries)
}

pub(crate) fn touch_last_opened(root: &Path) -> Result<(), ProjectError> {
    let path = local_file(root);
    let text = toml::to_string(&Local { last_opened: unix_now() }).unwrap_or_default();
    std::fs::write(&path, text).map_err(at(&path))
}

/// The picture a project shows in the library: a copy of one asset's
/// poster, made when the project is closed.
fn project_poster_file(root: &Path) -> PathBuf {
    root.join(".shotgun/poster.rgba")
}

/// Makes the poster of `asset` the picture of the project.
pub fn save_project_poster(root: &Path, asset: sg_core::AssetId) -> std::io::Result<()> {
    std::fs::copy(crate::layout::cache::poster(root, asset), project_poster_file(root)).map(|_| ())
}

/// The project's picture as raw RGBA, `sg_media::derive::POSTER_W` x `POSTER_H`.
pub fn read_project_poster(root: &Path) -> std::io::Result<Vec<u8>> {
    let bytes = std::fs::read(project_poster_file(root))?;
    if bytes.len() == (sg_media::derive::POSTER_W * sg_media::derive::POSTER_H * 4) as usize {
        Ok(bytes)
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "poster has the wrong size"))
    }
}
