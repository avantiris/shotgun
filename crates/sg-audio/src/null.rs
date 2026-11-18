//! A backend without a device: a thread that renders blocks at the pace a
//! sound card would and throws them away. Keeps the clock and the whole
//! engine running where no audio output is wanted (automated runs) or none
//! is available.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::graph::Rt;
use crate::{AudioError, SAMPLE_RATE, monotonic_ns};

pub struct Null {
    active: Arc<AtomicBool>,
    quit: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Null {
    pub fn start(mut rt: Rt, quantum: u32) -> Result<Null, AudioError> {
        let active = Arc::new(AtomicBool::new(false));
        let quit = Arc::new(AtomicBool::new(false));
        let (thread_active, thread_quit) = (active.clone(), quit.clone());
        let thread = std::thread::Builder::new().name("sg-audio-null".into()).spawn(move || {
            let mut block = vec![0.0f32; quantum as usize * 2];
            let period = Duration::from_nanos(quantum as u64 * 1_000_000_000 / SAMPLE_RATE as u64);
            let mut next = std::time::Instant::now();
            while !thread_quit.load(Ordering::Relaxed) {
                if !thread_active.load(Ordering::Relaxed) {
                    // Parked until playback starts; `unpark` wakes it.
                    std::thread::park_timeout(Duration::from_secs(3600));
                    next = std::time::Instant::now();
                    continue;
                }
                rt.process(&mut block, monotonic_ns(), 0);
                next += period;
                std::thread::sleep(next.saturating_duration_since(std::time::Instant::now()));
            }
        })?;
        Ok(Null { active, quit, thread: Some(thread) })
    }

    pub fn activator(&self) -> Box<dyn Fn(bool) + Send> {
        let active = self.active.clone();
        let thread = self.thread.as_ref().map(|t| t.thread().clone());
        Box::new(move |on| {
            active.store(on, Ordering::Relaxed);
            if let Some(thread) = &thread {
                thread.unpark();
            }
        })
    }
}

impl Drop for Null {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}
