//! Running external programs with cancellation and captured stderr.

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// Shared flag that stops a running job. Cheap to clone and to check.
#[derive(Clone, Default, Debug)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("cancelled")]
    Cancelled,
    #[error("cannot start {program}: {source}")]
    Spawn { program: String, source: std::io::Error },
    /// The program ran and failed; `stderr` is the end of what it printed.
    #[error("{program} failed: {stderr}")]
    Failed { program: String, stderr: String },
    #[error("{0}")]
    Other(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ToolError {
    pub fn other(message: impl Into<String>) -> ToolError {
        ToolError::Other(message.into())
    }
}

/// How much of stderr is kept for the error message.
const STDERR_TAIL: usize = 2000;

/// Runs `command`, calling `on_line` for every line it prints to stdout.
/// Kills the program when `cancel` is set.
pub fn run_lines(mut command: Command, cancel: &Cancel, mut on_line: impl FnMut(&str)) -> Result<(), ToolError> {
    let program = command.get_program().to_string_lossy().into_owned();
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|source| ToolError::Spawn { program: program.clone(), source })?;
    let (stdout, stderr) = match (child.stdout.take(), child.stderr.take()) {
        (Some(out), Some(err)) => (out, err),
        _ => return Err(ToolError::other("child has no pipes")),
    };
    let stderr_thread = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut text);
        let cut = text.len().saturating_sub(STDERR_TAIL);
        let start = (cut..=text.len()).find(|&i| text.is_char_boundary(i)).unwrap_or(text.len());
        text[start..].trim().to_owned()
    });
    let (tx, rx) = mpsc::channel::<String>();
    let stdout_thread = std::thread::spawn(move || {
        // Split on \r as well: progress output often rewrites one line.
        for line in BufReader::new(stdout).split(b'\n').map_while(Result::ok) {
            for part in line.split(|&b| b == b'\r').filter(|p| !p.is_empty()) {
                if tx.send(String::from_utf8_lossy(part).into_owned()).is_err() {
                    return;
                }
            }
        }
    });
    let mut cancelled = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => on_line(&line),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if cancel.is_cancelled() && !cancelled {
            cancelled = true;
            let _ = child.kill();
        }
    }
    let status = child.wait()?;
    let _ = stdout_thread.join();
    let stderr = stderr_thread.join().unwrap_or_default();
    if cancelled {
        Err(ToolError::Cancelled)
    } else if status.success() {
        Ok(())
    } else {
        Err(ToolError::Failed { program, stderr })
    }
}

/// Runs `command` and returns everything it printed to stdout.
pub fn run_output(command: Command, cancel: &Cancel) -> Result<String, ToolError> {
    let mut out = String::new();
    run_lines(command, cancel, |line| {
        out.push_str(line);
        out.push('\n');
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn collects_lines_and_stderr() {
        let out = run_output(sh("echo one; printf 'two\\rthree\\n'"), &Cancel::new()).unwrap();
        assert_eq!(out, "one\ntwo\nthree\n");
        let err = run_output(sh("echo oops >&2; exit 3"), &Cancel::new()).unwrap_err();
        assert_eq!(err.to_string(), "sh failed: oops");
    }

    #[test]
    fn cancel_kills_the_program() {
        let cancel = Cancel::new();
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            c2.cancel();
        });
        let start = std::time::Instant::now();
        let err = run_lines(sh("exec sleep 30"), &cancel, |_| {}).unwrap_err();
        assert!(matches!(err, ToolError::Cancelled));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn missing_program_is_reported() {
        let err = run_output(Command::new("definitely-not-a-program-xyz"), &Cancel::new()).unwrap_err();
        assert!(err.to_string().starts_with("cannot start definitely-not-a-program-xyz"));
    }
}
