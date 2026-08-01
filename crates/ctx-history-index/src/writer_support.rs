use super::*;

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
