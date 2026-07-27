use std::{
    collections::hash_map::DefaultHasher,
    ffi::OsStr,
    fs::{self, FileType, ReadDir},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};

use crate::commands::import::SourceStats;

use super::observation::SourceChangeFingerprint;

// Directory traversal remains constant-memory and descriptor-bounded. Pace
// large filesystem walks independently from the smaller SQLite write page so
// the second, race-detection pass does not sleep once per Store transaction.
const SOURCE_IMPORT_PACE_OPERATIONS: usize = 512;
const SOURCE_IMPORT_PACE_INTERVAL: Duration = Duration::from_millis(5);
// The selected source root is depth zero. A walk may therefore retain the root
// plus this many nested `ReadDir` frames, but it never opens another one.
const SOURCE_IMPORT_MAX_DIRECTORY_DEPTH: usize = 64;

pub(super) fn bounded_source_root_stats(path: &Path) -> Result<SourceStats> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("stat import source {}", path.display()))?;
    if !metadata.file_type().is_dir() {
        return Err(anyhow!(
            "bounded source-root inventory requires a directory: {}",
            path.display()
        ));
    }
    let mut stats = SourceStats::default();
    let mut fingerprint = SourceChangeFingerprint::default();
    for entry_path in SourceImportDirectoryWalk::new(path)? {
        let entry_path = entry_path?;
        let metadata = fs::metadata(&entry_path)
            .with_context(|| format!("stat import source file {}", entry_path.display()))?;
        stats.files += 1;
        stats.bytes = stats.bytes.saturating_add(metadata.len());
        if !entry_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-shm"))
        {
            let observation = ctx_history_capture::observe_ordinary_file(&entry_path)
                .with_context(|| format!("observe import source file {}", entry_path.display()))?;
            fingerprint.observe(
                entry_path.strip_prefix(path).unwrap_or(&entry_path),
                &observation,
            );
        }
    }
    stats.change_token = Some(fingerprint.finish());
    Ok(stats)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DirectoryEntriesFingerprint {
    count: u64,
    xor: u64,
    sum: u64,
}

impl DirectoryEntriesFingerprint {
    fn observe(&mut self, name: &OsStr, file_type: FileType) {
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        file_type.is_dir().hash(&mut hasher);
        file_type.is_file().hash(&mut hasher);
        file_type.is_symlink().hash(&mut hasher);
        let hash = hasher.finish();
        self.count = self.count.saturating_add(1);
        self.xor ^= hash;
        self.sum = self.sum.wrapping_add(hash);
    }
}

struct SourceImportDirectoryFrame {
    path: PathBuf,
    entries: ReadDir,
    metadata_len: u64,
    metadata_modified: SystemTime,
    fingerprint: DirectoryEntriesFingerprint,
}

impl SourceImportDirectoryFrame {
    fn open(path: PathBuf) -> Result<Self> {
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("stat import source directory {}", path.display()))?;
        if !metadata.file_type().is_dir() {
            return Err(anyhow!(
                "import source directory changed during inventory: {}",
                path.display()
            ));
        }
        let entries = fs::read_dir(&path)
            .with_context(|| format!("read import source directory {}", path.display()))?;
        Ok(Self {
            path,
            entries,
            metadata_len: metadata.len(),
            metadata_modified: metadata.modified().unwrap_or(UNIX_EPOCH),
            fingerprint: DirectoryEntriesFingerprint::default(),
        })
    }

    fn revalidate(self, pacer: &mut InventoryPacer) -> Result<()> {
        let Self {
            path,
            entries,
            metadata_len,
            metadata_modified,
            fingerprint,
        } = self;
        // Close the exhausted handle before reopening the directory to verify
        // its entries, keeping this pass inside the same descriptor bound.
        drop(entries);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("revalidate import source directory {}", path.display()))?;
        if !metadata.file_type().is_dir()
            || metadata.len() != metadata_len
            || metadata.modified().unwrap_or(UNIX_EPOCH) != metadata_modified
            || directory_entries_fingerprint(&path, pacer)? != fingerprint
        {
            return Err(anyhow!(
                "import source directory changed during inventory: {}",
                path.display()
            ));
        }
        Ok(())
    }
}

pub(super) struct SourceImportDirectoryWalk {
    frames: Vec<SourceImportDirectoryFrame>,
    failed: bool,
    pacer: InventoryPacer,
}

impl SourceImportDirectoryWalk {
    pub(super) fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            frames: vec![SourceImportDirectoryFrame::open(root.to_path_buf())?],
            failed: false,
            pacer: InventoryPacer::new(),
        })
    }

    fn next_path(&mut self) -> Result<Option<PathBuf>> {
        loop {
            let Some(frame) = self.frames.last_mut() else {
                return Ok(None);
            };
            let Some(entry) = frame.entries.next() else {
                let Some(frame) = self.frames.pop() else {
                    return Err(anyhow!(
                        "import source directory walk lost its active frame"
                    ));
                };
                frame.revalidate(&mut self.pacer)?;
                continue;
            };
            self.pacer.observe();
            let entry = entry.with_context(|| {
                format!("read import source entry under {}", frame.path.display())
            })?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("stat import source entry {}", path.display()))?;
            frame.fingerprint.observe(&entry.file_name(), file_type);
            if file_type.is_dir() {
                let directory_depth = self.frames.len();
                if directory_depth > SOURCE_IMPORT_MAX_DIRECTORY_DEPTH {
                    return Err(anyhow!(
                        "import source directory depth exceeds the {}-level limit: {}",
                        SOURCE_IMPORT_MAX_DIRECTORY_DEPTH,
                        path.display()
                    ));
                }
                self.frames.push(SourceImportDirectoryFrame::open(path)?);
            } else if file_type.is_file() {
                return Ok(Some(path));
            }
        }
    }
}

impl Iterator for SourceImportDirectoryWalk {
    type Item = Result<PathBuf>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.next_path() {
            Ok(Some(path)) => Some(Ok(path)),
            Ok(None) => None,
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

fn directory_entries_fingerprint(
    path: &Path,
    pacer: &mut InventoryPacer,
) -> Result<DirectoryEntriesFingerprint> {
    let mut fingerprint = DirectoryEntriesFingerprint::default();
    for entry in fs::read_dir(path)
        .with_context(|| format!("revalidate import source directory {}", path.display()))?
    {
        pacer.observe();
        let entry = entry
            .with_context(|| format!("revalidate import source entry under {}", path.display()))?;
        let entry_path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("revalidate import source entry {}", entry_path.display()))?;
        fingerprint.observe(&entry.file_name(), file_type);
    }
    Ok(fingerprint)
}

struct InventoryPacer {
    operations: usize,
    last_page_at: Instant,
}

impl InventoryPacer {
    fn new() -> Self {
        Self {
            operations: 0,
            last_page_at: Instant::now(),
        }
    }

    fn observe(&mut self) {
        self.operations = self.operations.saturating_add(1);
        if self.operations < SOURCE_IMPORT_PACE_OPERATIONS {
            return;
        }
        let elapsed = self.last_page_at.elapsed();
        if elapsed < SOURCE_IMPORT_PACE_INTERVAL {
            thread::sleep(SOURCE_IMPORT_PACE_INTERVAL - elapsed);
        }
        self.operations = 0;
        self.last_page_at = Instant::now();
    }
}

pub(super) fn pace_inventory_page() {
    thread::sleep(SOURCE_IMPORT_PACE_INTERVAL);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_nested_tree(root: &Path, directory_depth: usize) -> PathBuf {
        let mut directory = root.to_path_buf();
        for _ in 0..directory_depth {
            directory = directory.join("d");
            fs::create_dir(&directory).unwrap();
        }
        directory
    }

    #[test]
    fn directory_walk_accepts_the_depth_limit_with_bounded_open_frames() {
        let temp = tempfile::tempdir().unwrap();
        let deepest = create_nested_tree(temp.path(), SOURCE_IMPORT_MAX_DIRECTORY_DEPTH);
        let leaf = deepest.join("leaf.jsonl");
        fs::write(&leaf, b"{}\n").unwrap();

        let mut walk = SourceImportDirectoryWalk::new(temp.path()).unwrap();
        let mut discovered = Vec::new();
        let mut peak_open_frames = walk.frames.len();
        while let Some(path) = walk.next_path().unwrap() {
            discovered.push(path);
            peak_open_frames = peak_open_frames.max(walk.frames.len());
            assert!(walk.frames.len() <= SOURCE_IMPORT_MAX_DIRECTORY_DEPTH + 1);
        }

        assert_eq!(discovered, vec![leaf]);
        assert_eq!(peak_open_frames, SOURCE_IMPORT_MAX_DIRECTORY_DEPTH + 1);
    }

    #[test]
    fn directory_walk_rejects_over_limit_depth_deterministically_before_opening_it() {
        let temp = tempfile::tempdir().unwrap();
        let deepest = create_nested_tree(temp.path(), SOURCE_IMPORT_MAX_DIRECTORY_DEPTH + 1);
        fs::write(deepest.join("leaf.jsonl"), b"{}\n").unwrap();

        let run = || {
            let mut walk = SourceImportDirectoryWalk::new(temp.path()).unwrap();
            let error = loop {
                match walk.next_path() {
                    Ok(Some(_)) => {}
                    Ok(None) => panic!("over-limit tree unexpectedly completed"),
                    Err(error) => break error.to_string(),
                }
            };
            (error, walk.frames.len())
        };
        let first = run();
        let second = run();

        assert_eq!(first, second);
        assert!(first.0.contains(&format!(
            "directory depth exceeds the {}-level limit",
            SOURCE_IMPORT_MAX_DIRECTORY_DEPTH
        )));
        assert!(first.0.contains(&deepest.display().to_string()));
        assert_eq!(first.1, SOURCE_IMPORT_MAX_DIRECTORY_DEPTH + 1);
    }
}
