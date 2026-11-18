//! Scripted runs: `--script "new:Demo;wait:500;rename:Other;undo;close;quit"`.
//! Automated checks drive the app through this instead of injecting pointer
//! or keyboard input into the desktop the user is working on.

use anyhow::{anyhow, bail};
use sg_ui::{Action, DevStep};

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Dev(DevStep),
    WaitMs(u64),
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
