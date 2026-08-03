use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub(super) struct PendingSource {
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
const ACTIVE_GENERATION_REBUILD_MARKER_FILE: &str = "active-generation-rebuild-required.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActiveGenerationRebuildMarker {
    pub(super) version: u32,
    pub(super) generation_id: String,
    pub(super) directory: String,
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
        || !GenerationSlot::names_are_valid(&marker.generation_id, &marker.directory)
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
