use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use ctx_history_sharing::{SharingObserver, SharingWorker};

use crate::DaemonRunProfile;

/// The persistent daemon owns sharing handles until shutdown, including error
/// exits. Their drop joins the workers before the daemon releases its lock.
/// Saved destinations are reconciled on the existing daemon scheduler. Enabling
/// sharing never requires a new watcher or restarts unrelated local work.
pub(super) struct Workers {
    enabled: bool,
    next_discovery: Instant,
    handles: BTreeMap<PathBuf, SharingWorker>,
    observer: Option<SharingObserver>,
}

impl Workers {
    pub(super) fn reconcile(&mut self, data_root: &Path) {
        if !self.enabled || Instant::now() < self.next_discovery {
            return;
        }
        self.next_discovery = Instant::now() + Duration::from_secs(30);
        let Ok(entries) = std::fs::read_dir(data_root.join("sharing")) else {
            return;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            if let std::collections::btree_map::Entry::Vacant(slot) =
                self.handles.entry(entry.path())
            {
                if let Ok(Some(worker)) = SharingWorker::start_with_observer(
                    data_root.to_path_buf(),
                    entry.path(),
                    self.observer.clone(),
                ) {
                    slot.insert(worker);
                }
            }
        }
    }

    pub(super) fn wait_duration(&self, otherwise: Duration) -> Duration {
        if self.enabled {
            otherwise.min(
                self.next_discovery
                    .saturating_duration_since(Instant::now()),
            )
        } else {
            otherwise
        }
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }
    #[cfg(test)]
    fn len(&self) -> usize {
        self.handles.len()
    }
}

pub(super) fn start_workers(
    data_root: &Path,
    profile: DaemonRunProfile,
    lifecycle_ready: bool,
    observer: impl FnOnce() -> Option<SharingObserver>,
) -> Workers {
    // Sharing owns config validation, failure isolation, cancellation and the
    // independent retry timer. Local refresh and queries never wait on a tick.
    let enabled = profile == DaemonRunProfile::Persistent && lifecycle_ready;
    let mut workers = Workers {
        enabled,
        next_discovery: Instant::now(),
        handles: BTreeMap::new(),
        observer: enabled.then(observer).flatten(),
    };
    workers.reconcile(data_root);
    workers
}

#[cfg(test)]
mod tests;
