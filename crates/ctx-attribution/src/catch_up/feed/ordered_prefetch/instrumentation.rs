use super::*;

#[derive(Default)]
pub(in super::super) struct CorePrefetchInstrumentation {
    configured_parallelism: AtomicUsize,
    workers_launched: AtomicUsize,
    active_workers: AtomicUsize,
    maximum_active_workers: AtomicUsize,
    planned_pages: AtomicUsize,
    materialized_pages: AtomicUsize,
    decoded_records: AtomicUsize,
    decoded_record_bytes: AtomicUsize,
    cancelled_waits_or_sends: AtomicUsize,
}

impl CorePrefetchInstrumentation {
    pub(in super::super) fn configured(&self, parallelism: usize, workers_launched: usize) {
        self.configured_parallelism
            .store(parallelism, AtomicOrdering::Relaxed);
        self.workers_launched
            .store(workers_launched, AtomicOrdering::Relaxed);
    }

    pub(in super::super) fn worker_started(&self) {
        let active = self.active_workers.fetch_add(1, AtomicOrdering::Relaxed) + 1;
        self.maximum_active_workers
            .fetch_max(active, AtomicOrdering::Relaxed);
    }

    pub(in super::super) fn worker_finished(&self) {
        self.active_workers.fetch_sub(1, AtomicOrdering::Relaxed);
    }

    pub(in super::super) fn page_planned(&self) {
        self.planned_pages.fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub(in super::super) fn page_materialized(&self) {
        self.materialized_pages
            .fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub(in super::super) fn records_decoded(&self, records: usize, record_bytes: usize) {
        self.decoded_records
            .fetch_add(records, AtomicOrdering::Relaxed);
        self.decoded_record_bytes
            .fetch_add(record_bytes, AtomicOrdering::Relaxed);
    }

    pub(in super::super) fn cancelled(&self) {
        self.cancelled_waits_or_sends
            .fetch_add(1, AtomicOrdering::Relaxed);
    }
}
