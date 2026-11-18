//! Worker threads. The UI thread hands closures over and gets results back
//! as event-loop events; it never waits.

use std::thread::JoinHandle;

use crossbeam_channel::Sender;

type Job = Box<dyn FnOnce() + Send>;

/// Runs jobs one after another on its own thread. Project create, open and
/// close go through one of these so that a close has always finished
/// writing before a later open reads the same folder.
pub struct Serial {
    tx: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl Serial {
    pub fn spawn(name: &str) -> std::io::Result<Serial> {
        let (tx, rx) = crossbeam_channel::unbounded::<Job>();
        let thread = std::thread::Builder::new().name(name.to_owned()).spawn(move || {
            for job in rx {
                job();
            }
        })?;
        Ok(Serial { tx: Some(tx), thread: Some(thread) })
    }

    pub fn run(&self, job: impl FnOnce() + Send + 'static) {
        if let Some(tx) = &self.tx {
            // The thread only ends when the sender is dropped in `finish`.
            let _ = tx.send(Box::new(job));
        }
    }

    /// Runs the queued jobs to the end and stops the thread.
    pub fn finish(mut self) {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
