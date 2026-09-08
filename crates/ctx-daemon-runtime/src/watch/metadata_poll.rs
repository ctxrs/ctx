//! macOS fallback for appends to files that were already dirty when watched.
//! Metadata only: no content hashing and no symlink traversal.
use notify::{
    event::{CreateKind, MetadataKind, ModifyKind, RemoveKind},
    Event, EventKind, RecursiveMode,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, SystemTime},
};

type Handler = Arc<dyn Fn(notify::Result<Event>) + Send + Sync>;
#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
    kind: u8,
}
struct Root {
    recursive: bool,
    entries: BTreeMap<PathBuf, Stamp>,
}
pub(super) struct MetadataWatcher {
    roots: Arc<Mutex<BTreeMap<PathBuf, Root>>>,
    stop: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}
impl MetadataWatcher {
    pub(super) fn new(handler: Handler) -> notify::Result<Self> {
        let roots = Arc::new(Mutex::new(BTreeMap::<PathBuf, Root>::new()));
        let watched = Arc::clone(&roots);
        let (stop, receive) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("ctx-metadata-watch".to_owned())
            .spawn(move || {
                while matches!(
                    receive.recv_timeout(Duration::from_secs(5)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    let mut roots = watched
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    for (path, root) in roots.iter_mut() {
                        let current = match scan(path, root.recursive) {
                            Ok(current) => current,
                            Err(error) => {
                                handler(Err(error));
                                continue;
                            }
                        };
                        for (path, stamp) in &current {
                            let kind = match root.entries.get(path) {
                                None => Some(EventKind::Create(CreateKind::Any)),
                                Some(old) if old != stamp => Some(EventKind::Modify(
                                    ModifyKind::Metadata(MetadataKind::WriteTime),
                                )),
                                _ => None,
                            };
                            if let Some(kind) = kind {
                                handler(Ok(Event::new(kind).add_path(path.clone())));
                            }
                        }
                        for path in root
                            .entries
                            .keys()
                            .filter(|path| !current.contains_key(*path))
                        {
                            handler(Ok(Event::new(EventKind::Remove(RemoveKind::Any))
                                .add_path(path.clone())));
                        }
                        root.entries = current;
                    }
                }
            })
            .map_err(notify::Error::io)?;
        Ok(Self {
            roots,
            stop,
            worker: Some(worker),
        })
    }
    pub(super) fn watch(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()> {
        let recursive = matches!(mode, RecursiveMode::Recursive);
        let entries = scan(path, recursive)?;
        self.roots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.to_path_buf(), Root { recursive, entries });
        Ok(())
    }
    pub(super) fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        self.roots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(path);
        Ok(())
    }
}
impl Drop for MetadataWatcher {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn scan(root: &Path, recursive: bool) -> notify::Result<BTreeMap<PathBuf, Stamp>> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    while let Some((path, depth)) = pending.pop() {
        if entries.len() >= 100_000 || depth > 64 {
            return Err(
                notify::Error::generic("metadata watch inventory exceeds its bound").add_path(path),
            );
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(notify::Error::io(error).add_path(path)),
        };
        let directory = metadata.file_type().is_dir();
        entries.insert(
            path.clone(),
            Stamp {
                modified: metadata.modified().ok(),
                len: metadata.len(),
                kind: if directory {
                    1
                } else if metadata.file_type().is_symlink() {
                    2
                } else {
                    0
                },
            },
        );
        if directory && (recursive || depth == 0) {
            for child in
                std::fs::read_dir(&path).map_err(|e| notify::Error::io(e).add_path(path.clone()))?
            {
                let child = child.map_err(notify::Error::io)?;
                if pending.len() + entries.len() >= 100_000 {
                    return Err(notify::Error::generic(
                        "metadata watch inventory exceeds its bound",
                    )
                    .add_path(path));
                }
                pending.push((child.path(), depth + 1));
            }
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn same_timestamp_append_is_reported() {
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let path = temp.path().join("active.jsonl");
        let mut writer = std::fs::File::create(&path).unwrap();
        writer.write_all(b"first\n").unwrap();
        writer.flush().unwrap();
        let modified = writer.metadata().unwrap().modified().unwrap();
        let (tx, rx) = mpsc::channel();
        let mut watcher = MetadataWatcher::new(Arc::new(move |event| {
            let _ = tx.send(event);
        }))
        .unwrap();
        watcher
            .watch(temp.path(), RecursiveMode::Recursive)
            .unwrap();
        writer.write_all(b"second\n").unwrap();
        writer.flush().unwrap();
        writer
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let event = rx
            .recv_timeout(Duration::from_secs(8))
            .expect("same-time size change must be observed")
            .unwrap();
        assert!(event.paths.contains(&path));
    }

    #[test]
    fn recursive_scan_does_not_follow_symlink_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("not-a-source"), b"private").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let entries = scan(&root, true).unwrap();
        assert!(entries.contains_key(&root.join("link")));
        assert!(!entries.contains_key(&root.join("link/not-a-source")));
    }
}
