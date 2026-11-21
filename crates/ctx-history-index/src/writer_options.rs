/// Conservative charge for one changed-session registry entry.
///
/// This covers the UUID key, fixed-size session identity facts, hash-table
/// spare capacity, and the route-local UUID undo entry while a route is active.
pub(crate) const CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES: usize = 1024;
/// Default indexing writer memory budget (512 MiB) when no explicit memory
/// limit is configured.
pub const DEFAULT_WRITER_MEMORY_BYTES: usize = 512 * 1024 * 1024;
const INDEXER_THREAD_CAP: usize = 8;
const CONSTRAINED_MEMORY_BYTES_PER_THREAD: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct WriterOptions {
    pub indexer_threads: usize,
    /// Explicit writer memory budget. Warm writers reserve up to one eighth
    /// of the indexing buffers for bounded source reconciliation; cold writers
    /// give the full buffer budget to Tantivy. This is not a process RSS cap;
    /// the changed-session registry independently uses it as a hard charged
    /// ceiling so construction-time identity authority cannot grow unbounded.
    pub memory_bytes: usize,
}

impl WriterOptions {
    /// Builds writer options for optional explicit memory and worker limits
    /// while guaranteeing the per-thread Tantivy heap minimum holds by
    /// construction.
    pub fn for_indexing_limits(memory_bytes: Option<usize>, workers: Option<usize>) -> Self {
        let parallelism = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        Self::for_indexing_limits_with_parallelism(memory_bytes, workers, parallelism)
    }

    pub fn for_indexing_limits_with_parallelism(
        memory_bytes: Option<usize>,
        workers: Option<usize>,
        available_parallelism: usize,
    ) -> Self {
        let memory_bytes = memory_bytes
            .unwrap_or(DEFAULT_WRITER_MEMORY_BYTES)
            .max(crate::INDEX_MEMORY_MIN_PER_THREAD);
        let max_threads_for_memory =
            (memory_bytes / crate::INDEX_MEMORY_MIN_PER_THREAD).clamp(1, INDEXER_THREAD_CAP);
        let indexer_threads = match workers {
            Some(workers) => workers
                .clamp(1, INDEXER_THREAD_CAP)
                .min(max_threads_for_memory),
            None => {
                let cpu_threads = available_parallelism.clamp(1, INDEXER_THREAD_CAP);
                let constrained_cap = if memory_bytes < DEFAULT_WRITER_MEMORY_BYTES {
                    (memory_bytes / CONSTRAINED_MEMORY_BYTES_PER_THREAD)
                        .clamp(1, INDEXER_THREAD_CAP)
                } else {
                    INDEXER_THREAD_CAP
                };
                cpu_threads.min(constrained_cap).min(max_threads_for_memory)
            }
        };
        Self {
            indexer_threads,
            memory_bytes,
        }
    }
}

impl Default for WriterOptions {
    fn default() -> Self {
        let parallelism = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        Self {
            indexer_threads: parallelism.clamp(1, INDEXER_THREAD_CAP),
            memory_bytes: DEFAULT_WRITER_MEMORY_BYTES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexing_limits_preserve_defaults_and_keep_per_thread_minimum() {
        let default = WriterOptions::for_indexing_limits_with_parallelism(None, None, 8);
        assert_eq!(default.memory_bytes, DEFAULT_WRITER_MEMORY_BYTES);
        assert_eq!(default.indexer_threads, 8);

        let constrained =
            WriterOptions::for_indexing_limits_with_parallelism(Some(128 * 1024 * 1024), None, 16);
        assert_eq!(constrained.memory_bytes, 128 * 1024 * 1024);
        assert_eq!(constrained.indexer_threads, 2);

        let explicit = WriterOptions::for_indexing_limits_with_parallelism(
            Some(32 * 1024 * 1024),
            Some(6),
            16,
        );
        assert_eq!(explicit.memory_bytes, 32 * 1024 * 1024);
        assert_eq!(explicit.indexer_threads, 2);
        assert!(
            explicit.memory_bytes >= crate::INDEX_MEMORY_MIN_PER_THREAD * explicit.indexer_threads
        );

        let minimum =
            WriterOptions::for_indexing_limits_with_parallelism(Some(15 * 1024 * 1024), Some(8), 8);
        assert_eq!(minimum.indexer_threads, 1);
        assert!(minimum.memory_bytes >= crate::INDEX_MEMORY_MIN_PER_THREAD);
    }
}
