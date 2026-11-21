use std::thread;

use ctx_history_index::WriterOptions;

pub use ctx_history_capture_runtime::{
    source_backed_refresh_work_budget, source_backed_refresh_work_budget_with_limits,
    IndexingResourceLimits,
};

/// Binds the neutral CPU split to the index writer without moving index policy
/// into the source-backed runtime crate.
pub fn source_backed_refresh_writer_options() -> WriterOptions {
    source_backed_refresh_writer_options_with_limits(IndexingResourceLimits::default())
}

/// Binds the neutral CPU split and optional explicit indexing memory/worker
/// limits to the index writer while guaranteeing per-thread heap minimums.
pub fn source_backed_refresh_writer_options_with_limits(
    limits: IndexingResourceLimits,
) -> WriterOptions {
    source_backed_refresh_writer_options_for_parallelism_with_limits(
        thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
        limits,
    )
}

#[cfg(test)]
fn source_backed_refresh_writer_options_for_parallelism(
    available_parallelism: usize,
) -> WriterOptions {
    source_backed_refresh_writer_options_for_parallelism_with_limits(
        available_parallelism,
        IndexingResourceLimits::default(),
    )
}

fn source_backed_refresh_writer_options_for_parallelism_with_limits(
    available_parallelism: usize,
    limits: IndexingResourceLimits,
) -> WriterOptions {
    WriterOptions {
        indexer_threads: limits.indexer_threads_for_parallelism(available_parallelism),
        memory_bytes: limits.effective_memory_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_options_preserve_the_runtime_cpu_split() {
        for available in 1..=64 {
            assert_eq!(
                source_backed_refresh_writer_options_for_parallelism(available).indexer_threads,
                ctx_history_capture_runtime::source_backed_refresh_indexer_threads_for_parallelism(
                    available
                )
            );
        }
    }

    #[test]
    fn writer_options_honor_explicit_indexing_limits() {
        let constrained = source_backed_refresh_writer_options_for_parallelism_with_limits(
            16,
            IndexingResourceLimits::from_memory_mb_and_workers(Some(128), None),
        );
        assert_eq!(constrained.memory_bytes, 128 * 1024 * 1024);
        assert_eq!(constrained.indexer_threads, 2);

        let bounded = source_backed_refresh_writer_options_for_parallelism_with_limits(
            16,
            IndexingResourceLimits::from_memory_mb_and_workers(Some(32), Some(4)),
        );
        assert_eq!(bounded.memory_bytes, 32 * 1024 * 1024);
        assert_eq!(bounded.indexer_threads, 2);
    }
}
