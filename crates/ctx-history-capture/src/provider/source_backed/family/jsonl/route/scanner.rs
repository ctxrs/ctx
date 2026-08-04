use ctx_history_core::{CertifiedSource, CertifiedSourceAppend};

use crate::repository_attribution::RepositoryAttributor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlFamilyAppendMode {
    CertifiedSuffix,
    Replacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlFamilyProjectionMode {
    Cold,
    CertifiedAppend,
    Replacement,
}

/// Publication mode selected by an optimized JSONL leaf executor.
///
/// The family keeps this seam provider-neutral: adapters may retain a native
/// parser or staged full-file projection, while the shared driver still owns
/// inventory, scheduling, writer staging, certification, and revalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlFamilyPublication {
    Append,
    Replace,
}

/// Mutable services reused by one shared JSONL scanner worker across every
/// leaf in its stripe. Keeping these caches at worker lifetime preserves
/// bounded parallelism while amortizing provider-neutral projection work.
#[derive(Debug, Default)]
pub(crate) struct JsonlFamilyWorkerContext {
    repository_attributor: RepositoryAttributor,
}

impl JsonlFamilyWorkerContext {
    pub(super) fn begin_leaf(&mut self) {
        self.repository_attributor.begin_source();
    }

    pub(crate) fn repository_attributor(&mut self) -> &mut RepositoryAttributor {
        &mut self.repository_attributor
    }
}

#[derive(Debug, Clone)]
pub(crate) struct JsonlFamilyOptimizedLeafOutcome {
    pub(super) certificate: CertifiedSource,
    pub(super) append: Option<CertifiedSourceAppend>,
}

impl JsonlFamilyOptimizedLeafOutcome {
    pub(crate) fn replacement(certificate: CertifiedSource) -> Self {
        Self {
            certificate,
            append: None,
        }
    }

    pub(crate) fn append(append: CertifiedSourceAppend) -> Self {
        Self {
            certificate: append.current().clone(),
            append: Some(append),
        }
    }
}

#[cfg(test)]
pub(super) use activity::{
    jsonl_family_scanner_activity, jsonl_family_scanner_probe,
    record_jsonl_family_scanner_activity, with_family_scanner_workers, JsonlFamilyScannerActivity,
    JsonlFamilyScannerProbe, FAMILY_SCANNER_WORKERS_OVERRIDE,
};

#[cfg(test)]
mod activity {
    use std::{
        cell::Cell,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Barrier,
        },
    };

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub(crate) struct JsonlFamilyScannerActivity {
        pub(crate) worker_count: usize,
        pub(crate) sources_started: usize,
        pub(crate) sources_completed: usize,
        pub(crate) peak_active_scanners: usize,
    }

    thread_local! {
        pub(in super::super) static FAMILY_SCANNER_WORKERS_OVERRIDE: Cell<Option<usize>> =
            const { Cell::new(None) };
        static FAMILY_SCANNER_ACTIVITY: Cell<JsonlFamilyScannerActivity> =
            const { Cell::new(JsonlFamilyScannerActivity {
                worker_count: 0,
                sources_started: 0,
                sources_completed: 0,
                peak_active_scanners: 0,
            }) };
    }

    pub(crate) fn jsonl_family_scanner_activity() -> JsonlFamilyScannerActivity {
        FAMILY_SCANNER_ACTIVITY.get()
    }

    pub(in super::super) struct JsonlFamilyScannerProbe {
        sources_started: AtomicUsize,
        sources_completed: AtomicUsize,
        active_scanners: AtomicUsize,
        peak_active_scanners: AtomicUsize,
        rendezvous_arrivals: AtomicUsize,
        rendezvous_target: usize,
        rendezvous: Barrier,
    }

    impl JsonlFamilyScannerProbe {
        pub(in super::super) fn enter(&self) -> JsonlFamilyActiveScanner<'_> {
            self.sources_started.fetch_add(1, Ordering::SeqCst);
            let active = self
                .active_scanners
                .fetch_add(1, Ordering::SeqCst)
                .saturating_add(1);
            self.peak_active_scanners
                .fetch_max(active, Ordering::SeqCst);
            if self.rendezvous_arrivals.fetch_add(1, Ordering::SeqCst) < self.rendezvous_target {
                self.rendezvous.wait();
            }
            JsonlFamilyActiveScanner { probe: self }
        }

        fn snapshot(&self, worker_count: usize) -> JsonlFamilyScannerActivity {
            debug_assert_eq!(self.active_scanners.load(Ordering::SeqCst), 0);
            JsonlFamilyScannerActivity {
                worker_count,
                sources_started: self.sources_started.load(Ordering::SeqCst),
                sources_completed: self.sources_completed.load(Ordering::SeqCst),
                peak_active_scanners: self.peak_active_scanners.load(Ordering::SeqCst),
            }
        }
    }

    pub(in super::super) struct JsonlFamilyActiveScanner<'probe> {
        probe: &'probe JsonlFamilyScannerProbe,
    }

    impl Drop for JsonlFamilyActiveScanner<'_> {
        fn drop(&mut self) {
            self.probe.sources_completed.fetch_add(1, Ordering::SeqCst);
            self.probe.active_scanners.fetch_sub(1, Ordering::SeqCst);
        }
    }

    pub(in super::super) fn jsonl_family_scanner_probe(
        worker_count: usize,
    ) -> Option<Arc<JsonlFamilyScannerProbe>> {
        FAMILY_SCANNER_WORKERS_OVERRIDE.with(|workers| {
            workers.get().map(|_| {
                let rendezvous_target = worker_count.clamp(1, 4);
                Arc::new(JsonlFamilyScannerProbe {
                    sources_started: AtomicUsize::new(0),
                    sources_completed: AtomicUsize::new(0),
                    active_scanners: AtomicUsize::new(0),
                    peak_active_scanners: AtomicUsize::new(0),
                    rendezvous_arrivals: AtomicUsize::new(0),
                    rendezvous_target,
                    rendezvous: Barrier::new(rendezvous_target),
                })
            })
        })
    }

    pub(in super::super) fn record_jsonl_family_scanner_activity(
        worker_count: usize,
        probe: Option<&JsonlFamilyScannerProbe>,
    ) {
        FAMILY_SCANNER_ACTIVITY.set(
            probe.map_or_else(JsonlFamilyScannerActivity::default, |probe| {
                probe.snapshot(worker_count)
            }),
        );
    }

    pub(in super::super) fn with_family_scanner_workers<T>(
        workers: usize,
        run: impl FnOnce() -> T,
    ) -> T {
        struct Restore(Option<usize>);

        impl Drop for Restore {
            fn drop(&mut self) {
                FAMILY_SCANNER_WORKERS_OVERRIDE.set(self.0);
            }
        }

        let previous = FAMILY_SCANNER_WORKERS_OVERRIDE.replace(Some(workers));
        let _restore = Restore(previous);
        FAMILY_SCANNER_ACTIVITY.set(JsonlFamilyScannerActivity::default());
        run()
    }
}
