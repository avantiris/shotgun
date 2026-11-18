//! git as a subprocess. Job pool only.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::ProjectError;

fn run(root: &Path, args: &[&str], fallback_identity: bool) -> Result<(), ProjectError> {
    let mut cmd = Command::new("git");
    cmd.current_dir(root).args(args).stdin(Stdio::null()).env("GIT_TERMINAL_PROMPT", "0");
    if fallback_identity {
        for (key, value) in [("NAME", "Shotgun"), ("EMAIL", "shotgun@localhost")] {
            cmd.env(format!("GIT_AUTHOR_{key}"), value).env(format!("GIT_COMMITTER_{key}"), value);
        }
    }
    let fail = |stderr: String| ProjectError::Git { command: args.join(" "), stderr };
    let out = cmd.output().map_err(|e| fail(e.to_string()))?;
    if out.status.success() { Ok(()) } else { Err(fail(String::from_utf8_lossy(&out.stderr).trim().to_owned())) }
}

pub fn init(root: &Path) -> Result<(), ProjectError> {
    run(root, &["init", "-q", "-b", "main"], false)
}

/// Stages everything that is not ignored and commits it.
pub fn commit_all(root: &Path, message: &str) -> Result<(), ProjectError> {
    run(root, &["add", "-A"], false)?;
    let args = ["commit", "-q", "-m", message];
    // A machine without a configured git identity must still get history.
    run(root, &args, false).or_else(|_| run(root, &args, true))
}
