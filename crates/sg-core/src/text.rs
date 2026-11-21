//! The text form of a project: `project.toml` plus one `.sg` file per
//! sequence. Line-oriented with a stable order, one clip per line, so git
//! diffs show exactly what an edit changed.
//!
//! This module only converts between a snapshot and file contents; reading
//! and writing the files is `sg-project`'s job.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::asset::Asset;
use crate::clips::{Clip, ClipStore};
use crate::colour::Scenario;
use crate::ids::ProjectId;
use crate::model::{Marker, Project, Sequence, Song, Track, TrackHeader, TrackKind, TrackSettings};
use crate::props::ClipProps;

pub const FORMAT_VERSION: u32 = 1;
pub const PROJECT_FILE: &str = "project.toml";
const SEQUENCE_MAGIC: &str = "shotgun-sequence";
/// Asset index files live here, one per asset.
pub const ASSET_INDEX_DIR: &str = "assets/index/";
/// One file per scenario: `scenarios/<id>.toml`.
pub const SCENARIO_DIR: &str = "scenarios/";

/// An asset file must stay inside `assets/`: no absolute path, no `..`.
fn is_bundle_path(path: &str) -> bool {
    path.starts_with("assets/") && !path.split('/').any(|part| part.is_empty() || part == "." || part == "..")
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TextError {
    #[error("{file}: {message}")]
    File { file: String, message: String },
    #[error("{file}:{line}: {message}")]
    Line { file: String, line: usize, message: String },
    #[error("{file} is missing")]
    Missing { file: String },
    #[error("{file} has format version {found}, this build reads up to {FORMAT_VERSION}")]
    Version { file: String, found: u32 },
}

#[derive(Serialize, Deserialize)]
struct ProjectToml {
    format: u32,
    id: ProjectId,
    name: String,
    /// File stems under `sequences/`, in project order.
    #[serde(default)]
    sequences: Vec<String>,
    /// The project's words (absent while there are none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    lyrics: String,
}

/// Name and id from a `project.toml`, without touching sequences. Used by
/// the library view.
pub fn project_header(toml_text: &str) -> Result<(ProjectId, String), TextError> {
    let header = parse_project_toml(toml_text)?;
    Ok((header.id, header.name))
}

/// Paths of the sequence files a `project.toml` lists, relative to the bundle
/// root, in project order.
pub fn sequence_paths(toml_text: &str) -> Result<Vec<String>, TextError> {
    let header = parse_project_toml(toml_text)?;
    Ok(header.sequences.iter().map(|stem| sequence_path(stem)).collect())
}

fn sequence_path(stem: &str) -> String {
    format!("sequences/{stem}.sg")
}

fn parse_project_toml(text: &str) -> Result<ProjectToml, TextError> {
    let file = || PROJECT_FILE.to_owned();
    let header: ProjectToml =
        toml::from_str(text).map_err(|e| TextError::File { file: file(), message: e.message().to_owned() })?;
    if header.format > FORMAT_VERSION {
        return Err(TextError::Version { file: file(), found: header.format });
    }
    // The names become file paths: accept only what `slug` produces, so a
    // crafted project.toml cannot point outside the bundle.
    if let Some(bad) = header.sequences.iter().find(|s| slug(s) != **s) {
        return Err(TextError::File { file: file(), message: format!("invalid sequence name {bad:?}") });
    }
    Ok(header)
}

/// Turns a name into a file-system-safe slug: lowercase ASCII letters, digits
/// and single dashes.
pub fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        let mapped: &str = match ch {
            'ä' | 'Ä' => "ae",
            'ö' | 'Ö' => "oe",
            'ü' | 'Ü' => "ue",
            'ß' => "ss",
            'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' => "e",
            'à' | 'â' | 'á' | 'À' => "a",
            'ç' | 'Ç' => "c",
            'î' | 'ï' | 'í' => "i",
            'ô' | 'ó' => "o",
            'û' | 'ù' | 'ú' => "u",
            c if c.is_ascii_alphanumeric() => {
                out.push(c.to_ascii_lowercase());
                continue;
            }
            _ => "-",
        };
        if mapped != "-" || !(out.is_empty() || out.ends_with('-')) {
            out.push_str(mapped);
        }
    }
    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() { "untitled".to_owned() } else { trimmed.to_owned() }
}

/// Returns `base`, or `base-2`, `base-3`, ... if `taken` says it is in use.
pub fn unique_slug(base: &str, mut taken: impl FnMut(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_owned();
    }
    (2..).map(|n| format!("{base}-{n}")).find(|s| !taken(s)).unwrap_or_else(|| base.to_owned())
}

/// The complete set of tracked text files for a snapshot, keyed by path
/// relative to the bundle root. A pure function of the snapshot.
pub fn to_files(p: &Project) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let mut stems: Vec<String> = Vec::with_capacity(p.sequences.len());
    for seq in &p.sequences {
        let stem = unique_slug(&slug(&seq.name), |s| stems.iter().any(|x| x == s));
        files.insert(sequence_path(&stem), write_sequence(seq));
        stems.push(stem);
    }
    let header = ProjectToml {
        format: FORMAT_VERSION,
        id: p.id,
        name: p.name.clone(),
        sequences: stems,
        lyrics: p.lyrics.to_string(),
    };
    // Serialising a struct of strings and integers cannot fail.
    let text = toml::to_string(&header).unwrap_or_default();
    files.insert(PROJECT_FILE.to_owned(), text);
    for (id, asset) in p.assets.iter() {
        // Plain strings, integers and tables: serialising cannot fail.
        files.insert(Asset::index_path(*id), toml::to_string(&**asset).unwrap_or_default());
    }
    for (id, scenario) in p.scenarios.iter() {
        files.insert(format!("{SCENARIO_DIR}{id}.toml"), toml::to_string(&**scenario).unwrap_or_default());
    }
    files
}

pub fn from_files(files: &BTreeMap<String, String>) -> Result<Project, TextError> {
    let text = files.get(PROJECT_FILE).ok_or_else(|| TextError::Missing { file: PROJECT_FILE.to_owned() })?;
    let header = parse_project_toml(text)?;
    let mut sequences = Vec::with_capacity(header.sequences.len());
    for stem in &header.sequences {
        let file = sequence_path(stem);
        let text = files.get(&file).ok_or_else(|| TextError::Missing { file: file.clone() })?;
        sequences.push(Arc::new(parse_sequence(&file, text)?));
    }
    let mut assets = BTreeMap::new();
    for (file, text) in files.range(ASSET_INDEX_DIR.to_owned()..).take_while(|(k, _)| k.starts_with(ASSET_INDEX_DIR)) {
        let asset: Asset = toml::from_str(text)
            .map_err(|e| TextError::File { file: file.clone(), message: e.message().to_owned() })?;
        if *file != Asset::index_path(asset.id) || !is_bundle_path(&asset.file) {
            return Err(TextError::File {
                file: file.clone(),
                message: "asset id or file path is not valid".to_owned(),
            });
        }
        assets.insert(asset.id, Arc::new(asset));
    }
    let mut scenarios = BTreeMap::new();
    for (file, text) in files.range(SCENARIO_DIR.to_owned()..).take_while(|(k, _)| k.starts_with(SCENARIO_DIR)) {
        let scenario: Scenario = toml::from_str(text)
            .map_err(|e| TextError::File { file: file.clone(), message: e.message().to_owned() })?;
        scenarios.insert(scenario.id, Arc::new(scenario.clamped()));
    }
    Ok(Project {
        id: header.id,
        name: header.name,
        sequences,
        assets: Arc::new(assets),
        scenarios: Arc::new(scenarios),
        lyrics: Arc::from(header.lyrics.as_str()),
    })
}

fn write_sequence(seq: &Sequence) -> String {
    let mut out = String::with_capacity(256 + seq.clip_count() * 80);
    // Writing to a String cannot fail.
    let _ = writeln!(out, "{SEQUENCE_MAGIC} {FORMAT_VERSION}");
    let _ = writeln!(out, "id {}", seq.id);
    let _ = writeln!(out, "name {}", quote(&seq.name));
    let _ = writeln!(out, "rate {}", seq.rate);
    let _ = writeln!(out, "size {}x{}", seq.width, seq.height);
    if let Some(song) = seq.song {
        let _ = writeln!(out, "song {} {}", song.asset, song.tempo);
    }
    if seq.master_db != 0.0 {
        let _ = writeln!(out, "master {}", seq.master_db);
    }
    if let Some(tempo) = seq.tempo {
        let origin = if tempo.origin != 0 { format!(" {}", tempo.origin) } else { String::new() };
        let _ = writeln!(out, "tempo {} {}{origin}", tempo.milli_bpm, tempo.beats_per_bar);
    }
    for (n, group) in seq.groups.iter().enumerate() {
        if *group != crate::model::Group::default() {
            let _ = writeln!(out, "group {n} {} {}", group.gain_db, group.mute as u8);
        }
    }
    if seq.space != crate::model::Space::default() {
        let space = seq.space;
        let _ = writeln!(out, "space {} {} {}", space.room_ds, space.echo_note.label(), space.echo_repeat);
    }
    for marker in seq.markers.iter() {
        let _ = writeln!(out, "marker {} {}", marker.frame, quote(&marker.label));
    }
    for track in &seq.tracks {
        let kind = match track.kind {
            TrackKind::Video => "video",
            TrackKind::Audio => "audio",
        };
        let _ = writeln!(out, "\ntrack {} {kind} {}", track.id, quote(&track.name));
        if !track.mix.is_default() {
            let _ = writeln!(out, "mix {}", serde_json::to_string(&track.mix).unwrap_or_default());
        }
        for plugin in track.plugins.iter() {
            let _ = writeln!(out, "plugin {}", serde_json::to_string(plugin).unwrap_or_default());
        }
        for (key, asset) in track.pads.iter() {
            let _ = writeln!(out, "pad {key} {asset}");
        }
        if let Some(sampler) = track.sampler {
            let hit = if sampler.one_shot { " hit" } else { "" };
            let _ = writeln!(out, "sampler {} {}{hit}", sampler.asset, sampler.root);
        }
        for c in track.clips.iter() {
            let _ = writeln!(out, "clip {} {} {} {} {}", c.id, c.asset, c.start, c.len, c.src_in);
        }
        // One line per note clip: its notes as start, length, pitch, velocity.
        for (id, notes) in track.notes.iter() {
            let list: Vec<String> =
                notes.iter().map(|n| format!("{}:{}:{}:{}", n.start, n.len, n.pitch, n.velocity)).collect();
            let _ = writeln!(out, "notes {id} {}", list.join(" "));
        }
        // One line per clip that has properties, sorted by clip id.
        for (id, props) in track.props.iter() {
            let _ = writeln!(out, "props {id} {}", serde_json::to_string(&**props).unwrap_or_default());
        }
    }
    out
}

fn parse_sequence(file: &str, text: &str) -> Result<Sequence, TextError> {
    let err = |line: usize, message: &str| TextError::Line {
        file: file.to_owned(),
        line: line + 1,
        message: message.to_owned(),
    };
    let mut lines = text.lines().enumerate();
    let (_, magic) = lines.next().ok_or_else(|| err(0, "empty file"))?;
    match magic.strip_prefix(SEQUENCE_MAGIC).map(|v| v.trim().parse::<u32>()) {
        Some(Ok(v)) if v <= FORMAT_VERSION => {}
        Some(Ok(found)) => return Err(TextError::Version { file: file.to_owned(), found }),
        _ => return Err(err(0, "not a shotgun sequence file")),
    }

    let (mut id, mut name, mut rate, mut size) = (None, None, None, None);
    let mut master_db = 0.0f32;
    let mut tempo = None;
    let mut space = crate::model::Space::default();
    let mut groups = [crate::model::Group::default(); crate::model::GROUPS];
    let mut song = None;
    let mut markers: Vec<Marker> = Vec::new();
    // The track being read: its header and clips so far.
    let mut current: Option<(Track, Vec<Clip>)> = None;
    let mut tracks = Vec::new();
    let mut finish = |current: &mut Option<(Track, Vec<Clip>)>, line: usize| -> Result<(), TextError> {
        if let Some((mut track, clips)) = current.take() {
            track.clips = ClipStore::from_sorted(clips).map_err(|e| err(line, &e.to_string()))?;
            tracks.push(Arc::new(track));
        }
        Ok(())
    };

    for (n, raw) in lines {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "clip" => {
                let (_, clips) = current.as_mut().ok_or_else(|| err(n, "clip before any track"))?;
                clips.push(parse_clip(rest).ok_or_else(|| err(n, "malformed clip line"))?);
            }
            "plugin" => {
                let (track, _) = current.as_mut().ok_or_else(|| err(n, "plugin before any track"))?;
                let plugin: crate::model::PluginRef =
                    serde_json::from_str(rest).map_err(|e| err(n, &format!("bad plugin: {e}")))?;
                // (One line each, in the order the sound runs through them.)
                Arc::make_mut(&mut track.plugins).push(plugin);
            }
            "pad" => {
                let (track, _) = current.as_mut().ok_or_else(|| err(n, "pad before any track"))?;
                let parsed = rest
                    .split_once(' ')
                    .and_then(|(key, asset)| Some((key.parse::<u8>().ok()?.min(127), asset.parse().ok()?)));
                Arc::make_mut(&mut track.pads).push(parsed.ok_or_else(|| err(n, "bad pad"))?);
            }
            "sampler" => {
                let (track, _) = current.as_mut().ok_or_else(|| err(n, "sampler before any track"))?;
                let mut words = rest.split(' ');
                let parsed = (|| {
                    Some(crate::model::Sampler {
                        asset: words.next()?.parse().ok()?,
                        root: words.next()?.parse::<u8>().ok()?.min(127),
                        one_shot: words.next() == Some("hit"),
                    })
                })();
                track.sampler = Some(parsed.ok_or_else(|| err(n, "bad sampler"))?);
            }
            "notes" => {
                let (track, _) = current.as_mut().ok_or_else(|| err(n, "notes before any track"))?;
                let (id, list) = rest.split_once(' ').unwrap_or((rest, ""));
                let id = id.parse().map_err(|_| err(n, "bad clip id"))?;
                let mut notes = Vec::new();
                for item in list.split_whitespace() {
                    let mut parts = item.split(':').map(str::parse::<i64>);
                    let (Some(Ok(start)), Some(Ok(len)), Some(Ok(pitch)), Some(Ok(velocity))) =
                        (parts.next(), parts.next(), parts.next(), parts.next())
                    else {
                        return Err(err(n, "malformed note"));
                    };
                    notes.push(crate::model::Note {
                        start,
                        len,
                        pitch: pitch.clamp(0, 127) as u8,
                        velocity: velocity.clamp(1, 127) as u8,
                    });
                }
                let mut map = BTreeMap::clone(&track.notes);
                map.insert(id, Arc::new(notes));
                track.notes = Arc::new(map);
            }
            "props" => {
                let (track, _) = current.as_mut().ok_or_else(|| err(n, "props before any track"))?;
                let (id, json) = rest.split_once(' ').ok_or_else(|| err(n, "malformed props line"))?;
                let id = id.parse().map_err(|_| err(n, "bad clip id"))?;
                let props: ClipProps =
                    serde_json::from_str(json).map_err(|e| err(n, &format!("bad clip properties: {e}")))?;
                let mut map = BTreeMap::clone(&track.props);
                map.insert(id, Arc::new(props));
                track.props = Arc::new(map);
            }
            "track" => {
                finish(&mut current, n)?;
                let mut parts = rest.splitn(3, ' ');
                let track_id = parts.next().and_then(|s| s.parse().ok()).ok_or_else(|| err(n, "bad track id"))?;
                let kind = match parts.next() {
                    Some("video") => TrackKind::Video,
                    Some("audio") => TrackKind::Audio,
                    _ => return Err(err(n, "track kind must be video or audio")),
                };
                let track_name = parts.next().and_then(unquote).ok_or_else(|| err(n, "bad track name"))?;
                current = Some((Track::from(TrackHeader { id: track_id, kind, name: track_name }), Vec::new()));
            }
            "mix" => {
                let (track, _) = current.as_mut().ok_or_else(|| err(n, "mix before any track"))?;
                let mix: TrackSettings =
                    serde_json::from_str(rest).map_err(|e| err(n, &format!("bad track settings: {e}")))?;
                track.mix = mix.clamped();
            }
            "song" => {
                let (asset, tempo) = rest.split_once(' ').unwrap_or((rest, "0"));
                let asset = asset.parse().map_err(|_| err(n, "bad song asset"))?;
                song = Some(Song {
                    asset,
                    tempo: tempo.parse::<i8>().map_err(|_| err(n, "bad song tempo"))?.clamp(-2, 2),
                });
            }
            "tempo" => {
                let mut words = rest.split(' ');
                let parsed = (|| {
                    Some(crate::model::Tempo {
                        milli_bpm: words.next()?.parse().ok()?,
                        beats_per_bar: words.next()?.parse().ok()?,
                        // (Where the first bar begins; left out when it begins with the timeline.)
                        origin: match words.next() {
                            Some(origin) => origin.parse().ok()?,
                            None => 0,
                        },
                    })
                })();
                tempo = Some(parsed.ok_or_else(|| err(n, "bad tempo"))?.clamped());
            }
            "group" => {
                let mut words = rest.split(' ');
                let parsed = (|| {
                    let slot = groups.get_mut(words.next()?.parse::<usize>().ok()?)?;
                    let gain_db = words.next()?.parse::<f32>().ok()?;
                    *slot = crate::model::Group { gain_db, mute: words.next()? == "1" }.clamped();
                    Some(())
                })();
                parsed.ok_or_else(|| err(n, "bad group"))?;
            }
            "space" => {
                let mut words = rest.split(' ');
                let parsed = (|| {
                    Some(crate::model::Space {
                        room_ds: words.next()?.parse().ok()?,
                        echo_note: crate::model::EchoNote::from_label(words.next()?)?,
                        echo_repeat: words.next()?.parse().ok()?,
                    })
                })();
                space = parsed.ok_or_else(|| err(n, "bad room and echo"))?.clamped();
            }
            "master" => {
                let level = rest.parse::<f32>().ok().filter(|db| db.is_finite());
                master_db = level.ok_or_else(|| err(n, "bad master level"))?.clamp(-60.0, 12.0);
            }
            "id" => id = Some(rest.parse().map_err(|_| err(n, "bad sequence id"))?),
            "name" => name = Some(unquote(rest).ok_or_else(|| err(n, "bad name"))?),
            "rate" => rate = Some(rest.parse().map_err(|_| err(n, "bad rate"))?),
            "size" => {
                let parsed = rest.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                size = Some(parsed.ok_or_else(|| err(n, "bad size"))?);
            }
            "marker" => {
                let (frame, label) = rest.split_once(' ').unwrap_or((rest, "\"\""));
                let frame = frame.parse().map_err(|_| err(n, "bad marker frame"))?;
                let label = unquote(label).ok_or_else(|| err(n, "bad marker label"))?;
                markers.push(Marker { frame, label });
            }
            // Unknown keys are skipped so that a newer minor format stays readable.
            _ => {}
        }
    }
    let last = text.lines().count();
    finish(&mut current, last)?;
    let missing = |what: &str| TextError::File { file: file.to_owned(), message: format!("missing {what}") };
    let (width, height) = size.ok_or_else(|| missing("size"))?;
    markers.sort_by_key(|m| m.frame);
    markers.dedup_by_key(|m| m.frame);
    Ok(Sequence {
        id: id.ok_or_else(|| missing("id"))?,
        name: name.ok_or_else(|| missing("name"))?,
        rate: rate.ok_or_else(|| missing("rate"))?,
        width,
        height,
        tracks,
        master_db,
        tempo,
        song,
        markers: Arc::new(markers),
        space,
        groups,
    })
}

fn parse_clip(rest: &str) -> Option<Clip> {
    let mut parts = rest.split_ascii_whitespace();
    let clip = Clip {
        id: parts.next()?.parse().ok()?,
        asset: parts.next()?.parse().ok()?,
        start: parts.next()?.parse().ok()?,
        len: parts.next()?.parse().ok()?,
        src_in: parts.next()?.parse().ok()?,
    };
    parts.next().is_none().then_some(clip)
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn unquote(s: &str) -> Option<String> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.push(match chars.next()? {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                c @ ('"' | '\\') => c,
                _ => return None,
            }),
            '"' => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::ops::tests::{any_op, base, make_op};
    use crate::ops::{Op, apply};

    #[test]
    fn slugs() {
        assert_eq!(slug("My First Video!"), "my-first-video");
        assert_eq!(slug("  Züri  Fäscht 2026 "), "zueri-faescht-2026");
        assert_eq!(slug("???"), "untitled");
        assert_eq!(slug("a/b\\c..d"), "a-b-c-d");
        assert_eq!(slug("../../etc"), "etc");
        let taken = ["main", "main-2"];
        assert_eq!(unique_slug("main", |s| taken.contains(&s)), "main-3");
        assert_eq!(unique_slug("cut", |s| taken.contains(&s)), "cut");
    }

    #[test]
    fn quoting_round_trips() {
        for s in ["", "plain", "with \"quotes\" and \\ slash", "line\nbreak\ttab", "ünïcödé 🎬"] {
            assert_eq!(unquote(&quote(s)).as_deref(), Some(s));
        }
        assert_eq!(unquote("no quotes"), None);
        assert_eq!(unquote("\"bad \\x\""), None);
        assert_eq!(unquote("\"a\"b\""), None);
    }

    #[test]
    fn mixer_settings_survive_the_files() {
        use crate::ops::tests::{A1, SEQ};
        let mix = TrackSettings {
            gain_db: -4.5,
            pan: -0.25,
            mute: false,
            solo: true,
            low_cut_hz: 90.0,
            denoise: 0.6,
            hidden: true,
            eq_low_db: 2.5,
            eq_mid_hz: 2500.0,
            comp_threshold_db: -12.0,
            ..TrackSettings::default()
        };
        let (p, _) = apply(&base(), &Op::SetTrackMix { seq: SEQ, track: A1, mix }).unwrap();
        let (p, _) = apply(&p, &Op::SetMasterGain { id: SEQ, db: -2.0 }).unwrap();
        let waltz = crate::model::Tempo { milli_bpm: 93_500, beats_per_bar: 3, origin: -4_800 };
        let (p, _) = apply(&p, &Op::SetTempo { id: SEQ, tempo: Some(waltz) }).unwrap();
        let space =
            crate::model::Space { room_ds: 32, echo_note: crate::model::EchoNote::DottedEighth, echo_repeat: 55 };
        let (p, _) = apply(&p, &Op::SetSpace { id: SEQ, space }).unwrap();
        let quiet = crate::model::Group { gain_db: -4.5, mute: true };
        let (p, _) = apply(&p, &Op::SetGroup { id: SEQ, group: 3, settings: quiet }).unwrap();
        let surge =
            crate::model::PluginRef { name: "Surge XT".into(), path: "/lib/vst3/Surge \"XT\".vst3".into(), id: 0 };
        let wide = crate::model::PluginRef { name: "Wide".into(), path: "/lib/clap/Wide.clap".into(), id: 0xBEEF };
        let (p, _) = apply(&p, &Op::SetTrackPlugins { seq: SEQ, track: A1, plugins: vec![surge, wide] }).unwrap();
        let hit = crate::model::Sampler { asset: crate::ids::AssetId(0xABC), root: 36, one_shot: true };
        let (p, _) = apply(&p, &Op::SetTrackSampler { seq: SEQ, track: A1, sampler: Some(hit) }).unwrap();
        let kit = vec![(36, crate::ids::AssetId(0xABC)), (42, crate::ids::AssetId(0xDEF))];
        let (p, _) = apply(&p, &Op::SetTrackPads { seq: SEQ, track: A1, pads: kit }).unwrap();
        let notes = vec![
            crate::model::Note { start: 0, len: 480, pitch: 60, velocity: 100 },
            crate::model::Note { start: 960, len: 240, pitch: 67, velocity: 64 },
        ];
        let (p, _) =
            apply(&p, &Op::SetClipNotes { seq: SEQ, track: A1, clip: crate::ClipId(5), notes: Some(notes) }).unwrap();
        let song = crate::model::Song { asset: crate::ids::AssetId(0xABC), tempo: -1 };
        let (p, _) = apply(&p, &Op::SetSong { id: SEQ, song: Some(song) }).unwrap();
        let files = to_files(&p);
        assert_eq!(from_files(&files).unwrap(), *p);
        // A project without any of it writes no extra lines.
        let plain: String = to_files(&base()).values().cloned().collect();
        assert!(!plain.contains("mix ") && !plain.contains("master "));
    }

    #[test]
    fn scenarios_survive_the_files() {
        use crate::colour::{ColourStats, Member, Module, ModuleKind};
        let id = crate::ids::ScenarioId(0x51);
        let mut scenario = Scenario::new(id, "Night stage", 3);
        scenario.hero = Some(crate::ids::AssetId(9));
        scenario.strength = 0.75;
        let stats = ColourStats { log_lum: -2.25, spread: 1.5, wb: [0.125, -0.25], saturation: 0.375 };
        scenario.members.insert(crate::ids::AssetId(9), Member { stats: Some(stats) });
        scenario.members.insert(crate::ids::AssetId(10), Member::default());
        scenario.look = vec![
            Module::new(ModuleKind::Balance),
            Module { on: false, mix: 0.5, ..Module::new(ModuleKind::SplitTone) },
        ];
        let (p, _) = apply(&base(), &Op::SetScenario { id, scenario: Some(Box::new(scenario)) }).unwrap();
        let files = to_files(&p);
        assert!(files.contains_key("scenarios/0000000000000051.toml"), "{:?}", files.keys());
        assert_eq!(from_files(&files).unwrap(), *p);
    }

    #[test]
    fn file_layout_is_readable_and_stable() {
        let (p, _) = apply(
            &base(),
            &Op::InsertClip {
                seq: crate::ops::tests::SEQ,
                track: crate::ops::tests::V1,
                clip: crate::ops::tests::clip(0xC1, 30, 90),
            },
        )
        .unwrap();
        let files = to_files(&p);
        assert_eq!(files.keys().collect::<Vec<_>>(), ["project.toml", "sequences/main.sg"]);
        assert_eq!(
            files["project.toml"],
            "format = 1\nid = \"0000000000000000000000000000002a\"\nname = \"Test\"\nsequences = [\"main\"]\n"
        );
        assert_eq!(
            files["sequences/main.sg"],
            "shotgun-sequence 1\n\
             id 0000000000000001\n\
             name \"Main\"\n\
             rate 60/1\n\
             size 1920x1080\n\
             \n\
             track 000000000000000a video \"V1\"\n\
             clip 00000000000000c1 000000000000000000000000000a55e7 30 90 0\n\
             \n\
             track 000000000000000b audio \"A1\"\n"
        );
        assert_eq!(project_header(&files["project.toml"]).unwrap(), (ProjectId(42), "Test".to_owned()));
    }

    #[test]
    fn sequences_with_equal_names_get_distinct_files() {
        let mut p = Project::clone(&base());
        let twin = Sequence { id: crate::ids::SequenceId(2), ..Sequence::clone(&p.sequences[0]) };
        p.sequences.push(Arc::new(twin));
        let files = to_files(&p);
        assert!(files.contains_key("sequences/main.sg") && files.contains_key("sequences/main-2.sg"));
        assert_eq!(from_files(&files).unwrap(), p);
    }

    #[test]
    fn errors_name_file_and_line() {
        let mut files = to_files(&base());
        files.insert("sequences/main.sg".into(), "shotgun-sequence 1\nid 0000000000000001\nclip x\n".into());
        assert_eq!(from_files(&files).unwrap_err().to_string(), "sequences/main.sg:3: clip before any track");
        files.insert("sequences/main.sg".into(), "shotgun-sequence 99\n".into());
        assert!(matches!(from_files(&files), Err(TextError::Version { found: 99, .. })));
        files.remove("sequences/main.sg");
        assert_eq!(from_files(&files).unwrap_err().to_string(), "sequences/main.sg is missing");
        let evil = files["project.toml"].replace("\"main\"", "\"../../etc/passwd\"");
        assert!(sequence_paths(&evil).unwrap_err().to_string().contains("invalid sequence name"));
        let overlapping = "shotgun-sequence 1\nid 0000000000000001\nname \"x\"\nrate 60/1\nsize 1x1\n\
            track 000000000000000a video \"V1\"\n\
            clip 0000000000000001 00000000000000000000000000000001 0 10 0\n\
            clip 0000000000000002 00000000000000000000000000000001 5 10 0\n";
        files.insert("sequences/main.sg".into(), overlapping.into());
        assert!(from_files(&files).unwrap_err().to_string().contains("overlaps"));
    }

    proptest! {
        #[test]
        fn files_round_trip(seeds in prop::collection::vec(any_op(), 0..120), name in "\\PC{0,24}") {
            let mut p = apply(&base(), &Op::SetProjectName { name }).unwrap().0;
            for seed in seeds {
                if let Ok((next, _)) = apply(&p, &make_op(&p, seed)) {
                    p = next;
                }
            }
            let files = to_files(&p);
            let back = from_files(&files).unwrap();
            prop_assert_eq!(&back, &*p);
            prop_assert_eq!(to_files(&back), files);
        }
    }
}
