use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Condvar, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ctx_history_capture::SourceBackedWatchCatalog;
use ctx_history_index::SourceRouteIdentity;
use notify::{
    event::{AccessKind, AccessMode, CreateKind, MetadataKind, ModifyKind, RemoveKind},
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use serde_json::{json, Value};

use crate::{compact_json, config::CONFIG_FILE};

use super::{
    dirty_source_routes::EventWatermark,
    health_search::create_private_dir_all,
    paths_status::{daemon_root_path, write_private_json_file},
};

const WATCH_DEBOUNCE_QUIET: Duration = Duration::from_millis(250);
const WATCH_DEBOUNCE_MAX: Duration = Duration::from_secs(2);
// Native callbacks must never grow daemon memory or block the backend. A full
// queue is represented by a catalog reconciliation in the separately
// coalesced wake state, so dropping the individual native payload is safe.
const WATCH_EVENT_QUEUE_CAPACITY: usize = 256;
const WATCH_RECEIPT_FILE: &str = "wakeup.json";

const WAKE_FILESYSTEM: u8 = 1;
const WAKE_IPC: u8 = 1 << 1;
const WAKE_SHUTDOWN: u8 = 1 << 2;
static NEXT_WATCHER_EPOCH: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Default)]
pub(super) struct SourceWatchBatch {
    pub(super) routes: BTreeMap<SourceRouteIdentity, EventWatermark>,
    pub(super) reconcile: Option<EventWatermark>,
    pub(super) rearm: bool,
}

impl SourceWatchBatch {
    fn is_empty(&self) -> bool {
        self.routes.is_empty() && self.reconcile.is_none() && !self.rearm
    }

    fn merge(&mut self, other: Self) {
        for (route, watermark) in other.routes {
            self.routes
                .entry(route)
                .and_modify(|current| *current = (*current).max(watermark))
                .or_insert(watermark);
        }
        if let Some(watermark) = other.reconcile {
            self.reconcile = Some(
                self.reconcile
                    .map_or(watermark, |current| current.max(watermark)),
            );
        }
        self.rearm |= other.rearm;
    }

    fn catalog_reconciliation(watermark: EventWatermark) -> Self {
        Self {
            reconcile: Some(watermark),
            rearm: true,
            ..Self::default()
        }
    }
}

pub(super) type SourceWatchSink = Arc<dyn Fn(&SourceWatchBatch) + Send + Sync>;

#[derive(Default)]
struct SourceWatchSinkSlot {
    sink: RwLock<Option<SourceWatchSink>>,
}

impl std::fmt::Debug for SourceWatchSinkSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceWatchSinkSlot")
            .field(
                "installed",
                &self
                    .sink
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_some(),
            )
            .finish()
    }
}

impl SourceWatchSinkSlot {
    fn set(&self, sink: Option<SourceWatchSink>) {
        *self
            .sink
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = sink;
    }

    fn is_installed(&self) -> bool {
        self.sink
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    fn dispatch(&self, batch: &SourceWatchBatch) -> bool {
        let sink = self
            .sink
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(sink) = sink {
            sink(batch);
            return true;
        }
        false
    }
}

#[derive(Debug, Default)]
struct DaemonWakeupState {
    pending: u8,
    filesystem_signals: u64,
    ipc_signals: u64,
    shutdown_signals: u64,
    blocking_waits: u64,
    timeout_wakeups: u64,
    scheduled_retry_wakeups: u64,
    work_cycles: u64,
    no_work_cycles: u64,
    // One merged batch replaces a queue of batches. Route entries therefore
    // cannot exceed the current exact watch-catalog cardinality.
    source_watch: SourceWatchBatch,
    // Exact observations are retained independently from the debounced daemon
    // wake. This closes sink installation races and lets an in-progress
    // publication fence events without forcing the synchronous loop awake for
    // every native callback.
    observed_source_watch: SourceWatchBatch,
}

#[derive(Debug, Clone, Default)]
pub(super) struct DaemonWake {
    pub(super) filesystem: bool,
    pub(super) shutdown: bool,
    pub(super) timed_out: bool,
    pub(super) source_watch: SourceWatchBatch,
}

#[derive(Debug, Default)]
pub(super) struct DaemonWakeup {
    state: Mutex<DaemonWakeupState>,
    changed: Condvar,
    source_watch_sink: SourceWatchSinkSlot,
    #[cfg(test)]
    source_watch_test_hook: SourceWatchTestHook,
}

#[cfg(test)]
#[derive(Default)]
struct SourceWatchTestHook {
    before_sink_dispatch: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[cfg(test)]
impl std::fmt::Debug for SourceWatchTestHook {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceWatchTestHook")
            .finish_non_exhaustive()
    }
}

impl DaemonWakeup {
    #[cfg(test)]
    pub(super) fn signal_filesystem(&self) {
        self.signal(WAKE_FILESYSTEM);
    }

    /// Records exact route observations immediately for the publication fence.
    ///
    /// The merged replay state is updated before any sink lookup or dispatch.
    /// Installation therefore observes every interleaving: either it replays
    /// this batch, or this call observes the installed sink and dispatches it.
    fn observe_source_watch(&self, batch: &SourceWatchBatch) {
        if batch.is_empty() {
            return;
        }
        self.lock_state().observed_source_watch.merge(batch.clone());
        #[cfg(test)]
        self.run_before_source_watch_sink_dispatch_hook();
        if self.source_watch_sink.dispatch(batch) {
            // The replay buffer is startup-only. Once an installed sink has
            // observed this batch, clearing the merged state prevents route
            // identities from accumulating across later catalog churn.
            self.lock_state().observed_source_watch = SourceWatchBatch::default();
        }
    }

    /// Coalesces the daemon wake independently from prompt ledger observation.
    fn signal_source_watch(&self, batch: SourceWatchBatch) {
        if batch.is_empty() {
            return;
        }
        let mut state = self.lock_state();
        state.pending |= WAKE_FILESYSTEM;
        state.filesystem_signals = state.filesystem_signals.saturating_add(1);
        state.source_watch.merge(batch);
        self.changed.notify_one();
    }

    pub(super) fn install_source_watch_sink(&self, sink: SourceWatchSink) {
        self.source_watch_sink.set(Some(sink));
        // Replay all observations merged before installation. Normal daemon
        // ingestion remains idempotent because route watermarks are monotonic.
        let pending = self.lock_state().observed_source_watch.clone();
        if !pending.is_empty() && self.source_watch_sink.dispatch(&pending) {
            self.lock_state().observed_source_watch = SourceWatchBatch::default();
        }
    }

    pub(super) fn has_source_watch_sink(&self) -> bool {
        self.source_watch_sink.is_installed()
    }

    #[cfg(test)]
    fn install_before_source_watch_sink_dispatch_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        let previous = self
            .source_watch_test_hook
            .before_sink_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(hook);
        assert!(previous.is_none(), "source-watch test hooks must not nest");
    }

    #[cfg(test)]
    fn run_before_source_watch_sink_dispatch_hook(&self) {
        let hook = self
            .source_watch_test_hook
            .before_sink_dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(hook) = hook {
            hook();
        }
    }

    pub(super) fn signal_ipc(&self) {
        self.signal(WAKE_IPC);
    }

    pub(super) fn signal_shutdown(&self) {
        self.signal(WAKE_SHUTDOWN);
    }

    fn signal(&self, reason: u8) {
        let mut state = self.lock_state();
        state.pending |= reason;
        if reason == WAKE_FILESYSTEM {
            state.filesystem_signals = state.filesystem_signals.saturating_add(1);
        } else if reason == WAKE_IPC {
            state.ipc_signals = state.ipc_signals.saturating_add(1);
        } else if reason == WAKE_SHUTDOWN {
            state.shutdown_signals = state.shutdown_signals.saturating_add(1);
        }
        self.changed.notify_one();
    }

    pub(super) fn wait(&self, timeout: Duration) -> DaemonWake {
        let mut state = self.lock_state();
        state.blocking_waits = state.blocking_waits.saturating_add(1);
        let timed_out = if state.pending == 0 {
            let (next, result) = self
                .changed
                .wait_timeout_while(state, timeout, |state| state.pending == 0)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            result.timed_out() && state.pending == 0
        } else {
            false
        };
        if timed_out {
            state.timeout_wakeups = state.timeout_wakeups.saturating_add(1);
        }
        let pending = std::mem::take(&mut state.pending);
        let source_watch = std::mem::take(&mut state.source_watch);
        DaemonWake {
            filesystem: pending & WAKE_FILESYSTEM != 0,
            shutdown: pending & WAKE_SHUTDOWN != 0,
            timed_out,
            source_watch,
        }
    }

    pub(super) fn record_cycle(&self, did_work: bool) {
        let mut state = self.lock_state();
        if did_work {
            state.work_cycles = state.work_cycles.saturating_add(1);
        } else {
            state.no_work_cycles = state.no_work_cycles.saturating_add(1);
        }
    }

    pub(super) fn record_scheduled_retry_wakeup(&self) {
        let mut state = self.lock_state();
        state.scheduled_retry_wakeups = state.scheduled_retry_wakeups.saturating_add(1);
    }

    fn snapshot(&self) -> Value {
        let state = self.lock_state();
        compact_json(json!({
            "blocking_waits": state.blocking_waits,
            "filesystem_signals": state.filesystem_signals,
            "ipc_signals": state.ipc_signals,
            "shutdown_signals": state.shutdown_signals,
            "timeout_wakeups": state.timeout_wakeups,
            "scheduled_retry_wakeups": state.scheduled_retry_wakeups,
            "work_cycles": state.work_cycles,
            "no_work_cycles": state.no_work_cycles,
        }))
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, DaemonWakeupState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

enum WatchMessage {
    Event {
        event: notify::Result<Event>,
        watermark: EventWatermark,
    },
    Stop,
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
    ingress_overflows: u64,
    ingress_disconnects: u64,
    coalesced_wakeups: u64,
    reconciliations: u64,
    forced_rearms: u64,
    registration_attempts: u64,
    last_relevant_path: Option<PathBuf>,
}

/// The daemon's single authoritative watch-catalog snapshot.
///
/// The native callback thread and the daemon reconciliation loop share this
/// exact owner. `None` is deliberately distinct from an authoritative empty
/// catalog: it means catalog construction has not succeeded yet and must not
/// initialize coordinator route authority.
#[derive(Debug, Clone, Default)]
pub(super) struct DaemonWatchCatalog {
    snapshot: Arc<RwLock<Option<SourceBackedWatchCatalog>>>,
}

impl DaemonWatchCatalog {
    pub(super) fn publish(&self, catalog: SourceBackedWatchCatalog) {
        *self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(catalog);
    }

    pub(super) fn snapshot(&self) -> Option<SourceBackedWatchCatalog> {
        self.snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
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

#[cfg(test)]
type RearmOverlapHook = Box<dyn FnMut(&Path)>;

pub(super) struct DaemonFileWatcher {
    data_root: PathBuf,
    wakeup: Arc<DaemonWakeup>,
    watcher: RecommendedWatcher,
    watched: BTreeMap<PathBuf, bool>,
    authority: Arc<RwLock<WatchAuthority>>,
    counters: Arc<Mutex<WatchCounters>>,
    sender: mpsc::SyncSender<WatchMessage>,
    accepting_events: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    last_error: Option<String>,
    rearm_pending: bool,
    watcher_epoch: u64,
    callback_sequence: Arc<AtomicU64>,
    #[cfg(test)]
    rearm_overlap_hook: Option<RearmOverlapHook>,
}

fn native_file_watcher(
    data_root: &Path,
    sender: &mpsc::SyncSender<WatchMessage>,
    counters: &Arc<Mutex<WatchCounters>>,
    wakeup: &Arc<DaemonWakeup>,
    accepting_events: &Arc<AtomicBool>,
    watcher_epoch: u64,
    callback_sequence: &Arc<AtomicU64>,
) -> Result<RecommendedWatcher> {
    let callback_data_root = data_root.to_path_buf();
    let callback_sender = sender.clone();
    let callback_counters = Arc::clone(counters);
    let callback_wakeup = Arc::clone(wakeup);
    let callback_accepting_events = Arc::clone(accepting_events);
    let callback_sequence = Arc::clone(callback_sequence);
    RecommendedWatcher::new(
        move |event: notify::Result<Event>| {
            forward_watch_event(
                &callback_data_root,
                &callback_counters,
                &callback_sender,
                &callback_wakeup,
                &callback_accepting_events,
                watcher_epoch,
                &callback_sequence,
                event,
            );
        },
        Config::default(),
    )
    .context("start native daemon filesystem watcher")
}

impl DaemonFileWatcher {
    pub(super) fn start(
        data_root: &Path,
        wakeup: Arc<DaemonWakeup>,
        catalog: DaemonWatchCatalog,
    ) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(WATCH_EVENT_QUEUE_CAPACITY);
        let authority = Arc::new(RwLock::new(WatchAuthority::new(data_root, catalog)));
        let counters = Arc::new(Mutex::new(WatchCounters::default()));
        let accepting_events = Arc::new(AtomicBool::new(true));
        let watcher_epoch = NEXT_WATCHER_EPOCH.fetch_add(1, Ordering::Relaxed);
        let callback_sequence = Arc::new(AtomicU64::new(0));
        let watcher = native_file_watcher(
            data_root,
            &sender,
            &counters,
            &wakeup,
            &accepting_events,
            watcher_epoch,
            &callback_sequence,
        )?;
        let thread_authority = Arc::clone(&authority);
        let thread_counters = Arc::clone(&counters);
        let thread_wakeup = Arc::clone(&wakeup);
        let thread_data_root = data_root.to_path_buf();
        let thread_daemon_root = daemon_root_path(data_root);
        let thread = thread::Builder::new()
            .name("ctx-daemon-watch".to_owned())
            .spawn(move || {
                watch_event_loop(
                    receiver,
                    thread_authority,
                    thread_counters,
                    thread_wakeup,
                    thread_data_root,
                    thread_daemon_root,
                );
            })
            .context("start daemon filesystem debounce worker")?;
        let mut service = Self {
            data_root: data_root.to_path_buf(),
            wakeup,
            watcher,
            watched: BTreeMap::new(),
            authority,
            counters,
            sender,
            accepting_events,
            thread: Some(thread),
            last_error: None,
            rearm_pending: false,
            watcher_epoch,
            callback_sequence,
            #[cfg(test)]
            rearm_overlap_hook: None,
        };
        let (_, registration) = service.reconcile_roots(false);
        registration?;
        Ok(service)
    }

    pub(super) fn startup_watermark(&self) -> EventWatermark {
        EventWatermark::new(self.watcher_epoch, 0)
    }

    pub(super) fn reconcile_roots(&mut self, force_rearm: bool) -> (SourceWatchBatch, Result<()>) {
        let authority = self
            .authority
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let catalog = authority.catalog.snapshot();
        let desired_paths = authority.target_paths();
        let desired = watch_roots(desired_paths.iter().map(PathBuf::as_path));
        self.rearm_pending |= force_rearm;
        let replace_native_watcher = self.rearm_pending;
        let registration_needed = desired.iter().any(|(path, recursive)| {
            replace_native_watcher || self.watched.get(path).copied() != Some(*recursive)
        });
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
        let mut registration_attempts = 0_u64;
        if replace_native_watcher {
            match native_file_watcher(
                &self.data_root,
                &self.sender,
                &self.counters,
                &self.wakeup,
                &self.accepting_events,
                self.watcher_epoch,
                &self.callback_sequence,
            ) {
                Ok(mut replacement) => {
                    let mut replacement_ready = true;
                    for (path, recursive) in &desired {
                        registration_attempts = registration_attempts.saturating_add(1);
                        let mode = if *recursive {
                            RecursiveMode::Recursive
                        } else {
                            RecursiveMode::NonRecursive
                        };
                        if let Err(error) = replacement.watch(path, mode) {
                            replacement_ready = false;
                            self.last_error = Some(format!("watch {}: {error}", path.display()));
                        }
                    }
                    if replacement_ready {
                        // Both native watchers are live during this hook. Any
                        // mutation in the handoff is delivered through the
                        // normal exact-route callback path, so a clean rearm
                        // does not need to manufacture an all-routes batch.
                        #[cfg(test)]
                        for path in desired.keys() {
                            if let Some(hook) = self.rearm_overlap_hook.as_mut() {
                                hook(path);
                            }
                        }
                        self.watcher = replacement;
                        self.watched = desired;
                        self.rearm_pending = false;
                    }
                }
                Err(error) => {
                    self.last_error = Some(error.to_string());
                }
            }
        } else {
            let stale = self
                .watched
                .keys()
                .filter(|path| !desired.contains_key(*path))
                .cloned()
                .collect::<Vec<_>>();
            for path in stale {
                if let Err(error) = self.watcher.unwatch(&path) {
                    self.last_error = Some(format!("unwatch {}: {error}", path.display()));
                }
                self.watched.remove(&path);
            }
            for (path, recursive) in &desired {
                let current = self.watched.get(path).copied();
                if current == Some(*recursive) {
                    continue;
                }
                if current.is_some() {
                    if let Err(error) = self.watcher.unwatch(path) {
                        self.last_error = Some(format!("unwatch {}: {error}", path.display()));
                    }
                    self.watched.remove(path);
                }
                let mode = if *recursive {
                    RecursiveMode::Recursive
                } else {
                    RecursiveMode::NonRecursive
                };
                registration_attempts = registration_attempts.saturating_add(1);
                match self.watcher.watch(path, mode) {
                    Ok(()) => {
                        self.watched.insert(path.clone(), *recursive);
                    }
                    Err(error) => {
                        self.last_error = Some(format!("watch {}: {error}", path.display()));
                    }
                }
            }
        }
        {
            let mut counters = self.lock_counters();
            counters.reconciliations = counters.reconciliations.saturating_add(1);
            counters.registration_attempts = counters
                .registration_attempts
                .saturating_add(registration_attempts);
            if force_rearm {
                counters.forced_rearms = counters.forced_rearms.saturating_add(1);
            }
        }
        let receipt = self.write_receipt(if self.last_error.is_some() {
            "degraded"
        } else {
            "active"
        });
        (affected, receipt)
    }

    fn next_watermark(&self) -> EventWatermark {
        EventWatermark::new(
            self.watcher_epoch,
            self.callback_sequence
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(current.saturating_add(1))
                })
                .unwrap_or_else(|current| current)
                .saturating_add(1),
        )
    }

    #[cfg(test)]
    pub(super) fn install_rearm_overlap_hook(&mut self, hook: impl FnMut(&Path) + 'static) {
        self.rearm_overlap_hook = Some(Box::new(hook));
    }

    pub(super) fn write_receipt(&self, status: &str) -> Result<()> {
        let counters = self.lock_counters();
        let wakeup = self.wakeup.snapshot();
        let value = compact_json(json!({
            "schema_version": 1,
            "status": status,
            "backend": "notify_recommended",
            "idle_strategy": "blocking",
            "watched_roots": self.watched.len(),
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
            "ingress_overflows": counters.ingress_overflows,
            "ingress_disconnects": counters.ingress_disconnects,
            "coalesced_wakeups": counters.coalesced_wakeups,
            "reconciliations": counters.reconciliations,
            "forced_rearms": counters.forced_rearms,
            "registration_attempts": counters.registration_attempts,
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
        self.accepting_events.store(false, Ordering::Release);
        let _ = self.sender.send(WatchMessage::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Persist the final in-memory blocking/wakeup/work counters only at
        // explicit service teardown. Idle operation itself performs no receipt
        // heartbeat writes.
        let _ = self.write_receipt("stopped");
    }
}

// This is the narrow adapter between notify's callback and the independently
// owned queue, wakeup, lifecycle, and watermark state.
#[allow(clippy::too_many_arguments)]
fn forward_watch_event(
    data_root: &Path,
    counters: &Mutex<WatchCounters>,
    sender: &mpsc::SyncSender<WatchMessage>,
    wakeup: &DaemonWakeup,
    accepting_events: &AtomicBool,
    watcher_epoch: u64,
    sequence: &AtomicU64,
    event: notify::Result<Event>,
) {
    if !accepting_events.load(Ordering::Acquire) {
        return;
    }
    if let Ok(event) = event.as_ref() {
        if !event.need_rescan() {
            if let Some(kind) = ignored_watch_event(data_root, event) {
                record_ignored_watch_event(counters, event, kind);
                return;
            }
        }
    }
    let watermark = EventWatermark::new(
        watcher_epoch,
        sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(1))
            })
            .unwrap_or_else(|current| current)
            .saturating_add(1),
    );
    match sender.try_send(WatchMessage::Event { event, watermark }) {
        Ok(()) => {}
        Err(mpsc::TrySendError::Full(_)) => {
            let mut counters = counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counters.ingress_overflows = counters.ingress_overflows.saturating_add(1);
            drop(counters);
            wakeup.signal_source_watch(SourceWatchBatch::catalog_reconciliation(watermark));
        }
        Err(mpsc::TrySendError::Disconnected(_)) => {
            let mut counters = counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counters.ingress_disconnects = counters.ingress_disconnects.saturating_add(1);
            drop(counters);
            wakeup.signal_source_watch(SourceWatchBatch::catalog_reconciliation(watermark));
        }
    }
}

fn watch_event_loop(
    receiver: mpsc::Receiver<WatchMessage>,
    authority: Arc<RwLock<WatchAuthority>>,
    counters: Arc<Mutex<WatchCounters>>,
    wakeup: Arc<DaemonWakeup>,
    data_root: PathBuf,
    daemon_root: PathBuf,
) {
    loop {
        let (first, first_watermark) = match receiver.recv() {
            Ok(WatchMessage::Event { event, watermark }) => (event, watermark),
            Ok(WatchMessage::Stop) | Err(_) => return,
        };
        let started = Instant::now();
        let mut relevant = record_and_observe_watch_event(
            &authority,
            &counters,
            &wakeup,
            &data_root,
            &daemon_root,
            first,
            first_watermark,
        );
        loop {
            let elapsed = started.elapsed();
            if elapsed >= WATCH_DEBOUNCE_MAX {
                break;
            }
            let timeout = WATCH_DEBOUNCE_QUIET.min(WATCH_DEBOUNCE_MAX - elapsed);
            match receiver.recv_timeout(timeout) {
                Ok(WatchMessage::Event { event, watermark }) => {
                    relevant.merge(record_and_observe_watch_event(
                        &authority,
                        &counters,
                        &wakeup,
                        &data_root,
                        &daemon_root,
                        event,
                        watermark,
                    ));
                }
                Ok(WatchMessage::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
        if !relevant.is_empty() {
            let mut counters = counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counters.coalesced_wakeups = counters.coalesced_wakeups.saturating_add(1);
            drop(counters);
            wakeup.signal_source_watch(relevant);
        }
    }
}

fn record_and_observe_watch_event(
    authority: &RwLock<WatchAuthority>,
    counters: &Mutex<WatchCounters>,
    wakeup: &DaemonWakeup,
    data_root: &Path,
    daemon_root: &Path,
    event: notify::Result<Event>,
    watermark: EventWatermark,
) -> SourceWatchBatch {
    let batch = record_watch_event(
        authority,
        counters,
        data_root,
        daemon_root,
        event,
        watermark,
    );
    wakeup.observe_source_watch(&batch);
    batch
}

fn record_watch_event(
    authority: &RwLock<WatchAuthority>,
    counters: &Mutex<WatchCounters>,
    data_root: &Path,
    daemon_root: &Path,
    event: notify::Result<Event>,
    watermark: EventWatermark,
) -> SourceWatchBatch {
    let event = match event {
        Ok(event) => event,
        Err(_) => {
            let mut counters = counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counters.backend_errors = counters.backend_errors.saturating_add(1);
            return SourceWatchBatch::catalog_reconciliation(watermark);
        }
    };
    if event.need_rescan() {
        let mut counters = counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        counters.raw_events = counters.raw_events.saturating_add(1);
        counters.rescan_notifications = counters.rescan_notifications.saturating_add(1);
        counters.last_relevant_path = event.paths.first().cloned();
        return SourceWatchBatch::catalog_reconciliation(watermark);
    }
    if let Some(kind) = ignored_watch_event(data_root, &event) {
        record_ignored_watch_event(counters, &event, kind);
        return SourceWatchBatch::default();
    }
    let authority = authority
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let catalog = authority.catalog.snapshot();
    let mut batch = SourceWatchBatch::default();
    let mut relevant_path = None;
    for event_path in &event.paths {
        if event_path.as_path() == data_root || event_path.starts_with(daemon_root) {
            continue;
        }
        if authority
            .controls
            .iter()
            .any(|target| declared_control_paths_overlap(target, event_path))
        {
            batch.reconcile = Some(watermark);
            relevant_path.get_or_insert_with(|| event_path.clone());
        }
        if let Some(catalog) = catalog.as_ref() {
            for route in catalog.routes_overlapping_path(event_path) {
                batch.routes.insert(route, watermark);
                relevant_path.get_or_insert_with(|| event_path.clone());
            }
        }
    }
    if event.paths.is_empty() {
        batch.reconcile = Some(watermark);
        batch.rearm = true;
    } else if !batch.is_empty() && watch_event_requires_rearm(&event) {
        batch.rearm = true;
    }
    drop(authority);
    if !batch.is_empty() {
        let mut counters = counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        counters.raw_events = counters.raw_events.saturating_add(1);
        counters.last_relevant_path = relevant_path;
    } else {
        let mut counters = counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        counters.ignored_other_events = counters.ignored_other_events.saturating_add(1);
    }
    batch
}

fn watch_event_requires_rearm(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Any
            | EventKind::Other
            | EventKind::Create(CreateKind::Any | CreateKind::Folder | CreateKind::Other)
            | EventKind::Modify(ModifyKind::Any | ModifyKind::Name(_) | ModifyKind::Other)
            | EventKind::Remove(RemoveKind::Any | RemoveKind::Folder | RemoveKind::Other)
    )
}

#[derive(Clone, Copy)]
enum IgnoredWatchEvent {
    Access,
    AccessTime,
}

fn ignored_watch_event(_data_root: &Path, event: &Event) -> Option<IgnoredWatchEvent> {
    if matches!(
        event.kind,
        EventKind::Access(kind)
            if !matches!(kind, AccessKind::Close(AccessMode::Write))
    ) {
        return Some(IgnoredWatchEvent::Access);
    }
    if matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime))
    ) {
        return Some(IgnoredWatchEvent::AccessTime);
    }
    None
}

fn record_ignored_watch_event(
    counters: &Mutex<WatchCounters>,
    event: &Event,
    kind: IgnoredWatchEvent,
) {
    let mut counters = counters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match kind {
        IgnoredWatchEvent::Access => {
            counters.ignored_access_events = counters.ignored_access_events.saturating_add(1);
            counters.last_ignored_access_path = event.paths.first().cloned();
        }
        IgnoredWatchEvent::AccessTime => {
            counters.ignored_access_time_events =
                counters.ignored_access_time_events.saturating_add(1);
        }
    }
}

fn declared_control_paths_overlap(target: &Path, event: &Path) -> bool {
    target == event || target.starts_with(event) || event.starts_with(target)
}

fn watch_roots<'a>(targets: impl IntoIterator<Item = &'a Path>) -> BTreeMap<PathBuf, bool> {
    let mut roots = BTreeMap::new();
    for target in targets {
        if target.is_dir() {
            roots
                .entry(target.to_path_buf())
                .and_modify(|recursive| *recursive = true)
                .or_insert(true);
            continue;
        }
        if target.is_file() {
            if let Some(parent) = target.parent() {
                roots.entry(parent.to_path_buf()).or_insert(false);
            }
            continue;
        }
        if let Some(existing) = nearest_existing_ancestor(target) {
            roots.entry(existing).or_insert(false);
        }
    }
    roots
}

fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|candidate| candidate.is_dir())
        .map(Path::to_path_buf)
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

pub(super) fn daemon_wakeup_report(data_root: &Path) -> Value {
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
