//! Scripted runs: `--script "new:Demo;wait:500;rename:Other;undo;close;quit"`.
//! Automated checks drive the app through this instead of injecting pointer
//! or keyboard input into the desktop the user is working on.

use anyhow::{anyhow, bail};
use sg_ui::{Action, DevStep, Transport};

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Dev(DevStep),
    WaitMs(u64),
    /// Render this many UI frames off-screen with the timeline zooming and
    /// scrolling, and print their CPU times.
    TimelineBench(usize),
    /// Write a PNG of the UI, rendered off-screen.
    Shot(std::path::PathBuf),
    Quit,
}

pub fn parse(script: &str) -> anyhow::Result<Vec<Step>> {
    script.split(';').map(str::trim).filter(|s| !s.is_empty()).map(parse_step).collect()
}

fn parse_step(text: &str) -> anyhow::Result<Step> {
    let (verb, arg) = text.split_once(':').unwrap_or((text, ""));
    let need =
        || if arg.is_empty() { Err(anyhow!("script step {verb:?} needs an argument")) } else { Ok(arg.to_owned()) };
    Ok(match verb {
        "filter" => Step::Dev(DevStep::Filter(arg.to_owned())),
        "new" => Step::Dev(DevStep::New(arg.to_owned())),
        "open" => Step::Dev(DevStep::Open(need()?)),
        "rename" => Step::Dev(DevStep::Rename(need()?)),
        "import" => Step::Dev(DevStep::Import(need()?)),
        "importsound" => Step::Dev(DevStep::ImportSound(need()?)),
        "asset" => Step::Dev(DevStep::SelectAsset(need()?.parse().map_err(|_| anyhow!("asset needs a number"))?)),
        "add" => Step::Dev(DevStep::Act(Action::AddAsset)),
        "insert" => Step::Dev(DevStep::Act(Action::InsertAsset)),
        "split" => Step::Dev(DevStep::Act(Action::Split)),
        "rippleq" => Step::Dev(DevStep::Act(Action::RippleStart)),
        "ripplee" => Step::Dev(DevStep::Act(Action::RippleEnd)),
        "xdel" => Step::Dev(DevStep::Act(Action::RippleDelete)),
        "select" => Step::Dev(DevStep::Act(Action::SelectUnder)),
        "selectall" => Step::Dev(DevStep::Act(Action::SelectAll)),
        "group" => Step::Dev(DevStep::Act(Action::Group)),
        "copy" => Step::Dev(DevStep::Act(Action::Copy)),
        // global:make (the selected asset), global:tab, global:use:<n>
        "global" => Step::Dev(DevStep::Global(need()?)),
        "cut" => Step::Dev(DevStep::Act(Action::Cut)),
        "ctrlv" => Step::Dev(DevStep::Act(Action::Paste)),
        "paste" => Step::Dev(DevStep::Act(Action::PasteClips)),
        "marker" => Step::Dev(DevStep::Act(Action::Marker)),
        "history" => Step::Dev(DevStep::Act(Action::History)),
        "exportpanel" => Step::Dev(DevStep::Act(Action::Export)),
        "export" => Step::Dev(DevStep::Export(need()?)),
        "drop" => {
            let (frame, row) = arg.split_once(':').ok_or_else(|| anyhow!("drop needs <frame>:<row or new>"))?;
            let (row, only) = match row.split_once(':') {
                Some((row, "sound")) => (row, Some(sg_core::TrackKind::Audio)),
                Some((row, "picture")) => (row, Some(sg_core::TrackKind::Video)),
                _ => (row, None),
            };
            let row = if row == "new" { None } else { Some(row.parse()?) };
            Step::Dev(DevStep::Drop(frame.parse()?, row, only))
        }
        "mixer" => Step::Dev(DevStep::Act(Action::Mixer)),
        "chain" => Step::Dev(DevStep::Chain),
        "cutout" => Step::Dev(DevStep::CutOut),
        "findkey" => Step::Dev(DevStep::FindKey),
        "sky" => Step::Dev(DevStep::Sky),
        // change:<service's name>:<sentence>
        "change" => Step::Dev(DevStep::Change(need()?)),
        "blurred" => Step::Dev(DevStep::Blurred),
        "builtinlooks" => Step::Dev(DevStep::BuiltinLooks),
        // scale:<root, 0 is C>:<1 minor, 0 major>
        "scale" => {
            let (root, minor) = arg.split_once(':').ok_or_else(|| anyhow!("scale needs <root>:<minor>"))?;
            Step::Dev(DevStep::Scale(root.parse()?, minor.parse()?))
        }
        // stretch:<semitones>:<times as fast>
        "stretch" => {
            let (semitones, tempo) = arg.split_once(':').ok_or_else(|| anyhow!("stretch needs <semitones>:<tempo>"))?;
            Step::Dev(DevStep::Stretch(semitones.parse()?, tempo.parse()?))
        }
        "stage" => Step::Dev(DevStep::Stage(need()?)),
        "lyrics" => Step::Dev(DevStep::Lyrics(need()?)),
        "help" => Step::Dev(DevStep::Act(Action::Help)),
        "colourpanel" => Step::Dev(DevStep::Act(Action::Colour)),
        "lookab" => Step::Dev(DevStep::Act(Action::ToggleLook)),
        "matchab" => Step::Dev(DevStep::Act(Action::ToggleMatch)),
        // scenario:new, scenario:none or scenario:<n> for the selected asset.
        "scenario" => Step::Dev(DevStep::Scenario(need()?)),
        // look:<module>:<k0>,<k1>,<k2>,<k3> adds a module to the shown scenario.
        "look" => Step::Dev(DevStep::Look(need()?)),
        "hero" => Step::Dev(DevStep::Hero),
        "lut" => Step::Dev(DevStep::Lut(need()?)),
        "transition" => Step::Dev(DevStep::Act(Action::Transition)),
        "blackout" => Step::Dev(DevStep::Act(Action::Blackout)),
        "song" => Step::Dev(DevStep::Song),
        "cutstobeats" => Step::Dev(DevStep::Programme(true)),
        "whipall" => Step::Dev(DevStep::Programme(false)),
        "sync" => Step::Dev(DevStep::Sync(false)),
        "syncall" => Step::Dev(DevStep::Sync(true)),
        "tempo" => Step::Dev(DevStep::Tempo(need()?.parse()?)),
        "bars" => Step::Dev(DevStep::Bars(need()?.parse()?)),
        "voice" => Step::Dev(DevStep::Voice(need()?.parse()?)),
        "plugins" => Step::Dev(DevStep::Plugins(need()?)),
        // noteclip:<audio track>:<frame>   note:<audio track>:<pitch>:<start ticks>:<length ticks>
        "noteclip" => {
            let (track, frame) = arg.split_once(':').ok_or_else(|| anyhow!("noteclip needs <track>:<frame>"))?;
            Step::Dev(DevStep::NoteClip(track.parse()?, frame.parse()?))
        }
        "note" => {
            let parts: Vec<&str> = arg.split(':').collect();
            let [track, pitch, start, len] = parts.as_slice() else {
                bail!("note needs <track>:<pitch>:<start>:<length>")
            };
            Step::Dev(DevStep::Note(track.parse()?, pitch.parse()?, start.parse()?, len.parse()?))
        }
        // hand:<pitch>:<on|off> plays a key of the open piano roll's instrument; recordnotes switches its "Record".
        "hand" => {
            let (pitch, state) = arg.split_once(':').ok_or_else(|| anyhow!("hand needs <pitch>:<on|off>"))?;
            Step::Dev(DevStep::Hand(pitch.parse()?, state == "on"))
        }
        "recordnotes" => Step::Dev(DevStep::RecordNotes),
        // arm:<audio track> marks the track recordings go onto.
        "arm" => Step::Dev(DevStep::Arm(need()?.parse()?)),
        // sampler:<audio track>:<root key> gives the track's notes the selected asset's sound to play.
        "sampler" => {
            let (track, root) = arg.split_once(':').ok_or_else(|| anyhow!("sampler needs <track>:<root key>"))?;
            Step::Dev(DevStep::Sampler(track.parse()?, root.parse()?))
        }
        // level opens (or closes) the input for its meter; hear adds it to what is heard.
        "level" => Step::Dev(DevStep::Monitor(false)),
        "hear" => Step::Dev(DevStep::Monitor(true)),
        // plugin:<audio track>:<path of the bundle | window | (empty)>
        "plugin" => {
            let (track, path) = arg.split_once(':').ok_or_else(|| anyhow!("plugin needs <track>:<path>"))?;
            Step::Dev(DevStep::Plugin(track.parse()?, path.to_owned()))
        }
        "angle" => Step::Dev(DevStep::Act(Action::Angle(need()?.parse()?))),
        "mix" => {
            // mix:<audio track number or master>:<field>=<value>
            let (which, rest) = arg.split_once(':').ok_or_else(|| anyhow!("mix needs <track>:<field>=<value>"))?;
            let (field, value) = rest.split_once('=').ok_or_else(|| anyhow!("mix needs <field>=<value>"))?;
            Step::Dev(DevStep::Mix(which.to_owned(), field.to_owned(), value.parse()?))
        }
        "size" => {
            let (w, h) = arg.split_once('x').ok_or_else(|| anyhow!("size needs <width>x<height>"))?;
            Step::Dev(DevStep::Size(w.parse()?, h.parse()?))
        }
        "key" => Step::Dev(DevStep::Act(Action::KeyframeSet)),
        "title" => Step::Dev(DevStep::Act(Action::AddTitle)),
        "record" => Step::Dev(DevStep::Act(Action::LiveRecord)),
        "recordsound" => Step::Dev(DevStep::Act(Action::RecordSound)),
        "metronome" => Step::Dev(DevStep::Act(Action::Metronome)),
        "loop" => Step::Dev(DevStep::Act(Action::Loop)),
        "vtrack" => Step::Dev(DevStep::AddTrack(sg_core::TrackKind::Video)),
        "atrack" => Step::Dev(DevStep::AddTrack(sg_core::TrackKind::Audio)),
        "prop" => {
            let (name, value) = arg.split_once('=').ok_or_else(|| anyhow!("prop needs <name>=<value>"))?;
            Step::Dev(DevStep::Prop(name.to_owned(), value.to_owned()))
        }
        "checkpoint" => Step::Dev(DevStep::Checkpoint(need()?)),
        "restore" => Step::Dev(DevStep::Restore(
            need()?.parse().map_err(|_| anyhow!("restore needs the position in the history list"))?,
        )),
        "prev" => Step::Dev(DevStep::Act(Action::PrevCut)),
        "next" => Step::Dev(DevStep::Act(Action::NextCut)),
        "view" => {
            let (zoom, left) =
                arg.split_once(',').ok_or_else(|| anyhow!("view needs <frames per point>,<left frame>"))?;
            Step::Dev(DevStep::TimelineView(zoom.parse()?, left.parse()?))
        }
        "timelinebench" => {
            Step::TimelineBench(need()?.parse().map_err(|_| anyhow!("timelinebench needs a number of frames"))?)
        }
        "play" => Step::Dev(DevStep::Act(Action::PlayPause)),
        "seek" => Step::Dev(DevStep::Transport(Transport::Seek(
            need()?.parse().map_err(|_| anyhow!("seek needs a frame number"))?,
        ))),
        "step" => Step::Dev(DevStep::Transport(Transport::Step(
            need()?.parse().map_err(|_| anyhow!("step needs a number of frames"))?,
        ))),
        "del" => Step::Dev(DevStep::Act(Action::Delete)),
        "undo" => Step::Dev(DevStep::Act(Action::Undo)),
        "redo" => Step::Dev(DevStep::Act(Action::Redo)),
        "close" => Step::Dev(DevStep::Act(Action::CloseProject)),
        "perf" => Step::Dev(DevStep::Act(Action::TogglePerf)),
        "esc" => Step::Dev(DevStep::Act(Action::Cancel)),
        "wait" => Step::WaitMs(need()?.parse().map_err(|_| anyhow!("wait needs milliseconds, got {arg:?}"))?),
        "shot" => Step::Shot(need()?.into()),
        "quit" => Step::Quit,
        _ => bail!("unknown script step {text:?}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_session() {
        let steps = parse("new:My Clip; wait:250;rename:a:b;undo;close;quit;").unwrap();
        assert_eq!(
            steps,
            [
                Step::Dev(DevStep::New("My Clip".into())),
                Step::WaitMs(250),
                Step::Dev(DevStep::Rename("a:b".into())),
                Step::Dev(DevStep::Act(Action::Undo)),
                Step::Dev(DevStep::Act(Action::CloseProject)),
                Step::Quit,
            ]
        );
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse("explode").is_err());
        assert!(parse("wait:soon").is_err());
        assert!(parse("open").is_err());
    }
}
