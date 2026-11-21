/// Conservative charge for one changed-session registry entry.
///
/// This covers the UUID key, fixed-size session identity facts, hash-table
/// spare capacity, and the route-local UUID undo entry while a route is active.
pub(crate) const CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES: usize = 1024;
/// Minimum decoupled logical ceiling for the spill-backed changed-session
/// registry (4 GiB nominal charge = 4,194,304 distinct sessions on 64-bit
/// hosts; 1 GiB = 1,048,576 on 32-bit hosts) so small `memory_bytes` settings
/// spill to disk instead of aborting large cold or upgrade rebuilds.
pub(crate) const CHANGED_SESSION_REGISTRY_MIN_CEILING_BYTES: usize = if usize::BITS >= 64 {
    4 * 1024 * 1024 * 1024
} else {
    1024 * 1024 * 1024
};
/// Default indexing writer memory budget (512 MiB) when no explicit memory
/// limit is configured.
pub const DEFAULT_WRITER_MEMORY_BYTES: usize = 512 * 1024 * 1024;
const INDEXER_THREAD_CAP: usize = 8;
const MAX_PARALLEL_LEAF_WORKERS: usize = 16;
const CONSTRAINED_MEMORY_BYTES_PER_THREAD: usize = 64 * 1024 * 1024;
const MAX_MERGE_THREADS: usize = 4;
const MERGE_MEMORY_BYTES_PER_THREAD: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct WriterOptions {
    pub indexer_threads: usize,
    /// Explicit writer memory budget. Warm writers reserve up to one eighth
    /// of the indexing buffers for bounded source reconciliation; cold writers
    /// give the full buffer budget to Tantivy. This is not a process RSS cap;
    /// the changed-session registry sizes its in-memory hot map from this
    /// budget and spills overflow entries to a private disk table.
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
            Some(workers) => {
                let effective_total = workers
                    .clamp(1, MAX_PARALLEL_LEAF_WORKERS)
                    .min(available_parallelism.max(1));
                (effective_total / 2)
                    .clamp(1, INDEXER_THREAD_CAP)
                    .min(max_threads_for_memory)
            }
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

    /// Bounds Tantivy's background merge thread pool proportionally to the
    /// configured worker and memory limits instead of inheriting Tantivy's
    /// fixed 4-thread default.
    pub fn merge_threads(&self) -> usize {
        let memory_cap =
            (self.memory_bytes / MERGE_MEMORY_BYTES_PER_THREAD).clamp(1, MAX_MERGE_THREADS);
        self.indexer_threads
            .clamp(1, MAX_MERGE_THREADS)
            .min(memory_cap)
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
        assert_eq!(default.merge_threads(), 4);

        let constrained =
            WriterOptions::for_indexing_limits_with_parallelism(Some(128 * 1024 * 1024), None, 16);
        assert_eq!(constrained.memory_bytes, 128 * 1024 * 1024);
        assert_eq!(constrained.indexer_threads, 2);
        assert_eq!(constrained.merge_threads(), 1);

        let explicit = WriterOptions::for_indexing_limits_with_parallelism(
            Some(32 * 1024 * 1024),
            Some(6),
            16,
        );
        assert_eq!(explicit.memory_bytes, 32 * 1024 * 1024);
        assert_eq!(explicit.indexer_threads, 2);
        assert_eq!(explicit.merge_threads(), 1);
        assert!(
            explicit.memory_bytes >= crate::INDEX_MEMORY_MIN_PER_THREAD * explicit.indexer_threads
        );

        let minimum =
            WriterOptions::for_indexing_limits_with_parallelism(Some(15 * 1024 * 1024), Some(8), 8);
        assert_eq!(minimum.indexer_threads, 1);
        assert_eq!(minimum.merge_threads(), 1);
        assert!(minimum.memory_bytes >= crate::INDEX_MEMORY_MIN_PER_THREAD);

        let split_two = WriterOptions::for_indexing_limits_with_parallelism(None, Some(2), 16);
        assert_eq!(split_two.indexer_threads, 1);
        assert_eq!(split_two.merge_threads(), 1);

        let cpu_clamped = WriterOptions::for_indexing_limits_with_parallelism(None, Some(16), 4);
        assert_eq!(cpu_clamped.indexer_threads, 2);
        assert_eq!(cpu_clamped.merge_threads(), 2);

        let medium_mem =
            WriterOptions::for_indexing_limits_with_parallelism(Some(256 * 1024 * 1024), None, 16);
        assert_eq!(medium_mem.indexer_threads, 4);
        assert_eq!(medium_mem.merge_threads(), 2);
    }
}
