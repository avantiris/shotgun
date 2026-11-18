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
        "asset" => Step::Dev(DevStep::SelectAsset(need()?.parse().map_err(|_| anyhow!("asset needs a number"))?)),
        "add" => Step::Dev(DevStep::Act(Action::AddAsset)),
        "insert" => Step::Dev(DevStep::Act(Action::InsertAsset)),
        "split" => Step::Dev(DevStep::Act(Action::Split)),
        "rippleq" => Step::Dev(DevStep::Act(Action::RippleStart)),
        "ripplee" => Step::Dev(DevStep::Act(Action::RippleEnd)),
        "xdel" => Step::Dev(DevStep::Act(Action::RippleDelete)),
        "select" => Step::Dev(DevStep::Act(Action::SelectUnder)),
        "copy" => Step::Dev(DevStep::Act(Action::Copy)),
        "paste" => Step::Dev(DevStep::Act(Action::PasteClips)),
        "marker" => Step::Dev(DevStep::Act(Action::Marker)),
        "history" => Step::Dev(DevStep::Act(Action::History)),
        "exportpanel" => Step::Dev(DevStep::Act(Action::Export)),
        "export" => Step::Dev(DevStep::Export(need()?)),
        "size" => {
            let (w, h) = arg.split_once('x').ok_or_else(|| anyhow!("size needs <width>x<height>"))?;
            Step::Dev(DevStep::Size(w.parse()?, h.parse()?))
        }
        "key" => Step::Dev(DevStep::Act(Action::KeyframeSet)),
        "title" => Step::Dev(DevStep::Act(Action::AddTitle)),
        "record" => Step::Dev(DevStep::Act(Action::LiveRecord)),
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
