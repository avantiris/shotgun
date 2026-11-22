//! A MIDI keyboard as an input (docs/spec/09-daw.md, M14): every MIDI
//! source the machine has is listened to through the ALSA sequencer, and
//! its notes are handed on as they arrive. Keyboards plugged in later are
//! taken up as well.
//!
//! One thread of its own, asleep in `poll` between notes. Nothing here
//! touches the realtime callback.

use std::ffi::CString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alsa::seq::{Addr, EvNote, EventType, PortCap, PortInfo, PortSubscribe, PortType, Seq};

use crate::AudioError;

/// Listens to the MIDI keyboards until dropped.
pub struct MidiIn {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The sequencer's address of the port notes arrive at (for tests, which send to it).
    pub address: (i32, i32),
}

fn failed(what: &str, e: alsa::Error) -> AudioError {
    AudioError::Midi(format!("{what}: {e}"))
}

/// Has `from` deliver its notes to `to`. A source that cannot be listened to is left out.
fn listen(seq: &Seq, from: Addr, to: Addr) {
    let Ok(wish) = PortSubscribe::empty() else { return };
    wish.set_sender(from);
    wish.set_dest(to);
    if let Err(e) = seq.subscribe_port(&wish) {
        log::debug!("MIDI source {}:{} not listened to: {e}", from.client, from.port);
    }
}

/// Whether notes can be had from this port: it can be read and subscribed
/// to, and is not one of the sequencer's own (timer, announcements).
fn is_source(info: &PortInfo, own: i32) -> bool {
    let caps = info.get_capability();
    caps.contains(PortCap::READ | PortCap::SUBS_READ)
        && !caps.contains(PortCap::NO_EXPORT)
        && info.get_client() != own
        && info.get_client() != 0
}

impl MidiIn {
    /// Starts listening, as the sequencer client `name`. `on_note` gets
    /// (key down or up, pitch, velocity) on the listening thread; a key
    /// down with velocity 0 counts as up, as keyboards send it.
    pub fn start(name: &str, on_note: impl Fn(bool, u8, u8) + Send + 'static) -> Result<MidiIn, AudioError> {
        let seq = Seq::open(None, Some(alsa::Direction::Capture), true).map_err(|e| failed("no MIDI sequencer", e))?;
        let name = CString::new(name).map_err(|e| AudioError::Midi(e.to_string()))?;
        seq.set_client_name(&name).map_err(|e| failed("cannot name the MIDI client", e))?;
        let port = seq
            .create_simple_port(
                &name,
                PortCap::WRITE | PortCap::SUBS_WRITE,
                PortType::MIDI_GENERIC | PortType::APPLICATION,
            )
            .map_err(|e| failed("cannot open a MIDI port", e))?;
        let own = seq.client_id().map_err(|e| failed("no MIDI client", e))?;
        let here = Addr { client: own, port };
        // Every source there is now, and the announcements of those to come.
        for client in alsa::seq::ClientIter::new(&seq) {
            for info in alsa::seq::PortIter::new(&seq, client.get_client()) {
                if is_source(&info, own) {
                    listen(&seq, info.addr(), here);
                }
            }
        }
        listen(&seq, Addr::system_announce(), here);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let run = move || {
            use alsa::poll::Descriptors;
            let Ok(mut fds) = (&seq, Some(alsa::Direction::Capture)).get() else { return };
            let mut sustain = Sustain::default();
            let mut input = seq.input();
            while !stopped.load(Ordering::Relaxed) {
                // (Woken a few times a second to see whether to stop.)
                match alsa::poll::poll(&mut fds, 200) {
                    Ok(_) => {}
                    // (A signal arrived during the wait: wait again.)
                    Err(e) if e.errno() == libc::EINTR => continue,
                    Err(_) => break,
                }
                while input.event_input_pending(true).is_ok_and(|waiting| waiting > 0) {
                    let Ok(event) = input.event_input() else { break };
                    match event.get_type() {
                        EventType::Noteon | EventType::Noteoff => {
                            let Some(note) = event.get_data::<EvNote>() else { continue };
                            let down = event.get_type() == EventType::Noteon && note.velocity > 0;
                            let (pitch, velocity) = (note.note.min(127), if down { note.velocity.min(127) } else { 0 });
                            for (down, pitch) in sustain.key(down, pitch).into_iter().flatten() {
                                on_note(down, pitch, if down { velocity } else { 0 });
                            }
                        }
                        // The sustain pedal (controller 64): down from the middle of its way.
                        EventType::Controller => {
                            let Some(control) = event.get_data::<alsa::seq::EvCtrl>() else { continue };
                            if control.param == 64 {
                                for pitch in sustain.pedal(control.value >= 64) {
                                    on_note(false, pitch, 0);
                                }
                            }
                        }
                        // A keyboard was plugged in: listen to it too.
                        EventType::PortStart => {
                            let Some(from) = event.get_data::<Addr>() else { continue };
                            if seq.get_any_port_info(from).is_ok_and(|info| is_source(&info, own)) {
                                listen(&seq, from, here);
                            }
                        }
                        _ => {}
                    }
                }
            }
        };
        let thread = std::thread::Builder::new().name("sg-audio-midi".into()).spawn(run)?;
        Ok(MidiIn { stop, thread: Some(thread), address: (own, port) })
    }
}

/// The sustain pedal, done here and not in the instrument (`PROPOSED`):
/// while it is down, keys that are let go keep sounding, and end when it
/// comes up. That works the same for the built-in instruments and for
/// every plugin, and a take records what was heard. What a sampled piano
/// does with its own pedal (the strings ringing along, half pedalling) it
/// does not do this way.
#[derive(Default)]
pub struct Sustain {
    down: bool,
    /// Keys that were let go while the pedal was down and still sound.
    ringing: Vec<u8>,
}

impl Sustain {
    /// A key went down or up: the notes to start and end for it, in order.
    pub fn key(&mut self, down: bool, pitch: u8) -> [Option<(bool, u8)>; 2] {
        let ringing = self.ringing.iter().position(|p| *p == pitch);
        match (down, ringing) {
            // Struck again while it still rings: it ends and begins anew.
            (true, Some(at)) => {
                self.ringing.remove(at);
                [Some((false, pitch)), Some((true, pitch))]
            }
            (true, None) => [Some((true, pitch)), None],
            (false, _) if self.down => {
                if ringing.is_none() {
                    self.ringing.push(pitch);
                }
                [None, None]
            }
            (false, _) => [Some((false, pitch)), None],
        }
    }

    /// The pedal went down or came up: the keys that end now.
    pub fn pedal(&mut self, down: bool) -> Vec<u8> {
        self.down = down;
        if down { Vec::new() } else { std::mem::take(&mut self.ringing) }
    }
}

impl Drop for MidiIn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Stands in for a keyboard (tests and `sg-bench midi`): sends notes to
/// the listening port of the sequencer client called `to` (the program
/// is "Shotgun"; an automated run is named after its window class). Each of `notes` is (pitch, velocity,
/// milliseconds held), played one after the other with `gap` between them.
pub fn play(to: &str, notes: &[(u8, u8, u64)], gap: std::time::Duration) -> Result<(), AudioError> {
    use alsa::seq::Event;
    let seq = Seq::open(None, Some(alsa::Direction::Playback), false).map_err(|e| failed("no MIDI sequencer", e))?;
    let name = CString::new("Shotgun stand-in keyboard").map_err(|e| AudioError::Midi(e.to_string()))?;
    seq.set_client_name(&name).map_err(|e| failed("cannot name the MIDI client", e))?;
    let port = seq
        .create_simple_port(&name, PortCap::READ | PortCap::SUBS_READ, PortType::MIDI_GENERIC | PortType::APPLICATION)
        .map_err(|e| failed("cannot open a MIDI port", e))?;
    let own = seq.client_id().map_err(|e| failed("no MIDI client", e))?;
    let target = alsa::seq::ClientIter::new(&seq)
        .filter(|client| client.get_client() != own && client.get_name().is_ok_and(|n| n == to))
        .find_map(|client| {
            alsa::seq::PortIter::new(&seq, client.get_client())
                .find(|port| port.get_capability().contains(PortCap::WRITE))
                .map(|port| port.addr())
        })
        .ok_or_else(|| AudioError::Midi(format!("no MIDI client called {to:?} is listening")))?;
    let send = |kind: EventType, note: u8, velocity: u8| {
        let data = EvNote { channel: 0, note, velocity, off_velocity: 0, duration: 0 };
        let mut event = Event::new(kind, &data);
        event.set_source(port);
        event.set_dest(target);
        event.set_direct();
        seq.event_output(&mut event).and_then(|_| seq.drain_output()).map_err(|e| failed("cannot send", e))
    };
    for &(pitch, velocity, held) in notes {
        send(EventType::Noteon, pitch, velocity)?;
        std::thread::sleep(std::time::Duration::from_millis(held));
        send(EventType::Noteoff, pitch, 0)?;
        std::thread::sleep(gap);
    }
    Ok(())
}

#[cfg(test)]
mod sustain_tests {
    use super::Sustain;

    #[test]
    fn the_pedal_holds_what_is_let_go_until_it_comes_up() {
        let mut sustain = Sustain::default();
        // Without the pedal a key is what it is.
        assert_eq!(sustain.key(true, 60), [Some((true, 60)), None]);
        assert_eq!(sustain.key(false, 60), [Some((false, 60)), None]);
        // Pedal down: two keys struck and let go keep sounding.
        assert!(sustain.pedal(true).is_empty());
        sustain.key(true, 60);
        sustain.key(true, 64);
        assert_eq!(sustain.key(false, 60), [None, None]);
        assert_eq!(sustain.key(false, 64), [None, None]);
        // One of them struck again: it ends and starts anew, and is held by the hand now.
        assert_eq!(sustain.key(true, 60), [Some((false, 60)), Some((true, 60))]);
        // Pedal up: what only the pedal held ends; the key still down does not.
        assert_eq!(sustain.pedal(false), vec![64]);
        assert_eq!(sustain.key(false, 60), [Some((false, 60)), None]);
    }
}
