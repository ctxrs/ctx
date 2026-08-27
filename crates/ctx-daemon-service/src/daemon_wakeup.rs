use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use anyhow::Result;
use ctx_daemon_runtime::{
    create_private_dir_all, daemon_root_path, write_private_json_file, NativeWatchEvent,
};
use ctx_history_capture::SourceBackedWatchCatalog;
use ctx_history_index::SourceRouteIdentity;
use ctx_history_refresh::EventWatermark;
use serde_json::{json, Value};

use crate::{compact_json, config::CONFIG_FILE};

mod event_routing;
#[cfg(test)]
use event_routing::record_and_observe_watch_event;
use event_routing::{ignored_watch_event, record_ignored_watch_event, record_watch_event};

#[cfg(all(test, target_os = "linux"))]
use ctx_daemon_runtime::WATCH_DEBOUNCE_QUIET;
#[cfg(test)]
use ctx_daemon_runtime::WATCH_EVENT_QUEUE_CAPACITY;
const WATCH_RECEIPT_FILE: &str = "wakeup.json";

#[derive(Debug, Clone, Default)]
pub(super) struct SourceWatchBatch {
    pub(super) routes: BTreeMap<SourceRouteIdentity, EventWatermark>,
    /// Exact ordinary-file members for routes whose entire coalesced event
    /// batch remained member-specific. A missing entry means exhaustive work.
    pub(super) members: BTreeMap<SourceRouteIdentity, BTreeSet<PathBuf>>,
    pub(super) reconcile: Option<EventWatermark>,
    pub(super) rearm: bool,
}

impl SourceWatchBatch {
    fn is_empty(&self) -> bool {
        self.routes.is_empty() && self.reconcile.is_none() && !self.rearm
    }

    fn merge(&mut self, other: Self) {
        if let Some(watermark) = other.reconcile {
            self.routes.clear();
            self.members.clear();
            self.reconcile = Some(
                self.reconcile
                    .map_or(watermark, |current| current.max(watermark)),
            );
            self.rearm |= other.rearm;
            return;
        }
        if self.reconcile.is_some() {
            self.rearm |= other.rearm;
            return;
        }
        let mut other_members = other.members;
        for (route, watermark) in other.routes {
            let already_recorded = self.routes.contains_key(&route);
            let incoming_members = other_members.remove(&route);
            self.routes
                .entry(route.clone())
                .and_modify(|current| *current = (*current).max(watermark))
                .or_insert(watermark);
            match (
                already_recorded,
                self.members.get_mut(&route),
                incoming_members,
            ) {
                (false, _, Some(members)) => {
                    self.members.insert(route, members);
                }
                (false, _, None) | (true, Some(_), None) => {
                    self.members.remove(&route);
                }
                (true, Some(current), Some(additional)) => {
                    current.extend(additional);
                }
                (true, None, _) => {}
            }
        }
        self.rearm |= other.rearm;
    }

    fn record_route(
        &mut self,
        route: SourceRouteIdentity,
        watermark: EventWatermark,
        member: Option<PathBuf>,
    ) {
        let mut batch = Self::default();
        batch.routes.insert(route.clone(), watermark);
        if let Some(member) = member {
            batch.members.insert(route, BTreeSet::from([member]));
        }
        self.merge(batch);
    }

    fn uncertainty(watermark: EventWatermark) -> Self {
        Self {
            reconcile: Some(watermark),
            rearm: true,
            ..Self::default()
        }
    }
}

impl ctx_daemon_runtime::CoalescingWakePayload for SourceWatchBatch {
    fn is_empty(&self) -> bool {
        SourceWatchBatch::is_empty(self)
    }

    fn merge(&mut self, other: Self) {
        SourceWatchBatch::merge(self, other);
    }
}

pub(super) type SourceWatchSink = Arc<dyn Fn(&SourceWatchBatch) + Send + Sync>;

#[derive(Debug, Clone, Default)]
pub(super) struct DaemonWake {
    pub(super) filesystem: bool,
    pub(super) shutdown: bool,
    pub(super) timed_out: bool,
    pub(super) source_watch: SourceWatchBatch,
}

#[derive(Default)]
pub(super) struct DaemonWakeup {
    inner: ctx_daemon_runtime::Wakeup<SourceWatchBatch>,
    #[cfg(test)]
    before_source_watch_sink_dispatch: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl std::fmt::Debug for DaemonWakeup {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DaemonWakeup")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl DaemonWakeup {
    #[cfg(test)]
    pub(super) fn signal_filesystem(&self) {
        self.inner.signal_filesystem();
    }

    fn observe_source_watch(&self, batch: &SourceWatchBatch) {
        #[cfg(not(test))]
        self.inner.observe_payload(batch);
        #[cfg(test)]
        self.inner.observe_payload_before_dispatch(batch, || {
            let hook = self
                .before_source_watch_sink_dispatch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(hook) = hook {
                hook();
            }
        });
    }

    fn signal_source_watch(&self, batch: SourceWatchBatch) {
        self.inner.signal_payload(batch);
    }

    pub(super) fn install_source_watch_sink(&self, sink: SourceWatchSink) {
        self.inner.install_payload_sink(sink);
    }

    pub(super) fn has_source_watch_sink(&self) -> bool {
        self.inner.has_payload_sink()
    }

    #[cfg(test)]
    fn install_before_source_watch_sink_dispatch_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        let previous = self
            .before_source_watch_sink_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(hook);
        assert!(previous.is_none(), "source-watch test hooks must not nest");
    }

    pub(super) fn signal_ipc(&self) {
        self.inner.signal_ipc();
    }

    pub(super) fn signal_shutdown(&self) {
        self.inner.signal_shutdown();
    }

    pub(super) fn wait(&self, timeout: Duration) -> DaemonWake {
        let wake = self.inner.wait(timeout);
        DaemonWake {
            filesystem: wake.filesystem,
            shutdown: wake.shutdown,
            timed_out: wake.timed_out,
            source_watch: wake.payload,
        }
    }

    #[cfg(test)]
    fn pending_source_watch(&self) -> SourceWatchBatch {
        self.inner.pending_payload()
    }

    pub(super) fn record_cycle(&self, did_work: bool) {
        self.inner.record_cycle(did_work);
    }

    pub(super) fn record_scheduled_retry_wakeup(&self) {
        self.inner.record_scheduled_retry_wakeup();
    }

    pub(super) fn record_scheduled_refresh_wakeup(&self) {
        self.inner.record_scheduled_refresh_wakeup();
    }

    fn snapshot(&self) -> Value {
        serde_json::to_value(self.inner.snapshot()).unwrap_or_else(|_| json!({}))
    }
}

#[derive(Debug, Default)]
struct WatchCounters {
    raw_events: u64,
    ignored_access_events: u64,
    ignored_access_time_events: u64,
    ignored_other_events: u64,
    last_ignored_access_path: Option<PathBuf>,
    backend_errors: u64,
    rescan_notifications: u64,
    last_relevant_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct DaemonWatchCatalog {
    state: Arc<RwLock<WatchCatalogState>>,
}

#[derive(Debug, Default)]
struct WatchCatalogState {
    snapshot: Option<SourceBackedWatchCatalog>,
    uncertain_through: Option<EventWatermark>,
}

impl DaemonWatchCatalog {
    pub(super) fn publish(&self, catalog: SourceBackedWatchCatalog) {
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot = Some(catalog);
    }

    pub(super) fn snapshot(&self) -> Option<SourceBackedWatchCatalog> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .snapshot
            .clone()
    }

    pub(super) fn fence_uncertainty(&self, watermark: EventWatermark) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.uncertain_through = Some(
            state
                .uncertain_through
                .map_or(watermark, |current| current.max(watermark)),
        );
    }

    pub(super) fn uncertainty_watermark(&self) -> Option<EventWatermark> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .uncertain_through
    }

    pub(super) fn clear_uncertainty_if_covered(&self, covered_through: EventWatermark) -> bool {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .uncertain_through
            .is_some_and(|current| current <= covered_through)
        {
            state.uncertain_through = None;
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone)]
struct WatchAuthority {
    catalog: DaemonWatchCatalog,
    controls: BTreeSet<PathBuf>,
}

impl WatchAuthority {
    fn new(data_root: &Path, catalog: DaemonWatchCatalog) -> Self {
        Self {
            catalog,
            controls: BTreeSet::from([data_root.join(CONFIG_FILE)]),
        }
    }

    fn target_paths(&self) -> Vec<PathBuf> {
        let mut paths = self.controls.iter().cloned().collect::<Vec<_>>();
        if let Some(catalog) = self.catalog.snapshot() {
            paths.extend(catalog.target_paths().map(Path::to_path_buf));
        }
        paths
    }
}

pub(super) struct DaemonFileWatcher {
    data_root: PathBuf,
    wakeup: Arc<DaemonWakeup>,
    authority: Arc<RwLock<WatchAuthority>>,
    counters: Arc<Mutex<WatchCounters>>,
    runtime: ctx_daemon_runtime::NativeFileWatcher,
    last_error: Option<String>,
}

impl DaemonFileWatcher {
    pub(super) fn start(
        data_root: &Path,
        wakeup: Arc<DaemonWakeup>,
        catalog: DaemonWatchCatalog,
    ) -> Result<Self> {
        let authority = Arc::new(RwLock::new(WatchAuthority::new(data_root, catalog)));
        let counters = Arc::new(Mutex::new(WatchCounters::default()));
        let classifier_authority = Arc::clone(&authority);
        let classifier_counters = Arc::clone(&counters);
        let classifier_data_root = data_root.to_path_buf();
        let classifier_daemon_root = daemon_root_path(data_root);
        let classify_event = Arc::new(
            move |event, watermark: ctx_daemon_runtime::WatchWatermark| {
                record_watch_event(
                    &classifier_authority,
                    &classifier_counters,
                    &classifier_data_root,
                    &classifier_daemon_root,
                    event,
                    EventWatermark::new(watermark.epoch, watermark.sequence),
                )
            },
        );
        let reconciliation_authority = Arc::clone(&authority);
        let ignored_data_root = data_root.to_path_buf();
        let ignored_counters = Arc::clone(&counters);
        let ignore_event = Arc::new(move |event: &NativeWatchEvent| {
            if event.needs_rescan() {
                return false;
            }
            let Some(kind) = ignored_watch_event(&ignored_data_root, event) else {
                return false;
            };
            record_ignored_watch_event(&ignored_counters, event, kind);
            true
        });
        let observed_wakeup = Arc::clone(&wakeup);
        let signal_wakeup = Arc::clone(&wakeup);
        let runtime = ctx_daemon_runtime::NativeFileWatcher::start(
            "ctx-daemon-watch",
            ignore_event,
            classify_event,
            Arc::new(move |watermark: ctx_daemon_runtime::WatchWatermark| {
                let watermark = EventWatermark::new(watermark.epoch, watermark.sequence);
                reconciliation_authority
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .catalog
                    .fence_uncertainty(watermark);
                SourceWatchBatch::uncertainty(watermark)
            }),
            Arc::new(move |batch| observed_wakeup.observe_source_watch(batch)),
            Arc::new(move |batch| signal_wakeup.signal_source_watch(batch)),
        )?;
        let mut service = Self {
            data_root: data_root.to_path_buf(),
            wakeup,
            authority,
            counters,
            runtime,
            last_error: None,
        };
        let (_, registration) = service.reconcile_roots(false);
        registration?;
        Ok(service)
    }

    pub(super) fn startup_watermark(&self) -> EventWatermark {
        let watermark = self.runtime.startup_watermark();
        EventWatermark::new(watermark.epoch, watermark.sequence)
    }

    pub(super) fn reconcile_roots(&mut self, force_rearm: bool) -> (SourceWatchBatch, Result<()>) {
        let authority = self
            .authority
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let catalog = authority.catalog.snapshot();
        let desired_paths = authority.target_paths();
        let desired = ctx_daemon_runtime::watch_roots(desired_paths.iter().map(PathBuf::as_path));
        let force_rearm = force_rearm || authority.catalog.uncertainty_watermark().is_some();
        let replace_native_watcher = self.runtime.replacement_required(force_rearm);
        let registration_needed = self.runtime.needs_registration(&desired, force_rearm);
        let affected = if registration_needed && !replace_native_watcher {
            catalog
                .as_ref()
                .map(|catalog| {
                    let watermark = self.next_watermark();
                    SourceWatchBatch {
                        routes: catalog
                            .route_ids()
                            .cloned()
                            .map(|route| (route, watermark))
                            .collect(),
                        ..SourceWatchBatch::default()
                    }
                })
                .unwrap_or_default()
        } else {
            SourceWatchBatch::default()
        };
        self.last_error = catalog
            .is_none()
            .then(|| "watch catalog authority is unavailable".to_owned());
        let registration = self.runtime.reconcile_paths(desired, force_rearm);
        if let Err(error) = registration.as_ref() {
            self.last_error = Some(error.to_string());
        }
        let receipt = self.write_receipt(if self.last_error.is_some() {
            "degraded"
        } else {
            "active"
        });
        let affected = if registration.is_ok() {
            affected
        } else {
            SourceWatchBatch::default()
        };
        (affected, registration.and(receipt))
    }

    pub(super) fn uncertainty_watermark(&self) -> Option<EventWatermark> {
        self.authority
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .uncertainty_watermark()
    }

    pub(super) fn clear_uncertainty_if_covered(&self, covered_through: EventWatermark) -> bool {
        self.authority
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .clear_uncertainty_if_covered(covered_through)
    }

    pub(super) fn worker_failed(&self) -> bool {
        self.runtime.worker_failed()
    }

    fn next_watermark(&self) -> EventWatermark {
        let watermark = self.runtime.next_watermark();
        EventWatermark::new(watermark.epoch, watermark.sequence)
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn install_rearm_overlap_hook(&mut self, hook: impl FnMut(&Path) + 'static) {
        self.runtime.install_rearm_overlap_hook(hook);
    }

    pub(super) fn write_receipt(&self, status: &str) -> Result<()> {
        let counters = self.lock_counters();
        let wakeup = self.wakeup.snapshot();
        let runtime = self.runtime.snapshot();
        let value = compact_json(json!({
            "schema_version": 1,
            "status": status,
            "backend": "notify_recommended",
            "idle_strategy": "blocking",
            "watched_roots": runtime.watched_roots,
            "catalog_routes": self.authority.read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .catalog.snapshot()
                .map_or(0, |catalog| catalog.route_ids().len()),
            "raw_events": counters.raw_events,
            "ignored_access_events": counters.ignored_access_events,
            "ignored_access_time_events": counters.ignored_access_time_events,
            "ignored_other_events": counters.ignored_other_events,
            "last_ignored_access_path": counters.last_ignored_access_path,
            "backend_errors": counters.backend_errors,
            "rescan_notifications": counters.rescan_notifications,
            "ingress_overflows": runtime.ingress_overflows,
            "ingress_disconnects": runtime.ingress_disconnects,
            "coalesced_wakeups": runtime.coalesced_wakeups,
            "reconciliations": runtime.reconciliations,
            "forced_rearms": runtime.forced_rearms,
            "registration_attempts": runtime.registration_attempts,
            "last_relevant_path": counters.last_relevant_path,
            "last_error": self.last_error,
            "wakeup": wakeup,
        }));
        drop(counters);
        let root = daemon_root_path(&self.data_root);
        create_private_dir_all(&root)?;
        write_private_json_file(&root.join(WATCH_RECEIPT_FILE), &value)
    }

    fn lock_counters(&self) -> std::sync::MutexGuard<'_, WatchCounters> {
        self.counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for DaemonFileWatcher {
    fn drop(&mut self) {
        self.runtime.stop();
        let _ = self.write_receipt("stopped");
    }
}

pub(super) fn write_degraded_wakeup_receipt(data_root: &Path, error: &anyhow::Error) -> Result<()> {
    let root = daemon_root_path(data_root);
    create_private_dir_all(&root)?;
    write_private_json_file(
        &root.join(WATCH_RECEIPT_FILE),
        &compact_json(json!({
            "schema_version": 1,
            "status": "degraded",
            "backend": "notify_recommended",
            "idle_strategy": "blocking_safety_reconciliation",
            "watched_roots": 0,
            "last_error": format!("{error:#}"),
        })),
    )
}

pub fn daemon_wakeup_report(data_root: &Path) -> Value {
    let path = daemon_root_path(data_root).join(WATCH_RECEIPT_FILE);
    fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| {
            compact_json(json!({
                "schema_version": 1,
                "status": "unavailable",
                "receipt_path": path,
            }))
        })
}

#[cfg(test)]
mod tests;
