use super::*;
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;

pub(super) struct PendingSource {
    pub(super) index_fields: IndexSourceFields,
    pub(super) staged: StagedPendingSource,
}

impl std::ops::Deref for PendingSource {
    type Target = StagedPendingSource;

    fn deref(&self) -> &Self::Target {
        &self.staged
    }
}

impl std::ops::DerefMut for PendingSource {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.staged
    }
}

const WRITER_HANDOFF_RETRY_WINDOW: Duration = Duration::from_millis(500);
const WRITER_HANDOFF_RETRY_INTERVAL: Duration = Duration::from_millis(5);
const GENERATION_INTEGRITY_RECEIPT_VERSION: u32 = 1;
const GENERATION_INTEGRITY_RECEIPT_SUFFIX: &str = ".integrity.json";
const ACTIVE_GENERATION_REBUILD_MARKER_FILE: &str = "active-generation-rebuild-required.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationIntegrityReceipt {
    version: u32,
    generation_id: String,
    directory: String,
    files: Vec<GenerationFileFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationFileFingerprint {
    name: String,
    length: u64,
    modified_unix_seconds: u64,
    modified_subsec_nanos: u32,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActiveGenerationRebuildMarker {
    pub(super) version: u32,
    pub(super) generation_id: String,
    pub(super) directory: String,
}

pub(super) fn write_generation_integrity_receipt(
    root: &Path,
    generation_id: &str,
    generation_path: &Path,
) -> Result<()> {
    let directory_name = generation_directory_name(generation_path)?;
    let receipt = GenerationIntegrityReceipt {
        version: GENERATION_INTEGRITY_RECEIPT_VERSION,
        generation_id: generation_id.to_owned(),
        directory: directory_name.to_owned(),
        files: generation_file_fingerprints(generation_path)?,
    };
    let bytes = serde_json::to_vec(&receipt)?;
    let directory = DurableMmapDirectory::open(root.join(MANIFEST_DIRECTORY))
        .map_err(tantivy::TantivyError::from)?;
    directory.atomic_write(
        Path::new(&generation_integrity_receipt_name(directory_name)),
        &bytes,
    )?;
    Ok(())
}

pub(super) fn validate_generation_integrity_receipt(
    root: &Path,
    generation_id: &str,
    generation_path: &Path,
) -> Result<()> {
    let directory_name = generation_directory_name(generation_path)?;
    let path = root
        .join(MANIFEST_DIRECTORY)
        .join(generation_integrity_receipt_name(directory_name));
    let bytes =
        fs::read(&path).map_err(|error| IndexError::GenerationPhysicalIntegrityMismatch {
            generation_id: generation_id.to_owned(),
            detail: error.to_string(),
        })?;
    let receipt: GenerationIntegrityReceipt = serde_json::from_slice(&bytes).map_err(|error| {
        IndexError::GenerationPhysicalIntegrityMismatch {
            generation_id: generation_id.to_owned(),
            detail: error.to_string(),
        }
    })?;
    let canonical = serde_json::to_vec(&receipt)?;
    if canonical != bytes
        || receipt.version != GENERATION_INTEGRITY_RECEIPT_VERSION
        || receipt.generation_id != generation_id
        || receipt.directory != directory_name
    {
        return Err(IndexError::GenerationPhysicalIntegrityMismatch {
            generation_id: generation_id.to_owned(),
            detail: "receipt is non-canonical or names a different generation".to_owned(),
        });
    }
    let current = generation_file_fingerprints(generation_path)?;
    if receipt.files != current {
        return Err(IndexError::GenerationPhysicalIntegrityMismatch {
            generation_id: generation_id.to_owned(),
            detail: "immutable index file metadata changed after publication".to_owned(),
        });
    }
    Ok(())
}

pub(super) fn reclaim_generation_integrity_receipts(
    root: &Path,
    retained_generation_directories: &[String],
) -> Result<()> {
    let directory = root.join(MANIFEST_DIRECTORY);
    let retained = retained_generation_directories
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut removed = false;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(generation_directory) = name.strip_suffix(GENERATION_INTEGRITY_RECEIPT_SUFFIX)
        else {
            continue;
        };
        if generation_directory.starts_with("generation-")
            && !retained.contains(generation_directory)
        {
            fs::remove_file(entry.path())?;
            removed = true;
        }
    }
    if removed {
        sync_directory(&directory)?;
    }
    Ok(())
}

pub(super) fn mark_active_generation_for_rebuild(
    root: &Path,
    active: &GenerationSlot,
) -> Result<()> {
    let marker = ActiveGenerationRebuildMarker {
        version: 1,
        generation_id: active.generation_id().to_owned(),
        directory: active.directory().to_owned(),
    };
    let bytes = serde_json::to_vec(&marker)?;
    let directory = DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
    directory.atomic_write(Path::new(ACTIVE_GENERATION_REBUILD_MARKER_FILE), &bytes)?;
    Ok(())
}

pub(super) fn load_active_generation_rebuild_marker(
    root: &Path,
) -> Result<Option<ActiveGenerationRebuildMarker>> {
    let bytes = match fs::read(root.join(ACTIVE_GENERATION_REBUILD_MARKER_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let marker: ActiveGenerationRebuildMarker = serde_json::from_slice(&bytes)
        .map_err(|_| IndexError::InvalidActiveGenerationRebuildMarker)?;
    if serde_json::to_vec(&marker)? != bytes
        || marker.version != 1
        || !is_generation_id(&marker.generation_id)
        || GenerationSlot::new(marker.generation_id.clone(), marker.directory.clone()).is_err()
    {
        return Err(IndexError::InvalidActiveGenerationRebuildMarker);
    }
    Ok(Some(marker))
}

pub(super) fn clear_active_generation_rebuild_marker(root: &Path) -> Result<()> {
    match fs::remove_file(root.join(ACTIVE_GENERATION_REBUILD_MARKER_FILE)) {
        Ok(()) => {
            sync_directory(root)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn generation_integrity_receipt_name(generation_directory: &str) -> String {
    format!("{generation_directory}{GENERATION_INTEGRITY_RECEIPT_SUFFIX}")
}

fn generation_directory_name(generation_path: &Path) -> Result<&str> {
    generation_path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or(IndexError::WriterInvariant(
            "generation path has no UTF-8 directory name",
        ))
}

fn generation_file_fingerprints(generation_path: &Path) -> Result<Vec<GenerationFileFingerprint>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(generation_path)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || skip_generation_integrity_file(&entry.file_name()) {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| IndexError::WriterInvariant("generation file name is not UTF-8"))?;
        let metadata = entry.metadata()?;
        let modified = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| {
                IndexError::WriterInvariant("generation file modification time predates Unix epoch")
            })?;
        files.push(GenerationFileFingerprint {
            name,
            length: metadata.len(),
            modified_unix_seconds: modified.as_secs(),
            modified_subsec_nanos: modified.subsec_nanos(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        });
    }
    files.sort();
    Ok(files)
}

fn skip_generation_integrity_file(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return true;
    };
    name.ends_with(".lock") || name.starts_with(".ctx-tantivy-atomic-")
}

pub(super) fn acquire_generation_writer_lock_with_retry(
    directory: &impl Directory,
    lock: &Lock,
) -> Result<DirectoryLock> {
    acquire_lock_with_retry(
        directory,
        lock,
        "failed to acquire the generation writer lock",
    )
}

fn acquire_lock_with_retry(
    directory: &impl Directory,
    lock: &Lock,
    context: &'static str,
) -> Result<DirectoryLock> {
    let deadline = Instant::now() + WRITER_HANDOFF_RETRY_WINDOW;
    loop {
        match directory.acquire_lock(lock) {
            Ok(lock) => return Ok(lock),
            Err(error @ LockError::LockBusy) if Instant::now() >= deadline => {
                return Err(tantivy::TantivyError::LockFailure(
                    error,
                    Some(
                        "Failed to acquire index lock. If you are using a regular directory, this \
                         means there is already an `IndexWriter` working on this `Directory`, in \
                         this process or in a different process."
                            .to_owned(),
                    ),
                )
                .into());
            }
            Err(LockError::LockBusy) => {
                std::thread::sleep(WRITER_HANDOFF_RETRY_INTERVAL);
            }
            Err(error) => {
                return Err(
                    tantivy::TantivyError::LockFailure(error, Some(context.to_owned())).into(),
                );
            }
        }
    }
}

pub(super) fn construct_index_writer_with_retry(
    index: &Index,
    options: &WriterOptions,
) -> Result<IndexWriter<IndexDocument>> {
    let deadline = Instant::now() + WRITER_HANDOFF_RETRY_WINDOW;
    loop {
        match index
            .writer_with_num_threads::<IndexDocument>(options.indexer_threads, options.memory_bytes)
        {
            Ok(writer) => return Ok(writer),
            Err(error @ tantivy::TantivyError::LockFailure(LockError::LockBusy, _))
                if Instant::now() >= deadline =>
            {
                return Err(error.into());
            }
            Err(tantivy::TantivyError::LockFailure(LockError::LockBusy, _)) => {
                std::thread::sleep(WRITER_HANDOFF_RETRY_INTERVAL);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Exact replay plus current inventories; the prior manifest is comparison state only.
pub(super) struct ExactReplayInventoryWitness<'a> {
    pub(super) base: &'a GenerationManifest,
}
/// Exact point lookup over the immutable generation captured when a writer opened.
///
/// Provider append adapters use this to resolve a small suffix against existing
/// deterministic identities without enumerating or decoding the validated prefix.
#[derive(Clone)]
pub struct BaseEventIdentityLookup {
    pub(super) searcher: Option<Searcher>,
    pub(super) event_id_field: Field,
}

impl BaseEventIdentityLookup {
    /// Returns whether the immutable base generation contains `event_id`.
    pub fn contains(&self, event_id: Uuid) -> Result<bool> {
        let Some(searcher) = self.searcher.as_ref() else {
            return Ok(false);
        };
        let query = TermQuery::new(
            Term::from_field_text(self.event_id_field, &event_id.to_string()),
            IndexRecordOption::Basic,
        );
        match searcher.search(&query, &Count)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(IndexError::DuplicateEventIdentity(event_id.to_string())),
        }
    }
}
