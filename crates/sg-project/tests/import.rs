//! The import pipeline against real files made with ffmpeg. Skipped when
//! ffmpeg is not on the PATH (it always is in the dev shell).

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use sg_core::{AssetKind, Op};
use sg_project::{Derived, ImportEvent, ImportSource, Importer, cache, create, open};

fn ffmpeg(args: &[&str], out: &Path) -> bool {
    Command::new("ffmpeg").args(["-v", "error", "-y"]).args(args).arg(out).status().is_ok_and(|s| s.success())
}

/// Runs one import to the end and returns its events.
fn import(
    importer: &Importer,
    root: &Path,
    source: ImportSource,
    rx: &mpsc::Receiver<ImportEvent>,
) -> Vec<ImportEvent> {
    let known = open(root, || {}).unwrap().document.project().assets.clone();
    let job = importer.import(root.to_owned(), known, source);
    let mut events = Vec::new();
    loop {
        let event = rx.recv_timeout(Duration::from_secs(120)).expect("import did not finish");
        let done = matches!(event, ImportEvent::Finished { job: j } if j == job);
        events.push(event);
        if done {
            return events;
        }
    }
}

fn derived(events: &[ImportEvent]) -> Vec<Derived> {
    events.iter().filter_map(|e| if let ImportEvent::Derived { what, .. } = e { Some(*what) } else { None }).collect()
}

fn setup() -> Option<(tempfile::TempDir, PathBuf, Importer, mpsc::Receiver<ImportEvent>)> {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg not found: skipping");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = create(&dir.path().join("lib"), "Import").unwrap().path;
    let (tx, rx) = mpsc::channel();
    let importer = Importer::new(move |e| {
        let _ = tx.send(e);
    })
    .unwrap();
    Some((dir, root, importer, rx))
}

#[test]
fn video_with_audio_gets_every_derived_file() {
    let Some((dir, root, importer, rx)) = setup() else { return };
    let src = dir.path().join("My Clip.mp4");
    assert!(ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x360:r=30:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:d=2:r=44100",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest"
        ],
        &src
    ));
    let events = import(&importer, &root, ImportSource::Path(src.clone()), &rx);
    let asset = events
        .iter()
        .find_map(|e| if let ImportEvent::Usable { asset, .. } = e { Some(asset.clone()) } else { None })
        .unwrap();
    assert_eq!(asset.kind, AssetKind::Video);
    assert_eq!(asset.label, "My Clip");
    assert_eq!(asset.file, format!("assets/video/my-clip-{}.mp4", asset.id.short()));
    assert_eq!(asset.size, std::fs::metadata(&src).unwrap().len());
    assert_eq!(std::fs::read(root.join(&asset.file)).unwrap(), std::fs::read(&src).unwrap());
    assert_eq!(asset.blake3, blake3::hash(&std::fs::read(&src).unwrap()).to_hex().to_string());
    let v = asset.video.as_ref().unwrap();
    assert_eq!((v.width, v.height, v.frames, v.rate.to_string().as_str()), (640, 360, 60, "30/1"));
    assert_eq!(asset.audio.as_ref().unwrap().sample_rate, 44_100);

    let d = derived(&events);
    for what in [Derived::Audio, Derived::Peaks, Derived::Poster, Derived::Scan, Derived::Proxy] {
        assert!(d.contains(&what), "{what:?} missing from {d:?}");
    }
    assert!(!d.contains(&Derived::Conform), "a constant-rate file needs no conform");
    assert_eq!(std::fs::metadata(cache::poster(&root, asset.id)).unwrap().len(), 320 * 180 * 4);
    let peaks = sg_media::peaks::Peaks::read(&cache::peaks(&root, asset.id)).unwrap();
    assert_eq!((peaks.sample_rate, peaks.channels), (48_000, 1));
    assert!((peaks.samples as i64 - 96_000).abs() < 2_000, "{}", peaks.samples);
    assert_eq!(std::fs::read_to_string(cache::scan(&root, asset.id)).unwrap().split(' ').next(), Some("cfr"));
    assert!(cache::proxy(&root, asset.id).is_file());
    assert!(!events.iter().any(|e| matches!(e, ImportEvent::Failed { .. })));
    // No temporary files stay behind.
    let leftovers: Vec<_> = std::fs::read_dir(root.join("assets"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    importer.shutdown();
}

#[test]
fn variable_frame_rate_is_conformed() {
    let Some((dir, root, importer, rx)) = setup() else { return };
    let src = dir.path().join("vfr.mp4");
    // Drop every fifth frame and keep the original timestamps.
    assert!(ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x360:r=30:d=2",
            "-vf",
            "select='mod(n,5)'",
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p"
        ],
        &src
    ));
    let events = import(&importer, &root, ImportSource::Path(src), &rx);
    let asset = events
        .iter()
        .find_map(|e| if let ImportEvent::Usable { asset, .. } = e { Some(asset.clone()) } else { None })
        .unwrap();
    assert!(std::fs::read_to_string(cache::scan(&root, asset.id)).unwrap().starts_with("vfr"));
    let d = derived(&events);
    assert!(d.contains(&Derived::Conform) && d.contains(&Derived::Proxy), "{d:?}");
    assert!(cache::conform(&root, asset.id, false).is_file());
    importer.shutdown();
}

#[test]
fn stills_audio_duplicates_and_junk() {
    let Some((dir, root, importer, rx)) = setup() else { return };
    let png = dir.path().join("pic.png");
    let mp3 = dir.path().join("tone.mp3");
    assert!(ffmpeg(&["-f", "lavfi", "-i", "testsrc2=s=320x240:r=1:d=1", "-frames:v", "1"], &png));
    assert!(ffmpeg(&["-f", "lavfi", "-i", "sine=f=330:d=1:r=44100"], &mp3));
    let junk = dir.path().join("notes.txt");
    std::fs::write(&junk, "not media").unwrap();

    // A folder imports every file in it; the text file fails on its own.
    let folder = dir.path().join("drop");
    std::fs::create_dir(&folder).unwrap();
    for f in [&png, &mp3, &junk] {
        std::fs::copy(f, folder.join(f.file_name().unwrap())).unwrap();
    }
    let events = import(&importer, &root, ImportSource::Path(folder), &rx);
    let kinds: Vec<AssetKind> = events
        .iter()
        .filter_map(|e| if let ImportEvent::Usable { asset, .. } = e { Some(asset.kind) } else { None })
        .collect();
    assert_eq!(kinds, [AssetKind::Image, AssetKind::Audio]);
    let failed: Vec<&str> = events
        .iter()
        .filter_map(|e| if let ImportEvent::Failed { label, .. } = e { Some(label.as_str()) } else { None })
        .collect();
    assert_eq!(failed, ["notes"]);
    assert!(!derived(&events).contains(&Derived::Proxy));
    assert_eq!(
        derived(&events).iter().filter(|d| **d == Derived::Poster).count(),
        2,
        "the audio file gets a waveform poster"
    );

    // Register the image in the project, then import the same content again.
    let mut opened = open(&root, || {}).unwrap();
    let image = events.iter().find_map(|e| match e {
        ImportEvent::Usable { asset, .. } if asset.kind == AssetKind::Image => Some(asset.clone()),
        _ => None,
    });
    let image = image.unwrap();
    let record = opened.document.apply(Op::AddAsset { asset: image.clone() }, None).unwrap();
    opened.writer.record(record, opened.document.project().clone());
    opened.writer.close().unwrap().join().unwrap();
    assert!(root.join(format!("assets/index/{}.toml", image.id)).is_file());
    let again = import(&importer, &root, ImportSource::Path(png), &rx);
    assert!(again.iter().any(|e| matches!(e, ImportEvent::Duplicate { id, .. } if *id == image.id)));

    // Removing the asset moves its media to the trash; undo brings it back.
    let mut opened = open(&root, || {}).unwrap();
    assert_eq!(opened.document.project().assets.len(), 1);
    let record = opened.document.apply(Op::RemoveAsset { id: image.id }, None).unwrap();
    opened.writer.record(record, opened.document.project().clone());
    opened.writer.close().unwrap().join().unwrap();
    assert!(!root.join(&image.file).exists());
    let trashed = root.join(".shotgun/trash").join(Path::new(&image.file).file_name().unwrap());
    assert!(trashed.is_file());
    let mut opened = open(&root, || {}).unwrap();
    let record = opened.document.undo().unwrap().unwrap();
    opened.writer.record(record, opened.document.project().clone());
    opened.writer.close().unwrap().join().unwrap();
    assert!(root.join(&image.file).is_file() && !trashed.exists());
    importer.shutdown();
}

#[test]
fn pasted_image_bytes_become_an_asset() {
    let Some((dir, root, importer, rx)) = setup() else { return };
    let png = dir.path().join("p.png");
    assert!(ffmpeg(&["-f", "lavfi", "-i", "color=c=red:s=64x64:d=1", "-frames:v", "1"], &png));
    let data = std::fs::read(&png).unwrap();
    let events = import(&importer, &root, ImportSource::Bytes { data, mime: "image/png".into() }, &rx);
    let asset = events
        .iter()
        .find_map(|e| if let ImportEvent::Usable { asset, .. } = e { Some(asset.clone()) } else { None })
        .unwrap();
    assert_eq!((asset.kind, asset.source.origin.as_str()), (AssetKind::Image, "clipboard"));
    assert!(asset.file.starts_with("assets/image/pasted-"));
    let bad =
        import(&importer, &root, ImportSource::Bytes { data: vec![1, 2, 3], mime: "application/pdf".into() }, &rx);
    assert!(bad.iter().any(|e| matches!(e, ImportEvent::Failed { .. })));
    importer.shutdown();
}

#[test]
fn cache_is_rebuilt_and_missing_media_is_reported() {
    let Some((dir, root, importer, rx)) = setup() else { return };
    let png = dir.path().join("pic.png");
    assert!(ffmpeg(&["-f", "lavfi", "-i", "testsrc2=s=320x240:r=1:d=1", "-frames:v", "1"], &png));
    let events = import(&importer, &root, ImportSource::Path(png.clone()), &rx);
    let asset = events
        .iter()
        .find_map(|e| if let ImportEvent::Usable { asset, .. } = e { Some(asset.clone()) } else { None })
        .unwrap();
    let mut opened = open(&root, || {}).unwrap();
    let record = opened.document.apply(Op::AddAsset { asset: asset.clone() }, None).unwrap();
    opened.writer.record(record, opened.document.project().clone());
    let assets = opened.document.project().assets.clone();
    opened.writer.close().unwrap().join().unwrap();

    // As after unzipping a bundle without its cache.
    std::fs::remove_dir_all(root.join(".shotgun/cache/thumb")).unwrap();
    std::fs::create_dir_all(root.join(".shotgun/cache/thumb")).unwrap();
    let wait = |job: u64| -> Vec<ImportEvent> {
        let mut events = Vec::new();
        loop {
            let e = rx.recv_timeout(Duration::from_secs(60)).unwrap();
            let done = matches!(e, ImportEvent::Finished { job: j } if j == job);
            events.push(e);
            if done {
                return events;
            }
        }
    };
    let events = wait(importer.restore_cache(root.clone(), assets.clone()));
    assert_eq!(derived(&events), [Derived::Poster]);
    assert!(cache::poster(&root, asset.id).is_file());

    // The media file is gone but its origin still exists (the app was closed
    // while a dropped file was still being copied): it is fetched again.
    std::fs::remove_file(root.join(&asset.file)).unwrap();
    let events = wait(importer.restore_cache(root.clone(), assets.clone()));
    assert!(!events.iter().any(|e| matches!(e, ImportEvent::Missing { .. })));
    assert_eq!(std::fs::read(root.join(&asset.file)).unwrap(), std::fs::read(&png).unwrap());

    // Gone for good: reported.
    std::fs::remove_file(root.join(&asset.file)).unwrap();
    std::fs::remove_file(&png).unwrap();
    let events = wait(importer.restore_cache(root.clone(), assets));
    assert!(events.iter().any(|e| matches!(e, ImportEvent::Missing { id, .. } if *id == asset.id)));
    importer.shutdown();
}
