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

fn output(root: &Path, args: &[&str]) -> Result<String, ProjectError> {
    let fail = |stderr: String| ProjectError::Git { command: args.join(" "), stderr };
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| fail(e.to_string()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(fail(String::from_utf8_lossy(&out.stderr).trim().to_owned()))
    }
}

/// True if the tracked files differ from the last commit.
pub fn has_changes(root: &Path) -> bool {
    output(root, &["status", "--porcelain"]).is_ok_and(|s| !s.trim().is_empty())
}

/// Commits if there is something to commit. Returns whether it did.
pub fn commit_if_changed(root: &Path, message: &str) -> Result<bool, ProjectError> {
    if !has_changes(root) {
        return Ok(false);
    }
    commit_all(root, message).map(|()| true)
}

/// Names the current commit. The tag name is derived from `name`; a number
/// is added if that name is taken.
pub fn checkpoint(root: &Path, name: &str) -> Result<String, ProjectError> {
    let base = sg_core::text::slug(name);
    let existing = output(root, &["tag", "--list"]).unwrap_or_default();
    let tag = sg_core::text::unique_slug(&base, |t| existing.lines().any(|l| l == t));
    let args = ["tag", "-a", &tag, "-m", name];
    run(root, &args, false).or_else(|_| run(root, &args, true))?;
    Ok(tag)
}

/// One saved state of a project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub commit: String,
    /// Unix seconds.
    pub time: u64,
    pub message: String,
    /// Names of the checkpoints on this state.
    pub checkpoints: Vec<String>,
}

/// The newest `limit` saved states, newest first.
pub fn history(root: &Path, limit: usize) -> Result<Vec<HistoryEntry>, ProjectError> {
    // Unit separator between fields: commit messages cannot contain it.
    let format = "--format=%H%x1f%ct%x1f%D%x1f%s";
    let log = output(root, &["log", format, "-n", &limit.to_string()])?;
    Ok(log
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\u{1f}');
            let commit = fields.next()?.to_owned();
            let time = fields.next()?.parse().ok()?;
            let refs = fields.next()?;
            let message = fields.next().unwrap_or("").to_owned();
            let checkpoints = refs.split(", ").filter_map(|r| r.strip_prefix("tag: ")).map(str::to_owned).collect();
            Some(HistoryEntry { commit, time, message, checkpoints })
        })
        .collect())
}

/// Makes the tracked project files equal to those of `commit` and records
/// that as a new commit. History is never rewritten.
pub fn restore(root: &Path, commit: &str, description: &str) -> Result<(), ProjectError> {
    if commit.len() < 7 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ProjectError::Git { command: "restore".into(), stderr: format!("not a commit id: {commit}") });
    }
    let source = format!("--source={commit}");
    run(
        root,
        &["restore", &source, "--staged", "--worktree", "--", "project.toml", "sequences", "assets/index", "scenarios"],
        false,
    )?;
    commit_if_changed(root, &format!("Restore: {description}")).map(|_| ())
}
