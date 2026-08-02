use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Condvar, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ctx_history_capture::SourceBackedWatchCatalog;
use ctx_history_index::SourceRouteIdentity;
use notify::{
    event::{AccessKind, AccessMode, MetadataKind, ModifyKind},
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
const WATCH_RECEIPT_FILE: &str = "wakeup.json";

const WAKE_FILESYSTEM: u8 = 1;
const WAKE_IPC: u8 = 1 << 1;
const WAKE_SHUTDOWN: u8 = 1 << 2;
static NEXT_WATCHER_EPOCH: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Default)]
pub(super) struct SourceWatchBatch {
    pub(super) routes: BTreeMap<SourceRouteIdentity, EventWatermark>,
    pub(super) reconcile: Option<EventWatermark>,
}

impl SourceWatchBatch {
    fn is_empty(&self) -> bool {
        self.routes.is_empty() && self.reconcile.is_none()
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
    source_watch_batches: VecDeque<SourceWatchBatch>,
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
}

impl DaemonWakeup {
    #[cfg(test)]
    pub(super) fn signal_filesystem(&self) {
        self.signal(WAKE_FILESYSTEM);
    }

    fn signal_source_watch(&self, batch: SourceWatchBatch) {
        if batch.is_empty() {
            return;
        }
        let mut state = self.lock_state();
        state.pending |= WAKE_FILESYSTEM;
        state.filesystem_signals = state.filesystem_signals.saturating_add(1);
        state.source_watch_batches.push_back(batch);
        self.changed.notify_one();
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
        let mut source_watch = SourceWatchBatch::default();
        while let Some(batch) = state.source_watch_batches.pop_front() {
            source_watch.merge(batch);
        }
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
    Event(notify::Result<Event>),
    Stop,
}

#[derive(Debug, Default)]
struct WatchCounters {
    raw_events: u64,
    ignored_access_events: u64,
    ignored_catalog_lock_events: u64,
    ignored_access_time_events: u64,
    ignored_other_events: u64,
    last_ignored_access_path: Option<PathBuf>,
    backend_errors: u64,
    coalesced_wakeups: u64,
    reconciliations: u64,
    last_relevant_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct WatchAuthority {
    routes: SourceBackedWatchCatalog,
    controls: BTreeSet<PathBuf>,
}

impl WatchAuthority {
    fn new(data_root: &Path, routes: SourceBackedWatchCatalog) -> Self {
        Self {
            routes,
            controls: BTreeSet::from([
                data_root.join(CONFIG_FILE),
                data_root.join("catalogs").join("explicit-sources"),
            ]),
        }
    }

    fn target_paths(&self) -> impl Iterator<Item = &Path> {
        self.controls
            .iter()
            .map(PathBuf::as_path)
            .chain(self.routes.target_paths())
    }
}

pub(super) struct DaemonFileWatcher {
    data_root: PathBuf,
    wakeup: Arc<DaemonWakeup>,
    watcher: RecommendedWatcher,
    watched: BTreeMap<PathBuf, bool>,
    authority: Arc<RwLock<WatchAuthority>>,
    counters: Arc<Mutex<WatchCounters>>,
    sender: mpsc::Sender<WatchMessage>,
    thread: Option<thread::JoinHandle<()>>,
    last_error: Option<String>,
    watcher_epoch: u64,
}

impl DaemonFileWatcher {
    pub(super) fn start(
        data_root: &Path,
        wakeup: Arc<DaemonWakeup>,
        catalog: SourceBackedWatchCatalog,
    ) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let authority = Arc::new(RwLock::new(WatchAuthority::new(data_root, catalog)));
        let counters = Arc::new(Mutex::new(WatchCounters::default()));
        let callback_sender = sender.clone();
        let callback_counters = Arc::clone(&counters);
        let callback_data_root = data_root.to_path_buf();
        let watcher = RecommendedWatcher::new(
            move |event: notify::Result<Event>| {
                forward_watch_event(
                    &callback_data_root,
                    &callback_counters,
                    &callback_sender,
                    event,
                );
            },
            Config::default(),
        )
        .context("start native daemon filesystem watcher")?;
        let thread_authority = Arc::clone(&authority);
        let thread_counters = Arc::clone(&counters);
        let thread_wakeup = Arc::clone(&wakeup);
        let thread_data_root = data_root.to_path_buf();
        let thread_daemon_root = daemon_root_path(data_root);
        let watcher_epoch = NEXT_WATCHER_EPOCH.fetch_add(1, Ordering::Relaxed);
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
                    watcher_epoch,
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
            thread: Some(thread),
            last_error: None,
            watcher_epoch,
        };
        service.reconcile_roots()?;
        service.write_receipt("active")?;
        Ok(service)
    }

    pub(super) fn startup_watermark(&self) -> EventWatermark {
        EventWatermark::new(self.watcher_epoch, 0)
    }

    pub(super) fn catalog(&self) -> SourceBackedWatchCatalog {
        self.authority
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .routes
            .clone()
    }

    pub(super) fn replace_catalog(&mut self, catalog: SourceBackedWatchCatalog) -> Result<()> {
        *self
            .authority
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            WatchAuthority::new(&self.data_root, catalog);
        self.reconcile_roots()
    }

    pub(super) fn reconcile_roots(&mut self) -> Result<()> {
        let authority = self
            .authority
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let desired = watch_roots(authority.target_paths());
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
            if self.watched.get(path) == Some(recursive) {
                continue;
            }
            let mode = if *recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            match self.watcher.watch(path, mode) {
                Ok(()) => {
                    self.watched.insert(path.clone(), *recursive);
                }
                Err(error) => {
                    self.last_error = Some(format!("watch {}: {error}", path.display()));
                }
            }
        }
        {
            let mut counters = self.lock_counters();
            counters.reconciliations = counters.reconciliations.saturating_add(1);
        }
        self.write_receipt(if self.last_error.is_some() {
            "degraded"
        } else {
            "active"
        })
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
                .routes.route_ids().len(),
            "raw_events": counters.raw_events,
            "ignored_access_events": counters.ignored_access_events,
            "ignored_catalog_lock_events": counters.ignored_catalog_lock_events,
            "ignored_access_time_events": counters.ignored_access_time_events,
            "ignored_other_events": counters.ignored_other_events,
            "last_ignored_access_path": counters.last_ignored_access_path,
            "backend_errors": counters.backend_errors,
            "coalesced_wakeups": counters.coalesced_wakeups,
            "reconciliations": counters.reconciliations,
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

fn forward_watch_event(
    data_root: &Path,
    counters: &Mutex<WatchCounters>,
    sender: &mpsc::Sender<WatchMessage>,
    event: notify::Result<Event>,
) {
    if let Ok(event) = event.as_ref() {
        if let Some(kind) = ignored_watch_event(data_root, event) {
            record_ignored_watch_event(counters, event, kind);
            return;
        }
    }
    let _ = sender.send(WatchMessage::Event(event));
}

fn watch_event_loop(
    receiver: mpsc::Receiver<WatchMessage>,
    authority: Arc<RwLock<WatchAuthority>>,
    counters: Arc<Mutex<WatchCounters>>,
    wakeup: Arc<DaemonWakeup>,
    data_root: PathBuf,
    daemon_root: PathBuf,
    watcher_epoch: u64,
) {
    let mut sequence = 0_u64;
    loop {
        let first = match receiver.recv() {
            Ok(WatchMessage::Event(event)) => event,
            Ok(WatchMessage::Stop) | Err(_) => return,
        };
        let started = Instant::now();
        sequence = sequence.saturating_add(1);
        let mut relevant = record_watch_event(
            &authority,
            &counters,
            &data_root,
            &daemon_root,
            first,
            EventWatermark::new(watcher_epoch, sequence),
        );
        loop {
            let elapsed = started.elapsed();
            if elapsed >= WATCH_DEBOUNCE_MAX {
                break;
            }
            let timeout = WATCH_DEBOUNCE_QUIET.min(WATCH_DEBOUNCE_MAX - elapsed);
            match receiver.recv_timeout(timeout) {
                Ok(WatchMessage::Event(event)) => {
                    sequence = sequence.saturating_add(1);
                    relevant.merge(record_watch_event(
                        &authority,
                        &counters,
                        &data_root,
                        &daemon_root,
                        event,
                        EventWatermark::new(watcher_epoch, sequence),
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
            return SourceWatchBatch {
                reconcile: Some(watermark),
                ..SourceWatchBatch::default()
            };
        }
    };
    if let Some(kind) = ignored_watch_event(data_root, &event) {
        record_ignored_watch_event(counters, &event, kind);
        return SourceWatchBatch::default();
    }
    let authority = authority
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        for route in authority.routes.routes_overlapping_path(event_path) {
            batch.routes.insert(route, watermark);
            relevant_path.get_or_insert_with(|| event_path.clone());
        }
    }
    if event.paths.is_empty() {
        batch.reconcile = Some(watermark);
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

#[derive(Clone, Copy)]
enum IgnoredWatchEvent {
    Access,
    CatalogLock,
    AccessTime,
}

fn ignored_watch_event(data_root: &Path, event: &Event) -> Option<IgnoredWatchEvent> {
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
    let catalog_lock = data_root
        .join("catalogs")
        .join("explicit-sources")
        .join("catalog.lock");
    if !event.paths.is_empty() && event.paths.iter().all(|path| path == &catalog_lock) {
        return Some(IgnoredWatchEvent::CatalogLock);
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
        IgnoredWatchEvent::CatalogLock => {
            counters.ignored_catalog_lock_events =
                counters.ignored_catalog_lock_events.saturating_add(1);
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
mod tests {
    use super::*;
    use ctx_history_capture::{
        ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
        ProviderSourceStatus, SourceBackedProviderRegistry, SourceBackedRoute,
        SourceBackedRouteDriver, SourceBackedSelectorAuthority,
    };
    use ctx_history_core::CaptureProvider;

    fn catalog_route(
        provider: CaptureProvider,
        path: PathBuf,
        source_format: &'static str,
    ) -> SourceBackedRoute {
        SourceBackedRoute::automatic(
            ProviderSource {
                provider,
                path,
                exists: true,
                source_format,
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
            },
            SourceBackedSelectorAuthority::DiscoveredWinner,
            SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
        )
        .unwrap()
    }

    fn watch_catalog(
        routes: impl IntoIterator<Item = SourceBackedRoute>,
    ) -> SourceBackedWatchCatalog {
        let mut registry = SourceBackedProviderRegistry::new();
        for route in routes {
            registry.register(route);
        }
        registry.watch_catalog()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn catalog_lock_query_churn_stays_idle_but_provider_append_wakes() {
        use std::{fs::OpenOptions, io::Write};

        let temp = tempfile::tempdir().expect("create watcher fixture");
        let data_root = temp.path().join("data");
        let catalog_root = data_root.join("catalogs").join("explicit-sources");
        let provider_root = temp.path().join("provider");
        let provider_file = provider_root.join("session.jsonl");
        fs::create_dir_all(&catalog_root).expect("create catalog root");
        fs::create_dir_all(&provider_root).expect("create provider root");
        fs::write(
            catalog_root.join("catalog-00000000000000000001.json"),
            b"{\"revision\":1}\n",
        )
        .expect("write catalog");
        fs::write(catalog_root.join("catalog.lock"), b"").expect("write catalog lock");
        fs::write(&provider_file, b"{\"event\":1}\n").expect("write provider fixture");

        let targets = Arc::new(RwLock::new(WatchAuthority::new(
            &data_root,
            watch_catalog([catalog_route(
                CaptureProvider::Codex,
                provider_file.clone(),
                "codex_history_jsonl",
            )]),
        )));
        let counters = Arc::new(Mutex::new(WatchCounters::default()));
        let wakeup = Arc::new(DaemonWakeup::default());
        let (sender, receiver) = mpsc::channel();
        let callback_sender = sender.clone();
        let callback_counters = Arc::clone(&counters);
        let callback_data_root = data_root.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<Event>| {
                forward_watch_event(
                    &callback_data_root,
                    &callback_counters,
                    &callback_sender,
                    event,
                );
            },
            Config::default(),
        )
        .expect("start fixture watcher");
        watcher
            .watch(&catalog_root, RecursiveMode::Recursive)
            .expect("watch catalog");
        watcher
            .watch(&provider_root, RecursiveMode::Recursive)
            .expect("watch provider");

        let thread_targets = Arc::clone(&targets);
        let thread_counters = Arc::clone(&counters);
        let thread_wakeup = Arc::clone(&wakeup);
        let thread_data_root = data_root.clone();
        let thread_daemon_root = daemon_root_path(&data_root);
        let watch_thread = thread::spawn(move || {
            watch_event_loop(
                receiver,
                thread_targets,
                thread_counters,
                thread_wakeup,
                thread_data_root,
                thread_daemon_root,
                1,
            );
        });

        for _ in 0..128 {
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .open(catalog_root.join("catalog.lock"))
                .expect("open catalog lock like a query");
            fs::read_to_string(catalog_root.join("catalog-00000000000000000001.json"))
                .expect("query catalog");
            drop(lock);
        }
        thread::sleep(WATCH_DEBOUNCE_QUIET * 3);

        let idle = wakeup.snapshot();
        assert_eq!(idle["filesystem_signals"], 0, "{idle:#}");
        assert_eq!(idle["work_cycles"], 0, "{idle:#}");
        assert_eq!(idle["no_work_cycles"], 0, "{idle:#}");
        let counters_after_churn = counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(counters_after_churn.coalesced_wakeups, 0);
        assert_eq!(counters_after_churn.reconciliations, 0);
        assert!(counters_after_churn.ignored_catalog_lock_events > 0);
        drop(counters_after_churn);

        let mut file = OpenOptions::new()
            .append(true)
            .open(provider_file)
            .expect("open provider fixture for append");
        file.write_all(b"{\"event\":2}\n")
            .expect("append provider event");
        file.flush().expect("flush provider append");
        drop(file);
        let wake = wakeup.wait(Duration::from_secs(2));
        assert!(wake.filesystem, "provider append did not wake the daemon");
        assert!(!wake.timed_out, "provider append exceeded two seconds");
        assert_eq!(wake.source_watch.routes.len(), 1);
        assert!(wake.source_watch.reconcile.is_none());

        sender.send(WatchMessage::Stop).expect("stop watch thread");
        watch_thread.join().expect("join watch thread");
        drop(watcher);
    }

    #[test]
    fn wakeup_blocks_until_signaled_and_coalesces_reasons() {
        let wakeup = Arc::new(DaemonWakeup::default());
        wakeup.signal_filesystem();
        wakeup.signal_ipc();
        let wake = wakeup.wait(Duration::from_secs(1));
        assert!(wake.filesystem);
        assert!(!wake.shutdown);
        assert!(!wake.timed_out);
        assert_eq!(wakeup.snapshot()["ipc_signals"], 1);
    }

    #[test]
    fn sqlite_companion_files_are_exact_catalog_targets() {
        let catalog = watch_catalog([catalog_route(
            CaptureProvider::OpenCode,
            PathBuf::from("/tmp/history.sqlite"),
            "opencode_sqlite",
        )]);
        assert_eq!(
            catalog
                .routes_overlapping_path(Path::new("/tmp/history.sqlite-wal"))
                .len(),
            1
        );
        assert_eq!(
            catalog
                .routes_overlapping_path(Path::new("/tmp/history.sqlite-shm"))
                .len(),
            1
        );
        assert!(catalog
            .routes_overlapping_path(Path::new("/tmp/unrelated.sqlite-wal"))
            .is_empty());
    }

    #[test]
    fn missing_target_uses_exact_nearest_ancestor_without_sibling_matching() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp
            .path()
            .join("missing")
            .join("nested")
            .join("history.jsonl");
        let catalog = watch_catalog([catalog_route(
            CaptureProvider::Codex,
            missing.clone(),
            "codex_history_jsonl",
        )]);
        let roots = watch_roots(catalog.target_paths());
        assert_eq!(roots, BTreeMap::from([(temp.path().to_path_buf(), false)]));
        assert_eq!(
            catalog
                .routes_overlapping_path(&temp.path().join("missing"))
                .len(),
            1
        );
        assert!(catalog
            .routes_overlapping_path(
                &temp
                    .path()
                    .join("unrelated")
                    .join("nested")
                    .join("history.jsonl"),
            )
            .is_empty());
    }

    #[test]
    fn core_owned_writes_do_not_retrigger_provider_refresh_or_increment_work_counters() {
        let data_root = Path::new("/tmp/ctx-data");
        let daemon_root = data_root.join("daemon");
        let targets = RwLock::new(WatchAuthority::new(
            data_root,
            watch_catalog([catalog_route(
                CaptureProvider::Codex,
                PathBuf::from("/tmp/provider/session.jsonl"),
                "codex_history_jsonl",
            )]),
        ));
        let counters = Mutex::new(WatchCounters::default());
        let event = |path: &Path| {
            let mut event = Event::new(notify::EventKind::Any);
            event.paths.push(path.to_path_buf());
            event
        };

        assert!(record_watch_event(
            &targets,
            &counters,
            data_root,
            &daemon_root,
            Ok(event(&daemon_root.join("wakeup.json"))),
            EventWatermark::new(1, 1),
        )
        .is_empty());
        assert!(record_watch_event(
            &targets,
            &counters,
            data_root,
            &daemon_root,
            Ok(event(data_root)),
            EventWatermark::new(1, 2),
        )
        .is_empty());
        let mut access = event(&data_root.join("config.toml"));
        access.kind = EventKind::Access(AccessKind::Read);
        assert!(record_watch_event(
            &targets,
            &counters,
            data_root,
            &daemon_root,
            Ok(access),
            EventWatermark::new(1, 3),
        )
        .is_empty());
        assert_eq!(counters.lock().unwrap().raw_events, 0);
        let mut access_time = event(&data_root.join("config.toml"));
        access_time.kind = EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime));
        assert!(record_watch_event(
            &targets,
            &counters,
            data_root,
            &daemon_root,
            Ok(access_time),
            EventWatermark::new(1, 4),
        )
        .is_empty());
        let mut catalog_lock = event(
            &data_root
                .join("catalogs")
                .join("explicit-sources")
                .join("catalog.lock"),
        );
        catalog_lock.kind = EventKind::Access(AccessKind::Close(AccessMode::Write));
        assert!(record_watch_event(
            &targets,
            &counters,
            data_root,
            &daemon_root,
            Ok(catalog_lock),
            EventWatermark::new(1, 5),
        )
        .is_empty());
        let mut close_write = event(&data_root.join("config.toml"));
        close_write.kind = EventKind::Access(AccessKind::Close(AccessMode::Write));
        assert!(!record_watch_event(
            &targets,
            &counters,
            data_root,
            &daemon_root,
            Ok(close_write),
            EventWatermark::new(1, 6),
        )
        .is_empty());
        assert!(!record_watch_event(
            &targets,
            &counters,
            data_root,
            &daemon_root,
            Ok(event(&data_root.join("config.toml"))),
            EventWatermark::new(1, 7),
        )
        .is_empty());
        assert_eq!(counters.lock().unwrap().raw_events, 2);
    }

    #[test]
    fn authoritative_changes_and_dynamic_discovery_remain_relevant() {
        use notify::event::{CreateKind, DataChange, RenameMode};

        let data_root = Path::new("/tmp/ctx-data");
        let daemon_root = data_root.join("daemon");
        let provider_file = PathBuf::from("/tmp/provider/session.jsonl");
        let sqlite = PathBuf::from("/tmp/provider/history.sqlite");
        let dynamic_source = PathBuf::from("/tmp/home/.codex/sessions");
        let catalog_root = data_root.join("catalogs").join("explicit-sources");
        let targets = RwLock::new(WatchAuthority::new(
            data_root,
            watch_catalog([
                catalog_route(
                    CaptureProvider::Codex,
                    provider_file.clone(),
                    "codex_history_jsonl",
                ),
                catalog_route(CaptureProvider::OpenCode, sqlite.clone(), "opencode_sqlite"),
                catalog_route(
                    CaptureProvider::Codex,
                    dynamic_source,
                    "codex_session_jsonl_tree",
                ),
            ]),
        ));
        let counters = Mutex::new(WatchCounters::default());
        let sequence = std::cell::Cell::new(0_u64);
        let relevant = |kind, paths: &[&Path]| {
            let mut event = Event::new(kind);
            event
                .paths
                .extend(paths.iter().map(|path| path.to_path_buf()));
            sequence.set(sequence.get().saturating_add(1));
            !record_watch_event(
                &targets,
                &counters,
                data_root,
                &daemon_root,
                Ok(event),
                EventWatermark::new(1, sequence.get()),
            )
            .is_empty()
        };

        assert!(relevant(
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
            &[&data_root.join("config.toml")],
        ));
        assert!(relevant(
            EventKind::Create(CreateKind::File),
            &[&catalog_root.join("catalog-00000000000000000002.json")],
        ));
        assert!(relevant(
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            &[&provider_file],
        ));
        assert!(relevant(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &[Path::new("/tmp/provider/session.tmp"), &provider_file],
        ));
        assert!(relevant(
            EventKind::Create(CreateKind::Folder),
            &[Path::new("/tmp/home/.codex")],
        ));
        assert!(relevant(
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            &[Path::new("/tmp/provider/history.sqlite-wal")],
        ));
        assert!(!record_watch_event(
            &targets,
            &counters,
            data_root,
            &daemon_root,
            Err(notify::Error::generic("overflow")),
            EventWatermark::new(1, 99),
        )
        .is_empty());
    }
}
