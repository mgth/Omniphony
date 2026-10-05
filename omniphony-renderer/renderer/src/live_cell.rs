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
//! Only writers free a value. Every value a write replaces is kept in the
//! cell, and each write frees the kept ones no reader holds any more, so a
//! reader — the render thread — never drops the last reference, however many
//! writes land while it holds a value.
//!
//! A counter that tells readers the value changed (a generation a cache is
//! keyed on) must be bumped after the write guard is dropped, and read before
//! the value is loaded: a reader that sees the bump then finds the data. A
//! bump inside the guard could be seen with the previous value, and the cache
//! would record the new generation over stale data. Debug builds check the
//! writer's side through [`write_held_on_this_thread`].
//!
//! [`LiveParams`]: crate::live_params::LiveParams

use arc_swap::{ArcSwap, Guard};
use parking_lot::{Mutex, MutexGuard};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

pub struct LiveCell<T> {
    current: ArcSwap<T>,
    /// Serializes writers. Holds the replaced values a reader may still hold.
    writer: Mutex<Vec<Arc<T>>>,
}

/// A published value, held for as long as the guard lives. Later writes do
/// not change what it shows.
pub struct LiveRead<T>(Guard<Arc<T>>);

/// Exclusive write access. Shows the published value until it is first
/// borrowed mutably, which takes a copy; the copy is published on drop.
pub struct LiveWrite<'a, T: Clone> {
    cell: &'a LiveCell<T>,
    retired: MutexGuard<'a, Vec<Arc<T>>>,
    /// The value this write started from. Always `Some` until the drop,
    /// which releases it before freeing what no one holds.
    base: Option<Arc<T>>,
    draft: Option<T>,
}

#[cfg(debug_assertions)]
thread_local! {
    static WRITES_HELD: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Whether this thread holds a [`LiveWrite`] of any cell. Always `false` in
/// release builds, where it is not tracked.
pub fn write_held_on_this_thread() -> bool {
    #[cfg(debug_assertions)]
    return WRITES_HELD.with(|held| held.get() > 0);
    #[cfg(not(debug_assertions))]
    false
}

impl<T> LiveCell<T> {
    pub fn new(value: T) -> Self {
        Self {
            current: ArcSwap::from_pointee(value),
            writer: Mutex::new(Vec::new()),
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

    fn write_with<'a>(&'a self, retired: MutexGuard<'a, Vec<Arc<T>>>) -> LiveWrite<'a, T> {
        #[cfg(debug_assertions)]
        WRITES_HELD.with(|held| held.set(held.get() + 1));
        // Writers are serialized by `retired`: nothing can publish between
        // this load and this guard's own publication.
        LiveWrite {
            cell: self,
            base: Some(self.current.load_full()),
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
        match &self.draft {
            Some(draft) => draft,
            None => self.base.as_ref().expect("held until drop"),
        }
    }
}

impl<T: Clone> DerefMut for LiveWrite<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        let base = self.base.as_ref().expect("held until drop");
        self.draft.get_or_insert_with(|| T::clone(base))
    }
}

impl<T: Clone> Drop for LiveWrite<'_, T> {
    fn drop(&mut self) {
        if let Some(draft) = self.draft.take() {
            // `swap` returns once every reader of the replaced value holds
            // a counted reference to it (arc-swap settles its borrowed
            // loads), so the counts below see all of them; and a replaced
            // value is never handed out again, so no count can grow.
            let replaced = self.cell.current.swap(Arc::new(draft));
            self.retired.push(replaced);
        }
        self.base = None;
        // Free, here on the writer's thread, every replaced value only the
        // cell still holds. A value a reader holds stays until a later
        // write finds it released. The mutex is released after this, so
        // the next writer sees the new value.
        self.retired.retain(|value| Arc::strong_count(value) > 1);
        #[cfg(debug_assertions)]
        WRITES_HELD.with(|held| held.set(held.get() - 1));
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

    /// Records the thread each value is dropped on.
    #[derive(Clone)]
    struct Tracked {
        id: u32,
        drops: Arc<std::sync::Mutex<Vec<(u32, String)>>>,
    }

    impl Drop for Tracked {
        fn drop(&mut self) {
            let thread = std::thread::current().name().unwrap_or("").to_owned();
            self.drops.lock().unwrap().push((self.id, thread));
        }
    }

    /// A reader holding a value across several writes is never the one that
    /// frees it: the writes keep it while it is held, and the first write
    /// after the reader let go frees it, on the writer's thread.
    #[test]
    fn a_reader_never_frees_a_value() {
        let drops = Arc::default();
        let cell = Arc::new(LiveCell::new(Tracked {
            id: 0,
            drops: Arc::clone(&drops),
        }));
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let reader = {
            let cell = Arc::clone(&cell);
            std::thread::Builder::new()
                .name("reader".into())
                .spawn(move || {
                    let v0 = cell.read();
                    held_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    assert_eq!(v0.id, 0);
                })
                .unwrap()
        };
        held_rx.recv().unwrap();
        // Two writes while the reader holds the first value.
        cell.write().id = 1;
        cell.write().id = 2;
        release_tx.send(()).unwrap();
        reader.join().unwrap();
        cell.write().id = 3;

        let drops = drops.lock().unwrap().clone();
        // Every replaced value is dropped by the writer (the test thread),
        // never by "reader".
        assert!(
            drops.iter().all(|(_, thread)| thread != "reader"),
            "{drops:?}"
        );
        for id in [0, 1, 2] {
            assert!(
                drops.iter().any(|(dropped, _)| *dropped == id),
                "value {id} still kept: {drops:?}"
            );
        }
    }

    #[test]
    fn a_write_guard_is_tracked_on_its_thread() {
        let cell = LiveCell::new(0);
        assert!(!write_held_on_this_thread());
        let w = cell.write();
        assert_eq!(write_held_on_this_thread(), cfg!(debug_assertions));
        drop(w);
        assert!(!write_held_on_this_thread());
        drop(cell.try_write());
        assert!(!write_held_on_this_thread());
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
