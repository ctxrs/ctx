use std::{path::Path, sync::Arc};

use super::source_route_ledger_now_ms;
use crate::{
    daemon_wakeup::{
        write_degraded_wakeup_receipt, DaemonFileWatcher, DaemonWakeup, DaemonWatchCatalog,
        SourceWatchBatch,
    },
    source_backed_refresh_coordinator::{
        pin_published_generation, source_backed_watch_catalog, CoreRefreshEngine,
    },
};
use anyhow::Result;
use ctx_history_capture::SourceBackedWatchCatalog;
use ctx_history_refresh::EventWatermark;

#[derive(Clone, Copy, Debug)]
pub(super) enum WatchCatalogReconcileTrigger {
    Startup,
    RuntimeActivation,
    SafetyTimeout,
    CatalogControl(EventWatermark),
    WatcherRecovery,
    Filesystem,
}

impl WatchCatalogReconcileTrigger {
    fn requests_catalog_refresh(self) -> bool {
        matches!(self, Self::Startup | Self::CatalogControl(_))
    }

    fn attempts_pending_catalog(self) -> bool {
        matches!(
            self,
            Self::Startup | Self::SafetyTimeout | Self::CatalogControl(_)
        )
    }

    fn ensures_watcher(self) -> bool {
        matches!(
            self,
            Self::Startup | Self::SafetyTimeout | Self::CatalogControl(_)
        )
    }

    fn reconciles_roots(self) -> bool {
        matches!(
            self,
            Self::SafetyTimeout
                | Self::CatalogControl(_)
                | Self::WatcherRecovery
                | Self::Filesystem
        )
    }

    fn watermark(self) -> Option<EventWatermark> {
        match self {
            Self::CatalogControl(watermark) => Some(watermark),
            _ => None,
        }
    }
}

/// Owns the daemon's one watch-catalog snapshot and its native projection.
///
/// Catalog publication and coordinator route-authority initialization share
/// one reconciliation path so they cannot drift during startup or recovery.
pub(super) struct DaemonWatchRuntime {
    wakeup: Arc<DaemonWakeup>,
    pub(super) catalog: DaemonWatchCatalog,
    pub(super) file_watcher: Option<DaemonFileWatcher>,
    catalog_refresh_pending: bool,
    provider_root_refresh_pending: bool,
    config: &'static dyn crate::DaemonConfigPort,
}

impl DaemonWatchRuntime {
    pub(super) fn new(
        wakeup: Arc<DaemonWakeup>,
        config: &'static dyn crate::DaemonConfigPort,
    ) -> Self {
        Self {
            wakeup,
            catalog: DaemonWatchCatalog::default(),
            file_watcher: None,
            catalog_refresh_pending: false,
            provider_root_refresh_pending: false,
            config,
        }
    }

    #[cfg(test)]
    pub(super) fn provider_root_refresh_pending_for_test(&self) -> bool {
        self.provider_root_refresh_pending
    }

    fn schedule_pending_missing_routes(&self, data_root: &Path, refresh: &CoreRefreshEngine) {
        let watermark = self
            .file_watcher
            .as_ref()
            .map(DaemonFileWatcher::startup_watermark)
            .unwrap_or_else(|| EventWatermark::new(0, 0));
        let now_ms = source_route_ledger_now_ms();
        let result = refresh.schedule_pending_missing_route_rechecks(data_root, watermark, now_ms);
        if let Err(error) = result {
            let _ = write_degraded_wakeup_receipt(data_root, &error);
        }
    }

    pub(super) fn reconcile_catalog_and_route_authority(
        &mut self,
        data_root: &Path,
        source_refresh: Option<&CoreRefreshEngine>,
        trigger: WatchCatalogReconcileTrigger,
        force_rearm: bool,
    ) {
        let config = self.config;
        self.reconcile_catalog_and_route_authority_with(
            data_root,
            source_refresh,
            trigger,
            force_rearm,
            |data_root| source_backed_watch_catalog(data_root, config),
            DaemonFileWatcher::start,
        );
    }

    pub(super) fn reconcile_catalog_and_route_authority_with<C, W>(
        &mut self,
        data_root: &Path,
        source_refresh: Option<&CoreRefreshEngine>,
        trigger: WatchCatalogReconcileTrigger,
        force_rearm: bool,
        mut construct_catalog: C,
        mut start_watcher: W,
    ) -> usize
    where
        C: FnMut(&Path) -> Result<SourceBackedWatchCatalog>,
        W: FnMut(&Path, Arc<DaemonWakeup>, DaemonWatchCatalog) -> Result<DaemonFileWatcher>,
    {
        if trigger.requests_catalog_refresh() {
            self.catalog_refresh_pending = true;
        }
        if self.file_watcher.is_none() && trigger.ensures_watcher() {
            // A missing watcher could also have missed a control/catalog
            // change. Attempt fresh catalog construction before recreating
            // the native projection, and retain this bit if either fails.
            self.catalog_refresh_pending = true;
        }

        let mut catalog_published = false;
        if self.catalog_refresh_pending && trigger.attempts_pending_catalog() {
            match construct_catalog(data_root) {
                Ok(catalog) => {
                    let previous_digest = self.catalog.snapshot().and_then(|catalog| {
                        catalog.provider_root_config_digest().map(str::to_owned)
                    });
                    let current_digest = catalog.provider_root_config_digest().map(str::to_owned);
                    let provider_root_config_changed =
                        match (previous_digest.as_deref(), current_digest.as_deref()) {
                            (Some(previous), Some(current)) => previous != current,
                            (None, Some(current)) => match pin_published_generation(data_root) {
                                Ok(Some(published)) => {
                                    published
                                        .verified_index()
                                        .manifest()
                                        .provider_root_config_digest()
                                        != current
                                }
                                Ok(None) => false,
                                Err(error) => {
                                    let _ = write_degraded_wakeup_receipt(data_root, &error);
                                    true
                                }
                            },
                            _ => false,
                        };
                    if provider_root_config_changed {
                        // Root aliases and source_groups are generation metadata, not
                        // live watch-catalog state. Exact route maintenance
                        // deliberately preserves the pinned generation's
                        // aliases, so a config topology change must cross one
                        // full-refresh publication boundary.
                        self.provider_root_refresh_pending = true;
                    }
                    self.catalog.publish(catalog);
                    self.catalog_refresh_pending = false;
                    catalog_published = true;
                }
                Err(error) => {
                    let _ = write_degraded_wakeup_receipt(data_root, &error);
                }
            }
        }

        if self.provider_root_refresh_pending {
            let desired_digest = self
                .catalog
                .snapshot()
                .and_then(|catalog| catalog.provider_root_config_digest().map(str::to_owned));
            let published_matches = desired_digest.as_deref().is_some_and(|desired| {
                match pin_published_generation(data_root) {
                    Ok(Some(published)) => {
                        published
                            .verified_index()
                            .manifest()
                            .provider_root_config_digest()
                            == desired
                    }
                    Ok(None) => false,
                    Err(error) => {
                        let _ = write_degraded_wakeup_receipt(data_root, &error);
                        false
                    }
                }
            });
            let refresh_pending =
                source_refresh.is_some_and(CoreRefreshEngine::has_pending_request);
            if published_matches && !refresh_pending {
                self.provider_root_refresh_pending = false;
            } else if let Some(source_refresh) = source_refresh {
                if let Err(error) = source_refresh.enqueue_periodic(data_root) {
                    let _ = write_degraded_wakeup_receipt(data_root, &error);
                }
            }
        }

        let mut watcher_recreated = false;
        if self.file_watcher.is_none() && trigger.ensures_watcher() {
            match start_watcher(data_root, Arc::clone(&self.wakeup), self.catalog.clone()) {
                Ok(watcher) => {
                    self.file_watcher = Some(watcher);
                    watcher_recreated = true;
                }
                Err(error) => {
                    // Keep catalog recovery pending: while no watcher exists,
                    // a later safety pass must refresh both authorities before
                    // recreating the native backend.
                    self.catalog_refresh_pending = true;
                    let _ = write_degraded_wakeup_receipt(data_root, &error);
                }
            }
        }

        let mut affected = SourceWatchBatch::default();
        if !watcher_recreated && (catalog_published || trigger.reconciles_roots()) {
            if let Some(watcher) = self.file_watcher.as_mut() {
                let (batch, receipt) = watcher.reconcile_roots(force_rearm);
                affected = batch;
                if let Err(error) = receipt {
                    let _ = write_degraded_wakeup_receipt(data_root, &error);
                }
            }
        }

        let watcher_unavailable = self.file_watcher.is_none();
        let coordinator_needs_authority =
            source_refresh.is_some_and(|refresh| !refresh.watch_routes_initialized());
        let must_poll_without_watcher = watcher_unavailable && trigger.reconciles_roots();
        let must_initialize_authority = catalog_published
            || watcher_recreated
            || coordinator_needs_authority
            || must_poll_without_watcher;
        let mut pending_missing_schedules = 0_usize;
        if must_initialize_authority {
            if let (Some(catalog), Some(source_refresh)) = (self.catalog.snapshot(), source_refresh)
            {
                source_refresh.install_watch_catalog(catalog.clone());
                let watermark = self
                    .file_watcher
                    .as_ref()
                    .map(DaemonFileWatcher::startup_watermark)
                    .unwrap_or_else(|| EventWatermark::new(0, 0));
                if watcher_unavailable {
                    // Without a watcher no provider-neutral observation can
                    // close the event race. Poll every exact catalog route
                    // through the ordinary fail-closed refresh path on each
                    // safety pass until watcher recovery succeeds.
                    source_refresh.schedule_startup_route_reconciliation(
                        catalog.route_ids().cloned(),
                        watermark,
                        source_route_ledger_now_ms(),
                    );
                } else if coordinator_needs_authority || watcher_recreated {
                    // Startup and watcher replacement are exhaustive safety
                    // boundaries. Live watcher events may trust append-only
                    // growth, but these boundaries must authenticate existing
                    // history so an offline old-prefix rewrite cannot be
                    // carried forward as an append.
                    source_refresh.schedule_startup_route_reconciliation(
                        catalog.route_ids().cloned(),
                        watermark,
                        source_route_ledger_now_ms(),
                    );
                }
                self.schedule_pending_missing_routes(data_root, source_refresh);
                pending_missing_schedules = pending_missing_schedules.saturating_add(1);
            }
        }
        if !coordinator_needs_authority {
            if let Some(source_refresh) = source_refresh {
                if let (Some(catalog), Some(watermark)) =
                    (self.catalog.snapshot(), trigger.watermark())
                {
                    source_refresh.record_watch_routes_requiring_exhaustive_reconciliation(
                        catalog.route_ids().cloned().map(|route| (route, watermark)),
                        source_route_ledger_now_ms(),
                    );
                }
                source_refresh.record_watch_routes_requiring_exhaustive_reconciliation(
                    affected.routes,
                    source_route_ledger_now_ms(),
                );
            }
        }
        if matches!(trigger, WatchCatalogReconcileTrigger::SafetyTimeout)
            && pending_missing_schedules == 0
        {
            if let Some(source_refresh) = source_refresh {
                self.schedule_pending_missing_routes(data_root, source_refresh);
                pending_missing_schedules = pending_missing_schedules.saturating_add(1);
            }
        }
        pending_missing_schedules
    }
}
