use super::*;

pub(super) const CORE_PREFETCH_ENCODED_BYTE_BUDGET: usize = 64 * 1024 * 1024;
pub(super) const CORE_PREFETCH_PAGE_ENCODED_BYTE_BUDGET: usize = 8 * 1024 * 1024;
pub(super) const CORE_PREFETCH_RECORD_PAGE_BUDGET: SnapshotPageBudget = SnapshotPageBudget::new(
    CORE_PREFETCH_PAGE_ENCODED_BYTE_BUDGET,
    MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
);

impl CoreWorkerLaunchSelection {
    pub(super) fn execution_options(self) -> CoreFeedExecutionOptions {
        CoreFeedExecutionOptions {
            prefetch_parallelism: self.budget.host_prefetch_workers,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CoreFeedExecutionOptions {
    pub(super) prefetch_parallelism: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct OrderedReconciliationOptions {
    pub(super) prefetch_parallelism: usize,
}

#[path = "ordered_prefetch/instrumentation.rs"]
mod instrumentation;
pub(super) use instrumentation::CorePrefetchInstrumentation;

#[path = "ordered_prefetch/credit_accounting.rs"]
mod credit_accounting;
pub(super) use credit_accounting::*;

#[path = "ordered_prefetch/current_page_pipeline.rs"]
mod current_page_pipeline;
pub(super) use current_page_pipeline::*;

#[path = "ordered_prefetch/reconciliation.rs"]
mod reconciliation;
pub(super) use reconciliation::reconcile_ordered_source_events;
