//! Hosting plugins (VST3 and CLAP) in processes of their own.
//!
//! Each plugin runs in a child process, `shotgun-plugin` (the binary of
//! this crate), and is driven over a pipe: a plugin that crashes or hangs
//! costs its track's sound, not the project, and its editor window (X11)
//! belongs to that process. Decision and measurements:
//! docs/spikes/s7-plugin-host.md.
//!
//! This library is the engine's side: the protocol and [`Client`]. It
//! blocks on the pipe, so it is for the disk thread and job threads, never
//! for the realtime callback or the UI thread. It never blocks for good:
//! a plugin that does not answer within its time ([`Patience`]) is ended
//! like one that crashed.

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// Requests, one byte each; see the functions of [`Client`] for what follows them.
pub mod tag {
    pub const LOAD: u8 = b'L';
    pub const PROCESS: u8 = b'P';
    pub const RESET: u8 = b'R';
    pub const GET_STATE: u8 = b'G';
    pub const SET_STATE: u8 = b'S';
    pub const EDITOR_OPEN: u8 = b'E';
    pub const EDITOR_CLOSE: u8 = b'e';
    pub const QUIT: u8 = b'Q';
}

/// The most frames one request may carry.
pub const MAX_FRAMES: usize = 8192;

/// A note starting or ending inside a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteEvent {
    /// Frames from the start of the block.
    pub offset: u32,
    pub on: bool,
    /// MIDI note number: 60 is middle C.
    pub pitch: u8,
    /// 1 to 127.
    pub velocity: u8,
}

#[derive(Debug, thiserror::Error)]
pub enum PlugError {
    #[error("the plugin host program was not found beside the application ({0})")]
    NoHost(PathBuf),
    #[error("cannot start the plugin host: {0}")]
    Spawn(std::io::Error),
    #[error("the plugin did not load: {0}")]
    Load(String),
    #[error("the plugin's process stopped answering: {0}")]
    Gone(std::io::Error),
}

/// What a loaded plugin is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub name: String,
    /// It takes notes and makes sound (no audio input).
    pub instrument: bool,
    pub has_editor: bool,
}

/// The program that hosts a plugin: beside the running executable, or
/// where `SHOTGUN_PLUGIN_HOST` says.
pub fn host_program() -> PathBuf {
    if let Some(set) = std::env::var_os("SHOTGUN_PLUGIN_HOST") {
        return PathBuf::from(set);
    }
    let beside = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("shotgun-plugin")));
    beside.unwrap_or_else(|| PathBuf::from("shotgun-plugin"))
}

/// How long a plugin may take to answer before it counts as hung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patience {
    /// Loading it, and reading or setting its whole state: sample
    /// libraries take their time.
    pub slow: std::time::Duration,
    /// A block of sound, a reset, its window.
    pub block: std::time::Duration,
}

impl Default for Patience {
    fn default() -> Patience {
        // Long against any honest block (they take microseconds to a few
        // milliseconds), short against waiting for sound that never comes.
        Patience { slow: std::time::Duration::from_secs(180), block: std::time::Duration::from_secs(4) }
    }
}

/// One end of the pipe to the plugin's process that gives up waiting
/// after `patience` instead of blocking for good.
struct Patient<T> {
    inner: T,
    patience: std::time::Duration,
}

impl<T: std::os::fd::AsRawFd> Patient<T> {
    fn wait(&self, events: libc::c_short) -> std::io::Result<()> {
        let mut fd = libc::pollfd { fd: self.inner.as_raw_fd(), events, revents: 0 };
        let deadline = std::time::Instant::now() + self.patience;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            // SAFETY: `fd` is one valid pollfd for the duration of the call.
            let ready = unsafe { libc::poll(&mut fd, 1, left.as_millis().min(i32::MAX as u128) as i32) };
            match ready {
                0 => return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no answer in time")),
                n if n > 0 => return Ok(()),
                _ => {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
            }
        }
    }
}

impl<T: Read + std::os::fd::AsRawFd> Read for Patient<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.wait(libc::POLLIN)?;
        self.inner.read(buf)
    }
}

impl<T: Write + std::os::fd::AsRawFd> Write for Patient<T> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // (A process that has stopped reading lets the pipe fill up.)
        self.wait(libc::POLLOUT)?;
        // What the pipe takes without blocking, so that a large block cannot hang half-written.
        self.inner.write(&buf[..buf.len().min(4096)])
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// One plugin in its own process.
pub struct Client {
    child: Child,
    to: BufWriter<Patient<ChildStdin>>,
    from: BufReader<Patient<ChildStdout>>,
    patience: Patience,
    /// Its window is open: a plugin may hold everything up while a dialog
    /// of its own is on screen, which is not hanging.
    editor_open: bool,
    pub loaded: Loaded,
    /// The process is gone or stopped answering: sound passes untouched from then on.
    dead: bool,
    scratch: Vec<u8>,
}

fn read_u32(from: &mut impl Read) -> std::io::Result<u32> {
    let mut bytes = [0u8; 4];
    from.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_bytes(from: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let len = read_u32(from)? as usize;
    let mut bytes = vec![0u8; len];
    from.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_flag(from: &mut impl Read) -> std::io::Result<bool> {
    let mut byte = [0u8; 1];
    from.read_exact(&mut byte)?;
    Ok(byte[0] != 0)
}

/// Where the bridge to Windows plugins (yabridge) that comes with the
/// program is: the folder holding `bin/yabridge-host.exe` and
/// `lib/libyabridge-*.so`, found through the `PATH`.
///
/// A bridged plugin is a small loader that looks for the bridge itself in
/// the folders `NIX_PROFILES` names, and `yabridgectl` looks there for
/// the loaders. Both must find the bridge built against the Wine the
/// plugins were installed with, not whichever one a profile of the system
/// happens to hold, and must find one at all on a system without any.
pub fn bridge_root() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.join("yabridge-host.exe").is_file())
        .filter_map(|dir| dir.parent().map(Path::to_path_buf))
        .find(|root| root.join("lib/libyabridge-vst3.so").is_file())
}

impl Client {
    /// Starts the host program and loads the plugin at `plugin` (a
    /// `.vst3` bundle or `.clap` file) into it. Takes as long as the plugin takes to load.
    pub fn start(plugin: &Path) -> Result<Client, PlugError> {
        Client::start_with(plugin, Patience::default())
    }

    /// As [`Client::start`], with other limits for a plugin that does not answer.
    pub fn start_with(plugin: &Path, patience: Patience) -> Result<Client, PlugError> {
        let program = host_program();
        if program.components().count() > 1 && !program.is_file() {
            return Err(PlugError::NoHost(program));
        }
        let mut command = Command::new(&program);
        if let Some(root) = bridge_root() {
            command.env("NIX_PROFILES", root);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // What a plugin prints goes where the application's own messages go.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(PlugError::Spawn)?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(PlugError::Load("no pipes".to_owned()));
        };
        let mut to = BufWriter::new(Patient { inner: stdin, patience: patience.slow });
        let mut from = BufReader::new(Patient { inner: stdout, patience: patience.slow });
        let path = plugin.to_string_lossy();
        let sent = (|| {
            to.write_all(&[tag::LOAD])?;
            to.write_all(&(path.len() as u32).to_le_bytes())?;
            to.write_all(path.as_bytes())?;
            to.flush()?;
            let ok = read_flag(&mut from)?;
            let (instrument, has_editor) = (read_flag(&mut from)?, read_flag(&mut from)?);
            let text = String::from_utf8_lossy(&read_bytes(&mut from)?).into_owned();
            Ok::<_, std::io::Error>((ok, instrument, has_editor, text))
        })();
        match sent {
            Ok((true, instrument, has_editor, name)) => Ok(Client {
                child,
                to,
                from,
                patience,
                editor_open: false,
                loaded: Loaded { name, instrument, has_editor },
                dead: false,
                scratch: Vec::new(),
            }),
            Ok((false, _, _, why)) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(PlugError::Load(why))
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(PlugError::Gone(e))
            }
        }
    }

    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// The process the plugin runs in.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// How long the next answers may take.
    fn allow(&mut self, patience: std::time::Duration) {
        self.to.get_mut().patience = patience;
        self.from.get_mut().patience = patience;
    }

    fn gone<T>(&mut self, result: std::io::Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(e) => {
                if !self.dead {
                    log::error!("plugin {}: its process stopped answering ({e})", self.loaded.name);
                    // Hung rather than gone: ended, so that nothing of it lingers.
                    let _ = self.child.kill();
                }
                self.dead = true;
                None
            }
        }
    }

    /// Runs interleaved stereo through the plugin in place, with the
    /// notes of this block. Returns `false` (and leaves the sound as it
    /// is; an instrument's block is silence then) if the plugin is gone.
    pub fn process(&mut self, block: &mut [f32], events: &[NoteEvent]) -> bool {
        if self.dead {
            return false;
        }
        self.allow(if self.editor_open { self.patience.slow } else { self.patience.block });
        let mut done = true;
        // (Longer blocks go in pieces; the events are split by their offsets.)
        let mut start = 0usize;
        while start < block.len() / 2 {
            let frames = (block.len() / 2 - start).min(MAX_FRAMES);
            let piece = &mut block[start * 2..(start + frames) * 2];
            let inside: Vec<NoteEvent> = events
                .iter()
                .filter(|e| (e.offset as usize) >= start && (e.offset as usize) < start + frames)
                .map(|e| NoteEvent { offset: e.offset - start as u32, ..*e })
                .collect();
            let result = self.exchange(piece, &inside);
            done &= self.gone(result).is_some();
            start += frames;
        }
        done
    }

    fn exchange(&mut self, piece: &mut [f32], events: &[NoteEvent]) -> std::io::Result<()> {
        let frames = piece.len() / 2;
        self.to.write_all(&[tag::PROCESS])?;
        self.to.write_all(&(frames as u32).to_le_bytes())?;
        self.to.write_all(&(events.len() as u32).to_le_bytes())?;
        for event in events {
            self.to.write_all(&event.offset.to_le_bytes())?;
            self.to.write_all(&[event.on as u8, event.pitch, event.velocity, 0])?;
        }
        self.scratch.clear();
        self.scratch.extend(piece.iter().flat_map(|s| s.to_le_bytes()));
        self.to.write_all(&self.scratch)?;
        self.to.flush()?;
        if !read_flag(&mut self.from)? {
            return Err(std::io::Error::other("the plugin refused the block"));
        }
        self.scratch.resize(frames * 8, 0);
        self.from.read_exact(&mut self.scratch)?;
        for (sample, bytes) in piece.iter_mut().zip(self.scratch.chunks_exact(4)) {
            *sample = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        Ok(())
    }

    fn simple(&mut self, request: u8) -> Option<bool> {
        if self.dead {
            return None;
        }
        self.allow(self.patience.block);
        let result = (|| {
            self.to.write_all(&[request])?;
            self.to.flush()?;
            read_flag(&mut self.from)
        })();
        self.gone(result)
    }

    /// Silences what is sounding (a jump on the timeline).
    pub fn reset(&mut self) {
        self.simple(tag::RESET);
    }

    /// Shows the plugin's own window. `false` if it has none.
    pub fn open_editor(&mut self) -> bool {
        self.editor_open = self.simple(tag::EDITOR_OPEN).unwrap_or(false);
        self.editor_open
    }

    pub fn close_editor(&mut self) {
        self.simple(tag::EDITOR_CLOSE);
        self.editor_open = false;
    }

    /// Everything the plugin is set to, as it wants to store it.
    pub fn state(&mut self) -> Option<Vec<u8>> {
        if self.dead {
            return None;
        }
        self.allow(self.patience.slow);
        let result = (|| {
            self.to.write_all(&[tag::GET_STATE])?;
            self.to.flush()?;
            read_bytes(&mut self.from)
        })();
        self.gone(result)
    }

    pub fn set_state(&mut self, state: &[u8]) -> bool {
        if self.dead {
            return false;
        }
        self.allow(self.patience.slow);
        let result = (|| {
            self.to.write_all(&[tag::SET_STATE])?;
            self.to.write_all(&(state.len() as u32).to_le_bytes())?;
            self.to.write_all(state)?;
            self.to.flush()?;
            read_flag(&mut self.from)
        })();
        self.gone(result).unwrap_or(false)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if !self.dead {
            let _ = self.to.write_all(&[tag::QUIT]);
            let _ = self.to.flush();
        }
        // A plugin that does not leave by itself within a moment is ended.
        for _ in 0..50 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
