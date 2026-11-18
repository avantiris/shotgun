//! Reproducible benchmarks for the budgets in docs/spec/04-performance.md.
//!
//!   sg-bench ops [clips]                 edit-op apply time on a synthetic project
//!   sg-bench journal <dir> [records]     journal append latency on a real disk
//!   sg-bench gen <library> <name> [clips]  write a synthetic project through the real edit path
//!   sg-bench open <project-dir> [reps]   time to open a bundle (files + journal + history)

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use sg_core::model::{TrackHeader, TrackKind};
use sg_core::{AssetId, Clip, Document, History, IdGen, Op, Project, SequenceId, TrackId, apply};

const TRACKS: usize = 20;

struct Samples(Vec<u64>);

impl Samples {
    fn report(mut self, name: &str, budget_us: f64) {
        self.0.sort_unstable();
        let at = |q: f64| self.0[((self.0.len() - 1) as f64 * q).round() as usize] as f64 / 1000.0;
        let (p50, p99, max) = (at(0.5), at(0.99), at(1.0));
        let verdict = if p99 <= budget_us { "pass" } else { "MISS" };
        println!(
            "{name:<22} n={:<6} p50 {p50:>8.2} us  p99 {p99:>8.2} us  max {max:>9.2} us  budget {budget_us:.0} us p99: {verdict}",
            self.0.len()
        );
    }
}

/// Clip `i` of every synthetic track: 2 s long with a 1 s gap after it, so
/// that inserts and moves have room.
fn synthetic_clip(ids: &mut IdGen, i: usize) -> Clip {
    Clip { id: ids.clip(), asset: AssetId(ids.next_u64() as u128), start: i as i64 * 180, len: 120, src_in: 0 }
}

/// Adds tracks until the first sequence has `count` of them.
fn add_tracks(mut project: Arc<Project>, ids: &mut IdGen, count: usize) -> anyhow::Result<(Arc<Project>, Vec<Op>)> {
    let seq = project.sequences.first().context("project has no sequence")?.id;
    let mut ops = Vec::new();
    while project.sequences[0].tracks.len() < count {
        let n = project.sequences[0].tracks.len();
        let kind = if n.is_multiple_of(2) { TrackKind::Video } else { TrackKind::Audio };
        let op =
            Op::AddTrack { seq, index: n, track: TrackHeader { id: ids.track(), kind, name: format!("T{}", n + 1) } };
        project = apply(&project, &op)?.0;
        ops.push(op);
    }
    Ok((project, ops))
}

fn empty_project(ids: &mut IdGen) -> anyhow::Result<Arc<Project>> {
    let seq = ids.sequence();
    let header = sg_core::SequenceHeader {
        id: seq,
        name: "Main".into(),
        rate: sg_core::Rate::FPS_60,
        width: 1920,
        height: 1080,
    };
    let project = Arc::new(Project::new(ids.project(), "bench"));
    Ok(apply(&project, &Op::AddSequence { index: 0, sequence: header })?.0)
}

fn bench_ops(clips: usize) -> anyhow::Result<()> {
    let mut ids = IdGen::new(1);
    let (mut project, _) = add_tracks(empty_project(&mut ids)?, &mut ids, TRACKS)?;
    let seq: SequenceId = project.sequences[0].id;
    let tracks: Vec<TrackId> = project.sequences[0].tracks.iter().map(|t| t.id).collect();
    let per_track = clips / TRACKS;
    let build = Instant::now();
    for &track in &tracks {
        for i in 0..per_track {
            project = apply(&project, &Op::InsertClip { seq, track, clip: synthetic_clip(&mut ids, i) })?.0;
        }
    }
    println!(
        "project: {} clips on {TRACKS} tracks, built with {} inserts in {:.0} ms",
        project.clip_count(),
        clips,
        build.elapsed().as_secs_f64() * 1000.0
    );

    let mut doc = Document::new(project, History::default());
    let n = 20_000;
    let timed = |name: &str, doc: &mut Document, make: &mut dyn FnMut(&Document, usize) -> Op| -> anyhow::Result<()> {
        let (mut forward, mut undo) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for k in 0..n {
            let op = make(doc, k);
            let t = Instant::now();
            doc.apply(op, None)?;
            forward.push(t.elapsed().as_nanos() as u64);
            // Undo so that every iteration meets the same project.
            let t = Instant::now();
            doc.undo()?;
            undo.push(t.elapsed().as_nanos() as u64);
        }
        Samples(forward).report(name, 1000.0);
        Samples(undo).report(&format!("  undo {name}"), 1000.0);
        Ok(())
    };
    // Deterministic spread over tracks and positions.
    let pick = |doc: &Document, k: usize| -> (TrackId, Clip) {
        let track = &doc.project().sequences[0].tracks[k % TRACKS];
        let i = (k * 7919) % per_track;
        let clip = track.clips.get(i as i64 * 180).unwrap_or_else(|| unreachable!("synthetic clip {i} exists"));
        (track.id, clip)
    };
    let mut ids2 = IdGen::new(2);
    timed("split", &mut doc, &mut |d, k| {
        let (track, clip) = pick(d, k);
        Op::split(seq, track, &clip, clip.start + 60, ids2.clip()).unwrap_or_else(|| unreachable!("60 is inside 120"))
    })?;
    timed("trim", &mut doc, &mut |d, k| {
        let (track, clip) = pick(d, k);
        Op::SetClip {
            seq,
            track,
            start: clip.start,
            clip: Clip { start: clip.start + 10, len: 100, src_in: 10, ..clip },
        }
    })?;
    timed("move in track", &mut doc, &mut |d, k| {
        let (track, clip) = pick(d, k);
        Op::SetClip { seq, track, start: clip.start, clip: Clip { start: clip.start + 30, ..clip } }
    })?;
    timed("move to other track", &mut doc, &mut |d, k| {
        let (track, clip) = pick(d, k);
        let other = d.project().sequences[0].tracks[(k + 1) % TRACKS].id;
        Op::Batch(vec![
            Op::RemoveClip { seq, track, start: clip.start },
            Op::InsertClip { seq, track: other, clip: Clip { start: clip.start + 125, len: 50, ..clip } },
        ])
    })?;
    timed("insert", &mut doc, &mut |d, k| {
        let (track, clip) = pick(d, k);
        Op::InsertClip { seq, track, clip: Clip { id: ids2.clip(), start: clip.end() + 5, len: 40, ..clip } }
    })?;
    timed("ripple delete", &mut doc, &mut |d, k| {
        let (track, clip) = pick(d, k);
        Op::ripple_delete(seq, track, &clip)
    })?;
    timed("ripple all tracks", &mut doc, &mut |d, k| {
        let (_, clip) = pick(d, k);
        Op::Batch(tracks.iter().map(|&track| Op::ShiftFrom { seq, track, from: clip.end(), delta: 60 }).collect())
    })?;
    Ok(())
}

fn bench_journal(dir: &Path, records: usize) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("bench-journal.log");
    let _ = std::fs::remove_file(&path);
    let (mut journal, _) = sg_project::Journal::open(&path)?;
    let mut ids = IdGen::new(3);
    let (seq, track) = (ids.sequence(), ids.track());
    let mut samples = Vec::with_capacity(records);
    let mut sync_samples = Vec::new();
    let mut last_sync = Instant::now();
    for i in 0..records {
        let clip = synthetic_clip(&mut ids, i);
        let op = Op::split(seq, track, &clip, clip.start + 60, ids.clip()).context("split")?;
        let record = sg_core::Record::Do { inv: op.clone(), op, group: None };
        let t = Instant::now();
        journal.append(&record)?;
        samples.push(t.elapsed().as_nanos() as u64);
        // The writer thread syncs once a second; emulate an editing pace of
        // 1000 ops/s so that syncs fall between appends as they do in the app.
        if i % 1000 == 999 {
            std::thread::sleep(Duration::from_millis(100));
        }
        if last_sync.elapsed() >= Duration::from_secs(1) {
            let t = Instant::now();
            journal.sync()?;
            sync_samples.push(t.elapsed().as_nanos() as u64);
            last_sync = Instant::now();
        }
    }
    let bytes = std::fs::metadata(&path)?.len();
    println!("journal: {records} records, {bytes} bytes ({} per record) in {}", bytes / records as u64, path.display());
    Samples(samples).report("append", 1000.0);
    if !sync_samples.is_empty() {
        Samples(sync_samples).report("fsync (writer thread)", f64::INFINITY);
    }
    std::fs::remove_file(&path)?;
    Ok(())
}

fn generate(library: &Path, name: &str, clips: usize, tracks: usize) -> anyhow::Result<PathBuf> {
    let created = sg_project::create(library, name)?;
    let mut opened = sg_project::open(&created.path, || {})?;
    let mut ids = IdGen::new(sg_project::random_seed());
    let apply_op = |doc: &mut Document, op: Op| -> anyhow::Result<()> {
        let record = doc.apply(op, None)?;
        opened.writer.record(record, doc.project().clone());
        Ok(())
    };
    let (_, track_ops) = add_tracks(opened.document.project().clone(), &mut ids, tracks.max(2))?;
    for op in track_ops {
        apply_op(&mut opened.document, op)?;
    }
    let seq = opened.document.project().sequences[0].id;
    let tracks: Vec<TrackId> = opened.document.project().sequences[0].tracks.iter().map(|t| t.id).collect();
    let start = Instant::now();
    for i in 0..clips / tracks.len() {
        for &track in &tracks {
            apply_op(&mut opened.document, Op::InsertClip { seq, track, clip: synthetic_clip(&mut ids, i) })?;
        }
    }
    let applied = start.elapsed();
    if let Some(thread) = opened.writer.close() {
        thread.join().map_err(|_| anyhow::anyhow!("writer thread panicked"))?;
    }
    println!(
        "{}: {} clips; edits applied in {:.0} ms, journalled and saved after {:.0} ms",
        created.path.display(),
        opened.document.project().clip_count(),
        applied.as_secs_f64() * 1000.0,
        start.elapsed().as_secs_f64() * 1000.0
    );
    Ok(created.path)
}

fn bench_open(root: &Path, reps: usize) -> anyhow::Result<()> {
    let mut samples = Vec::with_capacity(reps);
    let mut info = String::new();
    for _ in 0..reps {
        let t = Instant::now();
        let opened = sg_project::open(root, || {})?;
        samples.push(t.elapsed().as_nanos() as u64);
        info = format!(
            "{} clips, {} undo steps, {} journal bytes",
            opened.document.project().clip_count(),
            opened.document.history().undo_len(),
            std::fs::metadata(root.join(".shotgun/journal/journal.log")).map_or(0, |m| m.len())
        );
        if let Some(thread) = opened.writer.close() {
            let _ = thread.join();
        }
    }
    println!("open {}: {info}", root.display());
    // The app adds its own frame on top; see `shotgun --bench-open`.
    Samples(samples).report("open bundle", 500_000.0);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    let number = |i: usize, default: usize| {
        arg(i).map_or(Ok(default), |s| s.parse().with_context(|| format!("bad number {s:?}")))
    };
    match arg(0) {
        Some("ops") => bench_ops(number(1, 50_000)?),
        Some("journal") => bench_journal(Path::new(arg(1).context("journal <dir>")?), number(2, 50_000)?),
        Some("gen") => {
            let path = generate(
                Path::new(arg(1).context("gen <library> <name>")?),
                arg(2).context("gen <library> <name>")?,
                number(3, 50_000)?,
                number(4, TRACKS)?,
            )?;
            println!("folder: {}", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default());
            Ok(())
        }
        Some("open") => bench_open(Path::new(arg(1).context("open <project-dir>")?), number(2, 10)?),
        // sg-bench seek <file> [threads] [skip 0/1] [max dimension]
        Some("seek") => {
            let threads = arg(2).map_or(Ok(0), |s| if s == "vaapi" { Ok(-1) } else { s.parse::<i32>() })?;
            bench_seek(Path::new(arg(1).context("seek <file>")?), threads, number(3, 0)? != 0, number(4, 4096)? as u32)
        }
        // sg-bench audio <sink name or "default"> [secs]
        Some("audio") => {
            let target = arg(1).context("audio <sink name|default> [secs]")?;
            bench_audio(
                (target != "default").then_some(target),
                number(2, 10)? as u64,
                Path::new("/disk/shotgun-dev/tmp"),
            )
        }
        _ => bail!(
            "usage: sg-bench ops [clips] | journal <dir> [records] | gen <library> <name> [clips] | open <project-dir> [reps]"
        ),
    }
}

/// Plays a generated tone through PipeWire and reports what the engine
/// measured. `target` names the sink; use a null sink for automated runs.
fn bench_audio(target: Option<&str>, secs: u64, dir: &Path) -> anyhow::Result<()> {
    use sg_audio::{Backend, ClipSpan, DEFAULT_QUANTUM, Engine, Mix, SAMPLE_RATE, TrackMix};
    std::fs::create_dir_all(dir)?;
    let wav = dir.join("bench-tone.wav");
    let frames = SAMPLE_RATE as usize * (secs as usize + 2);
    let mut bytes = Vec::with_capacity(44 + frames * 8);
    let data = (frames * 8) as u32;
    bytes.extend(b"RIFF");
    bytes.extend((36 + data).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(3u16.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(SAMPLE_RATE.to_le_bytes());
    bytes.extend((SAMPLE_RATE * 8).to_le_bytes());
    bytes.extend(8u16.to_le_bytes());
    bytes.extend(32u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data.to_le_bytes());
    for n in 0..frames {
        let s = (n as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 0.05;
        bytes.extend(s.to_le_bytes());
        bytes.extend(s.to_le_bytes());
    }
    std::fs::write(&wav, bytes)?;

    let engine = Engine::start(Backend::PipeWire { target: target.map(str::to_owned), quantum: DEFAULT_QUANTUM })?;
    let clip = ClipSpan { start: 0, len: frames as i64, src_in: 0, wav: wav.clone(), gain: 1.0 };
    engine.set_mix(Mix { tracks: vec![TrackMix { clips: vec![clip], gain: 1.0, pan: 0.0, mute: false }] });

    // Play start: from the call until the realtime thread renders the first block.
    let mut starts = Vec::new();
    for round in 0..10 {
        let t = Instant::now();
        engine.play(0);
        while !engine.is_playing() {
            if t.elapsed() > Duration::from_secs(3) {
                bail!("the audio stream did not start (is the target sink there?)");
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        starts.push(t.elapsed().as_nanos() as u64);
        std::thread::sleep(Duration::from_millis(if round == 9 { 0 } else { 150 }));
        if round < 9 {
            engine.pause();
            std::thread::sleep(Duration::from_millis(150));
        }
    }
    Samples(starts).report("audio start", 80_000.0);

    // Clock: the heard position must advance at one sample per 1/48000 s.
    let t0 = Instant::now();
    let p0 = engine.position();
    let mut worst = 0.0f64;
    while t0.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(50));
        let expected = p0 as f64 + t0.elapsed().as_secs_f64() * SAMPLE_RATE as f64;
        worst = worst.max((engine.position() as f64 - expected).abs());
    }
    let stats = engine.stats();
    engine.pause();
    std::thread::sleep(Duration::from_millis(50));
    let stats_after = engine.stats();
    println!(
        "pipewire stream: quantum {} frames ({:.2} ms), reported output delay {} frames ({:.2} ms)",
        stats.quantum,
        stats.quantum as f64 * 1000.0 / SAMPLE_RATE as f64,
        stats.delay_frames,
        stats.delay_frames as f64 * 1000.0 / SAMPLE_RATE as f64
    );
    println!(
        "over {secs} s: {} callbacks, graph xruns {}, ring underruns {}, worst process() {:.1} us, realtime policy {} (2 = SCHED_RR)",
        stats.callbacks,
        stats.xruns,
        stats.underruns,
        stats.process_max_ns as f64 / 1000.0,
        stats_after.sched_policy
    );
    println!(
        "clock: heard position stayed within {:.0} samples ({:.2} ms) of wall-clock time",
        worst,
        worst * 1000.0 / SAMPLE_RATE as f64
    );
    std::fs::remove_file(&wav)?;
    Ok(())
}

/// Random seeks in one file with the in-process decoder.
fn bench_seek(path: &Path, threads: i32, skip_nonref: bool, max_dim: u32) -> anyhow::Result<()> {
    let mut decoder = sg_media::decode::VideoDecoder::open_with(path, threads.max(0), threads < 0)?;
    decoder.set_skip_nonref(skip_nonref);
    decoder.set_max_dim(max_dim);
    let probed = sg_media::probe(path, &sg_media::Cancel::new())?;
    let frames = probed.video.map_or(600, |v| v.frames).max(2);
    let mut samples = Vec::new();
    let mut frame = 37i64;
    for _ in 0..60 {
        frame = (frame * 7919 + 1031) % frames;
        // SG_SEEK_GAP_MS: idle time between seeks, as when a person clicks around.
        if let Some(gap) = std::env::var("SG_SEEK_GAP_MS").ok().and_then(|g| g.parse().ok()) {
            std::thread::sleep(Duration::from_millis(gap));
        }
        let t = Instant::now();
        decoder.frame(frame)?;
        samples.push(t.elapsed().as_nanos() as u64);
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let how = if threads < 0 { "vaapi".to_owned() } else { format!("sw t={threads}") };
    Samples(samples).report(&format!("{name} {how} skip={}", skip_nonref as u8), 150_000.0);
    Ok(())
}
