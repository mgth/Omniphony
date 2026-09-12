//! Slow work, off whoever asked for it.
//!
//! Reading a directory, walking a catalogue, downloading a file: none of it
//! belongs on the thread that draws, and all of it has the same shape — do the
//! work, hand back one answer, and ask for the frame that will show it. What
//! the answer means is the caller's business; getting off its thread is not.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use crate::host::commands::SharedState;

/// Run `work` on a thread of its own. The answer comes back on the receiver,
/// and the waker asks for a frame, since the answer usually arrives while
/// nothing on screen is moving.
pub fn run<T, F>(state: &Arc<SharedState>, work: F) -> Receiver<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = channel();
    let state = Arc::clone(state);
    let spawned = std::thread::Builder::new()
        .name("studio-job".into())
        .spawn(move || {
            let _ = tx.send(work());
            (state.waker)();
        });
    if let Err(e) = spawned {
        // A machine that cannot start a thread has worse problems, but the
        // caller must not be left waiting on a receiver nothing will fill:
        // the sender is dropped here, so its poll sees a disconnect.
        eprintln!("[jobs] could not start a worker thread: {e}");
    }
    rx
}
