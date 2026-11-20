//! The global assets (feedback F7.3): media kept outside any project so
//! that it can be used in all of them (memes, jingles, logos).
//!
//! Nothing is linked. "Make global" copies an asset's file out of its
//! project into this folder; using a global asset imports that copy into
//! the open project like any dropped file. Projects stay whole on their own.
//!
//! The folder is `_global` in the library (`PROPOSED`: the user asked for a
//! folder under the home directory, whose disk is nearly full; the library
//! is on the large disk, and the scan for projects skips a folder without
//! a `project.toml`). `SHOTGUN_GLOBAL` names another place.
//!
//! Layout: `<kind>/<label>.<ext>`, and beside it `<label>.<ext>.rgba`, the
//! poster as the asset panel draws it. All calls do file I/O: job threads only.

use std::io;
use std::path::{Path, PathBuf};

use sg_core::{Asset, AssetKind};

use crate::layout::cache;

const KINDS: [AssetKind; 3] = [AssetKind::Video, AssetKind::Audio, AssetKind::Image];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalEntry {
    /// The file to import; its name is unique within its kind.
    pub path: PathBuf,
    pub kind: AssetKind,
    /// File name without the extension.
    pub label: String,
    pub bytes: u64,
}

/// Where the global assets of a library are kept.
pub fn dir(library: &Path) -> PathBuf {
    std::env::var_os("SHOTGUN_GLOBAL").map_or_else(|| library.join("_global"), PathBuf::from)
}

/// The file name an asset gets here: its label, made safe, with the
/// extension of its file.
pub fn file_name(asset: &Asset) -> String {
    let safe: String = asset
        .label
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == '\0' || c.is_control() { '_' } else { c })
        .collect();
    let safe = safe.trim().trim_start_matches('.');
    let stem = if safe.is_empty() { "asset" } else { safe };
    match Path::new(&asset.file).extension() {
        Some(extension) => format!("{stem}.{}", extension.to_string_lossy()),
        None => stem.to_owned(),
    }
}

fn poster_of(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".rgba");
    PathBuf::from(name)
}

/// Everything in the folder, by label. A missing folder is an empty list.
pub fn list(dir: &Path) -> io::Result<Vec<GlobalEntry>> {
    let mut entries = Vec::new();
    for kind in KINDS {
        let files = match std::fs::read_dir(dir.join(kind.dir())) {
            Ok(files) => files,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for file in files.flatten() {
            let path = file.path();
            let Ok(meta) = file.metadata() else { continue };
            // (Posters and half-written copies are not assets.)
            if !meta.is_file() || path.extension().is_some_and(|e| e == "rgba" || e == "part") {
                continue;
            }
            let label = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            entries.push(GlobalEntry { path, kind, label, bytes: meta.len() });
        }
    }
    entries.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()).then_with(|| a.path.cmp(&b.path)));
    Ok(entries)
}

/// Copies an asset of the project at `root` into the folder, poster
/// included. An entry of the same name and kind is replaced.
pub fn add(dir: &Path, root: &Path, asset: &Asset) -> io::Result<PathBuf> {
    let folder = dir.join(asset.kind.dir());
    std::fs::create_dir_all(&folder)?;
    let to = folder.join(file_name(asset));
    // Under another name first: a copy that fails half way is never listed as an asset.
    let part = folder.join(format!("{}.part", file_name(asset)));
    let copied = std::fs::copy(root.join(&asset.file), &part).and_then(|_| std::fs::rename(&part, &to));
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    // (An asset without a poster yet, or a sound, is shown by its name alone.)
    let _ = std::fs::copy(cache::poster(root, asset.id), poster_of(&to));
    Ok(to)
}

/// Takes an entry out of the folder. Projects that used it keep their copies.
pub fn remove(dir: &Path, file: &Path) -> io::Result<()> {
    // Only what lies in the folder is ever deleted.
    if !file.starts_with(dir) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a global asset"));
    }
    std::fs::remove_file(file)?;
    let _ = std::fs::remove_file(poster_of(file));
    Ok(())
}

/// The poster of an entry (raw RGBA), if it has one.
pub fn read_poster(file: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(poster_of(file))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn asset(label: &str, file: &str, kind: AssetKind) -> Asset {
        Asset {
            id: sg_core::AssetId(7),
            blake3: String::new(),
            size: 5,
            file: file.to_owned(),
            kind,
            label: label.to_owned(),
            tags: vec![],
            source: sg_core::Source { origin: "test".into(), imported: 0, uploader: None },
            video: None,
            audio: None,
        }
    }

    #[test]
    fn an_asset_is_copied_listed_and_removed() {
        let temp = tempfile::tempdir().unwrap();
        let (root, store) = (temp.path().join("project"), temp.path().join("_global"));
        std::fs::create_dir_all(root.join("assets/video")).unwrap();
        std::fs::write(root.join("assets/video/cat-12345678.mp4"), b"hello").unwrap();
        // Nothing there yet.
        assert_eq!(list(&store).unwrap(), vec![]);
        let cat = asset("Funny cat: take/2", "assets/video/cat-12345678.mp4", AssetKind::Video);
        let poster = cache::poster(&root, cat.id);
        std::fs::create_dir_all(poster.parent().unwrap()).unwrap();
        std::fs::write(&poster, b"picture").unwrap();
        let file = add(&store, &root, &cat).unwrap();
        // Named by its label, a copy (the project keeps its file), with the poster beside it.
        assert_eq!(file, store.join("video/Funny cat: take_2.mp4"));
        assert!(root.join(&cat.file).is_file());
        assert_eq!(read_poster(&file).unwrap(), b"picture");
        let listed = list(&store).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            (listed[0].label.as_str(), listed[0].kind, listed[0].bytes),
            ("Funny cat: take_2", AssetKind::Video, 5)
        );
        // Again: replaced, not doubled.
        add(&store, &root, &cat).unwrap();
        assert_eq!(list(&store).unwrap().len(), 1);
        // Only files of the folder can be removed.
        assert!(remove(&store, &root.join(&cat.file)).is_err());
        remove(&store, &file).unwrap();
        assert_eq!(list(&store).unwrap(), vec![]);
        assert!(root.join(&cat.file).is_file());
    }
}
