//! A value read lock-free and written under a mutex: how [`LiveParams`] is
//! shared between the render thread and the control threads.
//!
//! Readers load the current `Arc` from an [`ArcSwap`]: no lock, so a control
//! write in progress — or one queued behind a slow reader, which is what a
//! readers-writer lock does — never stalls the render thread. Writers take a
//! mutex, edit a private copy and publish it when their guard drops; a write
//! guard that was only read through publishes nothing and copies nothing.
//!
//! A write costs one clone of the value. Writes come from the control plane
//! (OSC, config seeding, profile switches) at human rate; the render thread
//! writes only on rare events and uses [`LiveCell::try_write`], which never
//! waits for another writer.
//!
//! The value a write replaces stays in the cell until the next write, so a
//! render thread still reading it when it is replaced is not, in the common
//! case, the thread that frees it.
//!
//! [`LiveParams`]: crate::live_params::LiveParams

use arc_swap::{ArcSwap, Guard};
use parking_lot::{Mutex, MutexGuard};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

pub struct LiveCell<T> {
    current: ArcSwap<T>,
    /// Serializes writers. Holds the value the last write replaced.
    writer: Mutex<Option<Arc<T>>>,
}

/// A published value, held for as long as the guard lives. Later writes do
/// not change what it shows.
pub struct LiveRead<T>(Guard<Arc<T>>);

/// Exclusive write access. Shows the published value until it is first
/// borrowed mutably, which takes a copy; the copy is published on drop.
pub struct LiveWrite<'a, T: Clone> {
    cell: &'a LiveCell<T>,
    retired: MutexGuard<'a, Option<Arc<T>>>,
    base: Arc<T>,
    draft: Option<T>,
}

impl<T> LiveCell<T> {
    pub fn new(value: T) -> Self {
        Self {
            current: ArcSwap::from_pointee(value),
            writer: Mutex::new(None),
        }
    }

    /// The published value. Lock-free and allocation-free; meant to be held
    /// briefly (a block, a message). Use [`load_full`](Self::load_full) to
    /// keep it longer.
    pub fn read(&self) -> LiveRead<T> {
        LiveRead(self.current.load())
    }

    /// The published value as an owned `Arc`.
    pub fn load_full(&self) -> Arc<T> {
        self.current.load_full()
    }
}

impl<T: Clone> LiveCell<T> {
    /// Wait for the other writers, then edit. Readers are never blocked.
    pub fn write(&self) -> LiveWrite<'_, T> {
        self.write_with(self.writer.lock())
    }

    /// Edit if no other writer holds the cell, without waiting: the form the
    /// render thread uses.
    pub fn try_write(&self) -> Option<LiveWrite<'_, T>> {
        self.writer
            .try_lock()
            .map(|retired| self.write_with(retired))
    }

    fn write_with<'a>(&'a self, retired: MutexGuard<'a, Option<Arc<T>>>) -> LiveWrite<'a, T> {
        // Writers are serialized by `retired`: nothing can publish between
        // this load and this guard's own publication.
        LiveWrite {
            cell: self,
            base: self.current.load_full(),
            retired,
            draft: None,
        }
    }
}

impl<T: Default> Default for LiveCell<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Deref for LiveRead<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Clone> Deref for LiveWrite<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.draft.as_ref().unwrap_or(&self.base)
    }
}

impl<T: Clone> DerefMut for LiveWrite<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        let base = &self.base;
        self.draft.get_or_insert_with(|| T::clone(base))
    }
}

impl<T: Clone> Drop for LiveWrite<'_, T> {
    fn drop(&mut self) {
        if let Some(draft) = self.draft.take() {
            let replaced = self.cell.current.swap(Arc::new(draft));
            // Frees the value retired by the previous write, here on the
            // writer's thread, and keeps this one alive for any reader still
            // holding it. The mutex is released after this, so the next
            // writer sees the new value.
            *self.retired = Some(replaced);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn write_publishes_on_drop() {
        let cell = LiveCell::new(vec![1]);
        {
            let mut w = cell.write();
            w.push(2);
            assert_eq!(*cell.read(), vec![1], "not published before the drop");
        }
        assert_eq!(*cell.read(), vec![1, 2]);
    }

    #[test]
    fn a_read_keeps_its_value_across_a_write() {
        let cell = LiveCell::new(1);
        let before = cell.read();
        *cell.write() = 2;
        assert_eq!(*before, 1);
        assert_eq!(*cell.read(), 2);
    }

    #[test]
    fn a_write_guard_read_through_publishes_nothing() {
        let cell = LiveCell::new(5);
        let published = cell.load_full();
        assert_eq!(*cell.write(), 5);
        assert!(Arc::ptr_eq(&published, &cell.load_full()));
    }

    #[test]
    fn try_write_does_not_wait_for_a_writer() {
        let cell = LiveCell::new(0);
        let w = cell.write();
        assert!(cell.try_write().is_none());
        drop(w);
        *cell.try_write().expect("free") = 3;
        assert_eq!(*cell.read(), 3);
    }

    #[test]
    fn writes_from_several_threads_are_not_lost() {
        let cell = Arc::new(LiveCell::new(0u32));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let cell = Arc::clone(&cell);
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        *cell.write() += 1;
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(*cell.read(), 4000);
    }

    /// The property the cell exists for: a reader on another thread gets the
    /// value while a writer is mid-edit.
    #[test]
    fn a_reader_is_not_blocked_by_an_open_write() {
        let cell = Arc::new(LiveCell::new(7));
        let mut w = cell.write();
        *w = 8;
        let (tx, rx) = mpsc::channel();
        let reader = {
            let cell = Arc::clone(&cell);
            std::thread::spawn(move || tx.send(*cell.read()).unwrap())
        };
        let seen = rx.recv_timeout(Duration::from_secs(5));
        drop(w);
        reader.join().unwrap();
        assert_eq!(seen, Ok(7));
    }
}
