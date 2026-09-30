use super::*;

/// One collector owned by the final daemon host, shared by all destinations.
/// A second root is ignored, never retained in an unbounded process registry.
pub(crate) struct DaemonCollector(Mutex<Option<Arc<Collector>>>);

impl DaemonCollector {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(None))
    }

    pub(crate) fn sharing_observer(
        &self,
        root: &Path,
    ) -> Option<ctx_history_sharing::SharingObserver> {
        let mut slot = self.0.try_lock().ok()?;
        if slot.is_none() {
            *slot = Collector::capture(root);
        }
        slot.as_ref()
            .filter(|c| c.root == root)
            .map(Collector::sharing_observer)
    }

    /// True means this owner handled summary materialization (or is busy).
    /// False lets the uploader flush saved Sift without a sharing collector.
    pub(crate) fn flush(&self, root: &Path) -> bool {
        let Ok(slot) = self.0.try_lock() else {
            return true;
        };
        let collector = slot.as_ref().filter(|c| c.root == root).cloned();
        drop(slot);
        if let Some(collector) = collector {
            collector.flush(false);
            return collector.active.load(Ordering::Relaxed)
                || matches!(collector.binding(), Binding::Deferred | Binding::OptedOut);
        }
        false
    }

    /// Called after service return has joined every sharing worker on success
    /// and error exits. WorkerStopped is therefore inside the final window.
    pub(crate) fn finish(&self, root: &Path) {
        let Ok(mut slot) = self.0.try_lock() else {
            return;
        };
        let collector = if slot.as_ref().is_some_and(|c| c.root == root) {
            slot.take()
        } else {
            None
        };
        drop(slot);
        if let Some(collector) = collector {
            collector.flush(true);
            collector.deactivate();
        } else if let Ok(Some(owner)) = crate::identity::try_existing_installation_id(root) {
            // A daemon with no sharing observer still owns saved Sift counters
            // and its terminal outbox drain. This does not create an identity.
            if opted_out(root) {
                purge_captured_owner(root, &owner, "");
                return;
            }
            if crate::observability_composition::optional_analytics_endpoint(root).is_none() {
                return;
            }
            crate::analytics_summary::flush(root, &owner);
            let _ = crate::observability_composition::drain_analytics_outbox_for_owner(
                root,
                &owner,
                DRAIN_TIMEOUT,
            );
        }
    }
}
