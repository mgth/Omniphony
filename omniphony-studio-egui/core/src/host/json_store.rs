//! Atomic JSON persistence and a coalescing, owned background writer.
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub fn load<T: DeserializeOwned + Default>(path: &Path) -> (T, Option<String>) {
    match std::fs::read(path) {
        Ok(data) => match serde_json::from_slice(&data) {
            Ok(value) => (value, None),
            Err(error) => (T::default(), Some(format!("{}: {error}", path.display()))),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (T::default(), None),
        Err(error) => (T::default(), Some(format!("{}: {error}", path.display()))),
    }
}

fn prepared<T: Serialize>(path: &Path, value: &T) -> Result<tempfile::NamedTempFile, String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, value).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    Ok(file)
}

/// Replace only after serialization and syncing succeed. The temporary file
/// lives beside the destination, so replacement stays on the same filesystem.
pub fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    prepared(path, value)?
        .persist(path)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Copy a validated legacy file once; never overwrite an existing destination
/// (including a malformed one), and leave the original available for rollback.
pub fn migrate<T: Serialize + DeserializeOwned>(
    source: &Path,
    target: &Path,
) -> Result<(), String> {
    if target.exists() || source == target {
        return Ok(());
    }
    let data = match std::fs::read(source) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let value: T = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    match prepared(target, &value)?.persist_noclobber(target) {
        Ok(_) => Ok(()),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

struct State<T> {
    pending: Option<(T, Instant)>,
    closing: bool,
    error: Option<String>,
}

/// At most one pending snapshot plus one write in progress. The deadline runs
/// in the core even when the window is minimized and produces no frames.
pub struct Writer<T> {
    shared: Arc<(Mutex<State<T>>, Condvar)>,
    worker: Option<JoinHandle<()>>,
    debounce: Duration,
}
impl<T: Serialize + Send + 'static> Writer<T> {
    /// Preserve unreadable or newer documents until the user recovers them.
    /// This writer owns no thread and never accepts snapshots for persistence.
    pub fn read_only(error: String) -> Self {
        Self {
            shared: Arc::new((
                Mutex::new(State {
                    pending: None,
                    closing: true,
                    error: Some(error),
                }),
                Condvar::new(),
            )),
            worker: None,
            debounce: Duration::ZERO,
        }
    }

    pub fn new(
        path: PathBuf,
        debounce: Duration,
        wake: crate::osc::Waker,
        error: Option<String>,
    ) -> std::io::Result<Self> {
        Self::with_save(debounce, wake, error, move |value| save(&path, value))
    }
    fn with_save(
        debounce: Duration,
        wake: crate::osc::Waker,
        error: Option<String>,
        save: impl Fn(&T) -> Result<(), String> + Send + 'static,
    ) -> std::io::Result<Self> {
        let shared = Arc::new((
            Mutex::new(State {
                pending: None,
                closing: false,
                error,
            }),
            Condvar::new(),
        ));
        let worker_state = shared.clone();
        let worker = std::thread::Builder::new()
            .name("studio-preferences".into())
            .spawn(move || {
                let (lock, changed) = &*worker_state;
                loop {
                    let mut state = lock.lock().unwrap();
                    while !state.closing && state.pending.is_none() {
                        state = changed.wait(state).unwrap();
                    }
                    if !state.closing
                        && let Some((_, due)) = &state.pending
                    {
                        let remaining = due.saturating_duration_since(Instant::now());
                        if !remaining.is_zero() {
                            drop(changed.wait_timeout(state, remaining).unwrap());
                            continue;
                        }
                    }
                    let Some((value, _)) = state.pending.take() else {
                        break;
                    };
                    drop(state);
                    let result = save(&value);
                    let mut state = lock.lock().unwrap();
                    state.error = result.err();
                    if state.error.is_some() && !state.closing && state.pending.is_none() {
                        // Retain the failed snapshot; retry with a bounded cadence.
                        state.pending = Some((value, Instant::now() + Duration::from_secs(5)));
                    }
                    drop(state);
                    wake();
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
            debounce,
        })
    }
    pub fn submit(&self, value: T) {
        let mut state = self.shared.0.lock().unwrap();
        if !state.closing {
            state.pending = Some((value, Instant::now() + self.debounce));
            self.shared.1.notify_one();
        }
    }
}
impl<T> Writer<T> {
    pub fn error(&self) -> Option<String> {
        self.shared.0.lock().unwrap().error.clone()
    }
    /// Finish the newest snapshot, bypassing debounce, and join the worker.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.shared.0.lock().unwrap().closing = true;
        self.shared.1.notify_one();
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| "preferences worker panicked".to_string())?;
        }
        self.error().map_or(Ok(()), Err)
    }
}
impl<T> Drop for Writer<T> {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            log::error!("[prefs] {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    #[test]
    fn read_only_writer_keeps_the_error_and_discards_all_submissions() {
        let mut writer = Writer::read_only("Newer preference format".into());
        writer.submit(42_u32);
        assert!(writer.shared.0.lock().unwrap().pending.is_none());
        assert!(writer.worker.is_none());
        assert_eq!(writer.shutdown(), Err("Newer preference format".into()));
    }

    #[test]
    fn failed_serialization_preserves_previous_document() {
        struct Invalid;
        impl Serialize for Invalid {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("invalid preferences"))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("prefs.json");
        save(&file, &vec![1, 2]).unwrap();
        let original = std::fs::read(&file).unwrap();
        assert!(save(&file, &Invalid).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), original);
        save(&file, &vec![3]).unwrap();
        assert_eq!(load::<Vec<u32>>(&file).0, vec![3]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn migration_is_repeatable_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.json");
        let new = dir.path().join("new.json");
        save(&old, &vec![1]).unwrap();
        migrate::<Vec<u32>>(&old, &new).unwrap();
        save(&new, &vec![2]).unwrap();
        migrate::<Vec<u32>>(&old, &new).unwrap();
        assert_eq!(load::<Vec<u32>>(&new).0, vec![2]);
        assert_eq!(load::<Vec<u32>>(&old).0, vec![1]);
        std::fs::write(&new, b"broken").unwrap();
        migrate::<Vec<u32>>(&old, &new).unwrap();
        assert!(load::<Vec<u32>>(&new).1.is_some());
    }
    #[test]
    fn immediate_shutdown_flushes_latest_snapshot_once() {
        let (tx, rx) = mpsc::channel();
        let mut writer = Writer::with_save(
            Duration::from_secs(60),
            Arc::new(|| {}),
            None,
            move |value: &u32| {
                tx.send(*value).unwrap();
                Ok(())
            },
        )
        .unwrap();
        for value in 0..100 {
            writer.submit(value);
        }
        writer.shutdown().unwrap();
        writer.shutdown().unwrap();
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![99]);
    }
    #[test]
    fn changes_during_slow_write_are_coalesced_and_failures_are_visible() {
        let (started, rx) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let mut writer =
            Writer::with_save(Duration::ZERO, Arc::new(|| {}), None, move |value: &u32| {
                started.send(*value).unwrap();
                if *value == 1 {
                    wait.recv().unwrap();
                }
                Err(format!("write {value} failed"))
            })
            .unwrap();
        writer.submit(1);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        writer.submit(2);
        writer.submit(3);
        // Release the blocked write only after shutdown has begun. Otherwise
        // write 3 can fail before closing and legitimately be retried by the
        // final flush, making the attempt count depend on thread scheduling.
        let shared = writer.shared.clone();
        let releaser = std::thread::spawn(move || {
            let (lock, changed) = &*shared;
            let mut state = lock.lock().unwrap();
            while !state.closing {
                state = changed.wait(state).unwrap();
            }
            drop(state);
            release.send(()).unwrap();
        });
        assert_eq!(writer.shutdown(), Err("write 3 failed".into()));
        releaser.join().unwrap();
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![3]);
    }
}
