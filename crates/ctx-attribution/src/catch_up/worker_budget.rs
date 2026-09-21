use std::thread;

pub(super) const MAX_CORE_PREFETCH_WORKERS: usize = 8;
pub(super) const MAX_HELPER_PREPARATION_WORKERS: usize = 16;
const CORE_LAUNCH_PRODUCT_PARTS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CoreLaunchProductBudget {
    pub(super) helper_preparation_workers: usize,
    pub(super) host_prefetch_workers: usize,
    pub(super) control_writer_headroom: usize,
}

pub(super) fn core_launch_product_budget(worker_count: usize) -> CoreLaunchProductBudget {
    let worker_count = worker_count.max(1);
    if worker_count <= 2 {
        return CoreLaunchProductBudget {
            helper_preparation_workers: 1,
            host_prefetch_workers: 1,
            control_writer_headroom: 0,
        };
    }
    let quarter = (worker_count / CORE_LAUNCH_PRODUCT_PARTS).max(1);
    let host_prefetch_workers = quarter.min(MAX_CORE_PREFETCH_WORKERS);
    let control_writer_headroom = quarter.min(worker_count.saturating_sub(1));
    let helper_preparation_workers = worker_count
        .saturating_sub(host_prefetch_workers)
        .saturating_sub(control_writer_headroom)
        .clamp(1, MAX_HELPER_PREPARATION_WORKERS);
    CoreLaunchProductBudget {
        helper_preparation_workers,
        host_prefetch_workers,
        control_writer_headroom,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CoreWorkerLaunchSelection {
    pub(super) budget: CoreLaunchProductBudget,
}

impl CoreWorkerLaunchSelection {
    pub fn from_runtime() -> Self {
        let available = thread::available_parallelism().map_or(1, usize::from);
        Self::from_requested(available, None)
    }

    fn from_requested(available: usize, requested: Option<usize>) -> Self {
        let worker_count = requested.unwrap_or_else(|| available.max(1));
        let budget = core_launch_product_budget(worker_count);
        debug_assert!(budget.helper_preparation_workers > 0);
        debug_assert!(budget.host_prefetch_workers > 0);
        debug_assert!(
            budget.helper_preparation_workers + budget.control_writer_headroom
                <= worker_count.max(1)
        );
        debug_assert!(
            budget.host_prefetch_workers + budget.control_writer_headroom <= worker_count.max(1)
        );
        Self { budget }
    }
}
