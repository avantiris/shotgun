//! Frame-accurate decoding against files made with ffmpeg: every frame of
//! the test pattern carries its own number as a block of grey levels.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sg_media::decode::VideoDecoder;

/// 120 frames at 30 fps, 320x180. Frame n writes its tens and units digits
/// as two grey blocks (luma 30 + 16 * digit), coarse enough to survive a
/// lossy codec, so a decoded frame identifies itself.
fn make(dir: &Path, name: &str, codec: &[&str]) -> Option<PathBuf> {
    let out = dir.join(name);
    let lum = "if(lt(Y,64)*lt(X,64),30+16*floor(N/10),if(lt(Y,64)*lt(X,128),30+16*mod(N,10),60+40*sin(X/9+N/5)))";
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=black:s=320x180:r=30:d=4"])
        .args(["-vf", &format!("geq=lum='{lum}':cb=128:cr=128")])
        .args(codec)
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(out)
}

fn number(frame: &sg_media::decode::VideoFrame) -> i64 {
    let digit = |x: usize| {
        // Green of an achromatic pixel; limited-range luma l is (l - 16) * 255 / 219.
        let g = frame.rgba[(32 * frame.width as usize + x) * 4 + 1] as f64;
        let luma = g * 219.0 / 255.0 + 16.0;
        ((luma - 30.0) / 16.0).round() as i64
    };
    digit(32) * 10 + digit(96)
}

fn check_file(path: &Path, threads: i32) {
    let mut dec = VideoDecoder::open(path, threads).unwrap();
    assert_eq!(dec.size(), (320, 180));
    // Sequential.
    for i in [8i64, 9, 10, 11] {
        let f = dec.frame(i).unwrap();
        assert_eq!((f.index, number(&f)), (i, i), "sequential {i}");
    }
    // Backwards, forwards, far jumps, repeated.
    for i in [60i64, 59, 10, 100, 100, 37, 38, 39, 12, 119, 0, 64, 20, 19, 18] {
        let f = dec.frame(i).unwrap();
        assert!((number(&f) - i).abs() <= 0, "seek to {i} gave frame {}", number(&f));
        assert_eq!(f.rgba.len(), 320 * 180 * 4);
    }
    // Past the end: the last frame is held.
    let f = dec.frame(500).unwrap();
    assert_eq!(number(&f), 119);
}

#[test]
fn intra_proxy_long_gop_and_open_gop() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg not found: skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let x264 = ["-pix_fmt", "yuv420p", "-c:v", "libx264", "-preset", "veryfast"];
    let cases: [(&str, Vec<&str>, i32); 4] = [
        ("intra.mp4", [&x264[..], &["-g", "1", "-tune", "fastdecode"]].concat(), 1),
        ("longgop.mp4", [&x264[..], &["-g", "60", "-bf", "3"]].concat(), 0),
        (
            "opengop.mp4",
            vec!["-pix_fmt", "yuv420p", "-c:v", "libx265", "-x265-params", "keyint=30:open-gop=1:log-level=error"],
            0,
        ),
        ("alpha.mkv", vec!["-c:v", "utvideo", "-pix_fmt", "gbrap"], 1),
    ];
    for (name, codec, threads) in cases {
        let Some(path) = make(dir.path(), name, &codec) else {
            eprintln!("could not encode {name}: skipping it");
            continue;
        };
        check_file(&path, threads);
    }
}

#[test]
fn not_a_video() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.mp4");
    std::fs::write(&path, b"not a video").unwrap();
    assert!(VideoDecoder::open(&path, 1).is_err());
    assert!(VideoDecoder::open(&dir.path().join("missing.mp4"), 1).is_err());
}

/// Diagnosis of the slow export (08-milestones.md, 2026-11-20): straight
/// through a file, as an export reads it.
#[test]
#[ignore = "needs /disk/shotgun-dev/media/m2/av.mp4"]
fn straight_through_a_file_as_an_export_reads_it() {
    let path = std::path::Path::new("/disk/shotgun-dev/media/m2/av.mp4");
    let mut decoder = VideoDecoder::open(path, 0).unwrap();
    decoder.set_rate(30, 1);
    decoder.set_max_dim(8192);
    decoder.set_planes(true);
    let start = std::time::Instant::now();
    for index in 0..120 {
        decoder.frame(index).unwrap();
    }
    let ms = start.elapsed().as_secs_f64() * 1000.0 / 120.0;
    eprintln!("{ms:.2} ms per frame, {} seeks", decoder.seeks());
    assert!(decoder.seeks() <= 1, "{} seeks", decoder.seeks());
}
