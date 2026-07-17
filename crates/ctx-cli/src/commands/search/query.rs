#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum RefreshArg {
    Background,
    Off,
    Wait,
}

impl RefreshArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Off => "off",
            Self::Wait => "wait",
        }
    }
}
#[derive(Debug, Clone)]
pub(crate) struct SearchRefreshReport {
    mode: RefreshArg,
    status: &'static str,
    source_count: usize,
    totals: ImportTotals,
    error: Option<String>,
}

#[derive(Debug)]
struct SearchRefreshFailure {
    error: anyhow::Error,
    totals: ImportTotals,
}

pub(crate) struct SearchRefreshRuntime {
    cached_work: Option<SearchRefreshWork>,
    source_watcher: Option<SearchRefreshSourceWatcher>,
    source_watcher_paths: Option<Vec<PathBuf>>,
    source_watcher_path_identities: Option<Vec<WatchedSourcePathIdentity>>,
    watcher_degraded: bool,
    watcher_recovery_pending: bool,
    watcher_error: Option<String>,
    watcher_retry_failures: u32,
    next_watcher_retry_at: Option<Instant>,
    next_watcher_retry_at_ms: Option<i64>,
    degraded_inventory_passes: u32,
    pending_inventory_reason: Option<SearchInventoryReason>,
    inventory_progress: Option<SearchInventoryProgress>,
    pending_dirty_paths: BTreeSet<SearchDirtyPath>,
    pending_inventory_sources: BTreeSet<PathBuf>,
    pending_full_inventory: bool,
    inventory_error: Option<String>,
    last_inventory_completed_at_ms: Option<i64>,
    next_inventory_at: Option<Instant>,
    next_inventory_at_ms: Option<i64>,
    durable_source_fingerprint: Option<String>,
    restored_daemon_status: bool,
    disk_io_pacer: Option<(ImportExecutionPolicy, DiskIoPacer)>,
}

struct SearchRefreshWork {
    source_fingerprint: String,
    publication_state_marker: String,
    publication_owner: Option<ProviderFilePublicationInventoryOwner>,
    passes_since_reinventory: usize,
    last_reinventory_at: Instant,
    plan: ImportPlan,
    execution_state: crate::commands::import::ImportExecutionState,
    inventory_failures: Vec<ImportSourceFailure>,
    failed_inventory_pending: (usize, usize),
    inventoried_source_count: usize,
    planned_total_bytes: u64,
}

struct SearchRefreshSourceWatcher {
    changes: Arc<Mutex<SearchRefreshSourceChanges>>,
    healthy: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    registered_watch_count: usize,
    directory_source_count: usize,
    _watcher: RecommendedWatcher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchInventoryReason {
    Startup,
    SourcesChanged,
    PublicationChanged,
    WatcherLoss,
    WatcherRecovery,
    DegradedFallback,
    HealthySweep,
}

impl SearchInventoryReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::SourcesChanged => "sources_changed",
            Self::PublicationChanged => "publication_changed",
            Self::WatcherLoss => "watcher_loss",
            Self::WatcherRecovery => "watcher_recovery",
            Self::DegradedFallback => "degraded_fallback",
            Self::HealthySweep => "healthy_sweep",
        }
    }
}

struct SearchInventoryProgress {
    source_fingerprint: String,
    reason: SearchInventoryReason,
    started_at_ms: i64,
    next_source_index: usize,
    total_sources: usize,
    completed_source_bytes: u64,
    completed_directory_entry_stat_operations: u64,
    completed_path_bytes: u64,
    active_source_index: Option<usize>,
    active_discovered_files: usize,
    scoped: bool,
    cursor: ImportInventoryCursor,
}

#[derive(Debug, Clone)]
struct SearchRefreshWatch {
    source_path: PathBuf,
    match_path: PathBuf,
    watch_path: PathBuf,
    recursive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SearchDirtyPath {
    source_path: PathBuf,
    changed_path: PathBuf,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SearchRefreshSourceChanges {
    full_rebuild: bool,
    dirty_paths: BTreeSet<SearchDirtyPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchedSourcePathIdentity {
    exists: bool,
    is_dir: bool,
    stable_id: Option<(u64, u64)>,
    fallback_len: Option<u64>,
    fallback_modified_at: Option<SystemTime>,
}

struct SearchRefreshExecution {
    totals: ImportTotals,
}

const DAEMON_SEARCH_REFRESH_REINVENTORY_INTERVAL: StdDuration = StdDuration::from_secs(5 * 60);
const MAX_SEARCH_REFRESH_ROOT_WATCHES: usize = 64;
const MAX_PENDING_DIRTY_PATHS: usize = 4_096;
const DAEMON_SEARCH_REFRESH_RETRY_DELAYS: [StdDuration; 5] = [
    StdDuration::from_secs(30),
    StdDuration::from_secs(60),
    StdDuration::from_secs(2 * 60),
    StdDuration::from_secs(4 * 60),
    StdDuration::from_secs(5 * 60),
];

impl Default for SearchRefreshRuntime {
    fn default() -> Self {
        Self {
            cached_work: None,
            source_watcher: None,
            source_watcher_paths: None,
            source_watcher_path_identities: None,
            watcher_degraded: false,
            watcher_recovery_pending: false,
            watcher_error: None,
            watcher_retry_failures: 0,
            next_watcher_retry_at: None,
            next_watcher_retry_at_ms: None,
            degraded_inventory_passes: 0,
            pending_inventory_reason: None,
            inventory_progress: None,
            pending_dirty_paths: BTreeSet::new(),
            pending_inventory_sources: BTreeSet::new(),
            pending_full_inventory: false,
            inventory_error: None,
            last_inventory_completed_at_ms: None,
            next_inventory_at: None,
            next_inventory_at_ms: None,
            durable_source_fingerprint: None,
            restored_daemon_status: false,
            disk_io_pacer: None,
        }
    }
}

impl SearchRefreshRuntime {
    pub(crate) fn install_daemon_disk_io_pacing(
        &mut self,
    ) -> ctx_history_capture::DiskIoPacingGuard {
        ctx_history_capture::install_disk_io_pacer(
            self.disk_io_pacer(ImportExecutionPolicy::Daemon),
        )
    }

    pub(crate) fn disk_io_pacer(&mut self, policy: ImportExecutionPolicy) -> DiskIoPacer {
        if self
            .disk_io_pacer
            .as_ref()
            .is_none_or(|(current, _)| *current != policy)
        {
            self.disk_io_pacer = Some((policy, policy.disk_io_pacer()));
        }
        self.disk_io_pacer
            .as_ref()
            .map(|(_, pacer)| pacer.clone())
            .expect("search refresh pacer must be initialized")
    }

    fn invalidate(&mut self) {
        self.cached_work = None;
    }

    pub(crate) fn restore_daemon_status(&mut self, value: Option<&Value>) {
        if self.restored_daemon_status {
            return;
        }
        self.restored_daemon_status = true;
        let Some(value) = value else {
            return;
        };
        let now = Instant::now();
        let now_ms = search_refresh_now_ms();
        self.durable_source_fingerprint = value
            .get("source_fingerprint")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(watcher) = value.get("watcher") {
            self.watcher_degraded = watcher
                .get("degraded")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            self.watcher_recovery_pending = watcher
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| state == "recovering");
            self.watcher_error = watcher
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned);
            self.watcher_retry_failures = watcher
                .get("retry_failures")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0);
            self.next_watcher_retry_at_ms = watcher.get("next_retry_at_ms").and_then(Value::as_i64);
            self.next_watcher_retry_at = self
                .next_watcher_retry_at_ms
                .and_then(|at_ms| search_refresh_future_instant(now, now_ms, at_ms));
        }
        let Some(inventory) = value.get("inventory") else {
            return;
        };
        if self.durable_source_fingerprint.is_none() {
            self.durable_source_fingerprint = inventory
                .get("source_fingerprint")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        self.inventory_error = inventory
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.last_inventory_completed_at_ms = inventory
            .get("last_completed_at_ms")
            .and_then(Value::as_i64);
        self.next_inventory_at_ms = inventory.get("next_at_ms").and_then(Value::as_i64);
        self.next_inventory_at = self
            .next_inventory_at_ms
            .and_then(|at_ms| search_refresh_future_instant(now, now_ms, at_ms));
        self.degraded_inventory_passes = inventory
            .get("degraded_passes")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        if inventory.get("state").and_then(Value::as_str) != Some("in_progress") {
            return;
        }
        // Inventory pages are accumulated in memory. After a daemon restart,
        // replay from page zero so previously completed pages cannot disappear
        // from the rebuilt plan. Preserve quiet restart behavior by waiting one
        // degraded-inventory interval instead of immediately rescanning.
        let restart_delay = daemon_search_refresh_retry_delay(1);
        self.next_inventory_at = now.checked_add(restart_delay);
        self.next_inventory_at_ms = search_refresh_timestamp_after(now_ms, restart_delay);
    }

    pub(crate) fn daemon_status_json(&self) -> Value {
        let now = Instant::now();
        let watcher_state = if self.watcher_degraded {
            if self.source_watcher.is_some() && self.watcher_recovery_pending {
                "recovering"
            } else {
                "degraded"
            }
        } else if self.source_watcher.is_some() {
            "healthy"
        } else {
            "initializing"
        };
        let inventory_progress = self.inventory_progress.as_ref().map(|progress| {
            json!({
                "source_fingerprint": progress.source_fingerprint,
                "reason": progress.reason.as_str(),
                "started_at_ms": progress.started_at_ms,
                "completed_sources": progress.next_source_index,
                "total_sources": progress.total_sources,
                "source_bytes": progress.completed_source_bytes,
                "directory_entry_stat_operations": progress.completed_directory_entry_stat_operations,
                "path_bytes": progress.completed_path_bytes,
                "active_source_index": progress.active_source_index,
                "active_discovered_files": progress.active_discovered_files,
            })
        });
        json!({
            "watcher": {
                "state": watcher_state,
                "degraded": self.watcher_degraded,
                "error": self.watcher_error,
                "retry_failures": self.watcher_retry_failures,
                "next_retry_at_ms": self.next_watcher_retry_at_ms,
                "next_retry_after_ms": self.next_watcher_retry_at.map(|retry| {
                    u64::try_from(retry.saturating_duration_since(now).as_millis())
                        .unwrap_or(u64::MAX)
                }),
                "coverage": self.source_watcher.as_ref().map(|watcher| {
                    if watcher.directory_source_count == 0 { "complete" } else { "root_only" }
                }).unwrap_or("none"),
                "registered_paths": self.source_watcher.as_ref().map(|watcher| watcher.registered_watch_count).unwrap_or(0),
                "directory_sources": self.source_watcher.as_ref().map(|watcher| watcher.directory_source_count).unwrap_or(0),
            },
            "inventory": {
                "state": if self.inventory_progress.is_some() { "in_progress" } else { "idle" },
                "source_fingerprint": self.durable_source_fingerprint,
                "progress": inventory_progress,
                "error": self.inventory_error,
                "last_completed_at_ms": self.last_inventory_completed_at_ms,
                "next_at_ms": self.next_inventory_at_ms,
                "next_fallback_at_ms": self.watcher_degraded.then_some(self.next_inventory_at_ms).flatten(),
                "next_sweep_at_ms": (!self.watcher_degraded).then_some(self.next_inventory_at_ms).flatten(),
                "degraded_passes": self.degraded_inventory_passes,
            },
        })
    }

    fn watcher_changes(&mut self, sources: &[SourceInfo]) -> Option<SearchRefreshSourceChanges> {
        let now = Instant::now();
        let now_ms = search_refresh_now_ms();
        let watched_paths = watched_source_paths(sources);
        let watched_path_identities = watched_source_path_identities(&watched_paths);
        let watcher_was_configured = self.source_watcher_paths.is_some();
        let paths_changed = self
            .source_watcher_paths
            .as_ref()
            .is_none_or(|current| *current != watched_paths);
        let path_identities_changed = self
            .source_watcher_path_identities
            .as_ref()
            .is_none_or(|current| *current != watched_path_identities);
        let watcher_unhealthy = self
            .source_watcher
            .as_ref()
            .is_some_and(|watcher| !watcher.is_healthy());
        if watcher_unhealthy {
            let error = self
                .source_watcher
                .as_ref()
                .and_then(SearchRefreshSourceWatcher::error)
                .unwrap_or_else(|| "source watcher stopped reporting reliable events".to_owned());
            self.cached_work = None;
            self.source_watcher = None;
            self.enter_watcher_degraded(error, now, now_ms, true);
        }
        if paths_changed || path_identities_changed {
            if watcher_was_configured {
                self.cached_work = None;
                self.request_inventory(SearchInventoryReason::SourcesChanged);
            }
            if path_identities_changed && watcher_was_configured {
                self.enter_watcher_degraded(
                    "watched source identity changed".to_owned(),
                    now,
                    now_ms,
                    true,
                );
            }
            self.source_watcher = None;
            self.source_watcher_paths = Some(watched_paths.clone());
            self.source_watcher_path_identities = Some(watched_path_identities);
        }

        let retry_due = self.source_watcher.is_none()
            && self.next_watcher_retry_at.is_none_or(|retry| now >= retry);
        let target_setup_due = (paths_changed || path_identities_changed)
            && (!self.watcher_degraded
                || self.next_watcher_retry_at.is_none_or(|retry| now >= retry));
        if target_setup_due || retry_due {
            match SearchRefreshSourceWatcher::new(watched_paths) {
                Ok(watcher) => {
                    let root_only_coverage = watcher.directory_source_count > 0;
                    self.source_watcher = Some(watcher);
                    self.next_watcher_retry_at = None;
                    self.next_watcher_retry_at_ms = None;
                    if root_only_coverage {
                        self.watcher_degraded = true;
                        self.watcher_recovery_pending = false;
                        self.watcher_error = Some(
                            "recursive watcher registration is disabled; directory sources use bounded periodic reconciliation"
                                .to_owned(),
                        );
                        self.watcher_retry_failures = 0;
                    } else if self.watcher_degraded {
                        self.watcher_recovery_pending = true;
                        self.request_inventory(SearchInventoryReason::WatcherRecovery);
                    }
                }
                Err(error) => {
                    self.enter_watcher_degraded(
                        error_summary(&error),
                        now,
                        now_ms,
                        self.inventory_progress.is_none(),
                    );
                }
            }
        }
        self.source_watcher
            .as_ref()
            .map(SearchRefreshSourceWatcher::take_changes)
    }

    fn enter_watcher_degraded(
        &mut self,
        error: String,
        now: Instant,
        now_ms: i64,
        immediate_reconciliation: bool,
    ) {
        let newly_degraded = !self.watcher_degraded;
        self.watcher_degraded = true;
        self.watcher_recovery_pending = false;
        self.watcher_error = Some(error);
        if newly_degraded {
            self.degraded_inventory_passes = 0;
        }
        if immediate_reconciliation && newly_degraded {
            self.request_inventory(SearchInventoryReason::WatcherLoss);
        }
        self.schedule_watcher_retry(now, now_ms);
    }

    fn schedule_watcher_retry(&mut self, now: Instant, now_ms: i64) {
        self.watcher_retry_failures = self.watcher_retry_failures.saturating_add(1);
        let delay = daemon_search_refresh_retry_delay(self.watcher_retry_failures);
        self.next_watcher_retry_at = now.checked_add(delay);
        self.next_watcher_retry_at_ms = search_refresh_timestamp_after(now_ms, delay);
    }

    fn request_inventory(&mut self, reason: SearchInventoryReason) {
        self.pending_inventory_reason = Some(reason);
        self.pending_inventory_sources.clear();
        self.pending_full_inventory = true;
        self.next_inventory_at = None;
        self.next_inventory_at_ms = None;
    }

    fn request_source_inventory(&mut self, source_path: PathBuf) {
        if self.pending_inventory_reason.is_none() {
            self.pending_inventory_reason = Some(SearchInventoryReason::SourcesChanged);
        }
        if self.pending_inventory_reason == Some(SearchInventoryReason::SourcesChanged)
            && !self.pending_full_inventory
        {
            self.pending_inventory_sources.insert(source_path);
        }
    }

    fn retain_dirty_paths(&mut self, dirty_paths: &BTreeSet<SearchDirtyPath>) {
        for dirty in dirty_paths {
            if self.pending_dirty_paths.len() >= MAX_PENDING_DIRTY_PATHS
                && !self.pending_dirty_paths.contains(dirty)
            {
                self.pending_dirty_paths.clear();
                self.request_inventory(SearchInventoryReason::SourcesChanged);
                return;
            }
            self.pending_dirty_paths.insert(dirty.clone());
        }
    }

    fn prepare_inventory(
        &mut self,
        sources: &[SourceInfo],
        source_fingerprint: &str,
        publication_state_marker: &str,
        publication_owner: Option<ProviderFilePublicationInventoryOwner>,
        force_rebuild: bool,
    ) -> Result<bool> {
        if self.inventory_publication_state_changed(publication_state_marker) {
            self.restart_inventory_after_publication_transition();
        }
        if self
            .inventory_progress
            .as_ref()
            .is_some_and(|progress| progress.source_fingerprint != source_fingerprint)
        {
            self.inventory_progress = None;
            self.cached_work = None;
            self.request_inventory(SearchInventoryReason::SourcesChanged);
        }
        if self.inventory_progress.is_some() {
            return Ok(true);
        }
        let now = Instant::now();
        let requested_reason = self.pending_inventory_reason;
        let reason = requested_reason.or_else(|| {
            if force_rebuild {
                Some(SearchInventoryReason::SourcesChanged)
            } else if self.cached_work.is_none()
                && self.last_inventory_completed_at_ms.is_none()
                && self.next_inventory_at.is_none_or(|next| now >= next)
            {
                Some(SearchInventoryReason::Startup)
            } else if self.next_inventory_at.is_some_and(|next| now >= next) {
                Some(if self.watcher_degraded {
                    SearchInventoryReason::DegradedFallback
                } else {
                    SearchInventoryReason::HealthySweep
                })
            } else {
                None
            }
        });
        let Some(reason) = reason else {
            return Ok(false);
        };
        let scoped = reason == SearchInventoryReason::SourcesChanged
            && !self.pending_full_inventory
            && !self.pending_inventory_sources.is_empty();
        let inventory_sources = if scoped {
            sources
                .iter()
                .filter(|source| self.pending_inventory_sources.contains(&source.path))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            sources.to_vec()
        };
        let total_sources = inventory_sources
            .len()
            .saturating_add(usize::from(!scoped && publication_owner.is_some()));
        let cursor = ImportInventoryCursor::new_with_publication_snapshot(
            inventory_sources,
            true,
            !scoped,
            publication_state_marker.to_owned(),
            publication_owner,
        )?;
        if requested_reason.is_some() {
            self.pending_inventory_reason = None;
        }
        self.pending_inventory_sources.clear();
        self.pending_full_inventory = false;
        if !scoped {
            self.cached_work = None;
            // Watcher changes observed before this pass are covered by the
            // full inventory. Changes that arrive while it is running remain
            // queued for a follow-up pass.
            self.pending_dirty_paths.clear();
            self.next_inventory_at = None;
            self.next_inventory_at_ms = None;
        }
        self.inventory_error = None;
        self.inventory_progress = Some(SearchInventoryProgress {
            source_fingerprint: source_fingerprint.to_owned(),
            reason,
            started_at_ms: search_refresh_now_ms(),
            next_source_index: 0,
            total_sources,
            completed_source_bytes: 0,
            completed_directory_entry_stat_operations: 0,
            completed_path_bytes: 0,
            active_source_index: None,
            active_discovered_files: 0,
            scoped,
            cursor,
        });
        Ok(true)
    }

    fn inventory_publication_state_changed(&self, publication_state_marker: &str) -> bool {
        self.inventory_progress.as_ref().is_some_and(|progress| {
            progress.cursor.publication_state_marker() != publication_state_marker
        })
    }

    fn inventory_publication_snapshot(
        &self,
    ) -> Option<(String, Option<ProviderFilePublicationInventoryOwner>)> {
        self.inventory_progress.as_ref().map(|progress| {
            (
                progress.cursor.publication_state_marker().to_owned(),
                progress.cursor.publication_owner().cloned(),
            )
        })
    }

    fn restart_inventory_after_publication_transition(&mut self) {
        self.inventory_progress = None;
        self.cached_work = None;
        self.request_inventory(SearchInventoryReason::PublicationChanged);
    }

    fn durable_sources_changed(&self, source_fingerprint: &str) -> bool {
        self.durable_source_fingerprint
            .as_deref()
            .is_some_and(|durable| durable != source_fingerprint)
    }

    fn advance_inventory(&mut self, store: &Store) -> Result<ImportInventoryCursorStep> {
        let Some(progress) = self.inventory_progress.as_mut() else {
            return Err(anyhow!(
                "inventory advanced without an active inventory pass"
            ));
        };
        progress.active_source_index = (progress.next_source_index < progress.total_sources)
            .then_some(progress.next_source_index);
        progress.cursor.advance(store)
    }

    fn note_inventory_slice(
        &mut self,
        slice: ImportInventorySliceProgress,
        measured_operations: u64,
    ) {
        let Some(progress) = self.inventory_progress.as_mut() else {
            return;
        };
        progress.completed_directory_entry_stat_operations = progress
            .completed_directory_entry_stat_operations
            .saturating_add(measured_operations.max(slice.operations));
        progress.completed_path_bytes = progress
            .completed_path_bytes
            .saturating_add(slice.path_bytes);
        progress.active_discovered_files = slice.discovered_files;
    }

    fn note_inventory_source_completed(&mut self, source_bytes: u64, operations: u64) {
        let Some(progress) = self.inventory_progress.as_mut() else {
            return;
        };
        progress.next_source_index = progress.next_source_index.saturating_add(1);
        progress.completed_source_bytes =
            progress.completed_source_bytes.saturating_add(source_bytes);
        progress.completed_directory_entry_stat_operations = progress
            .completed_directory_entry_stat_operations
            .saturating_add(operations);
        progress.active_source_index = None;
        progress.active_discovered_files = 0;
        self.inventory_error = None;
    }

    fn note_inventory_cursor_complete(&mut self) {
        if let Some(progress) = self.inventory_progress.as_mut() {
            progress.next_source_index = progress.total_sources;
            progress.active_source_index = None;
            progress.active_discovered_files = 0;
        }
    }

    fn note_inventory_error(&mut self, error: String) {
        self.inventory_error = Some(error);
    }

    fn inventory_is_complete(&self) -> bool {
        self.inventory_progress
            .as_ref()
            .is_some_and(|progress| progress.next_source_index >= progress.total_sources)
    }

    fn complete_inventory(&mut self) {
        let now = Instant::now();
        let now_ms = search_refresh_now_ms();
        let completed_reason = self
            .inventory_progress
            .as_ref()
            .map(|progress| progress.reason);
        let completed_scoped = self
            .inventory_progress
            .as_ref()
            .is_some_and(|progress| progress.scoped);
        if !completed_scoped {
            self.durable_source_fingerprint = self
                .inventory_progress
                .as_ref()
                .map(|progress| progress.source_fingerprint.clone());
        }
        if self
            .source_watcher
            .as_ref()
            .is_some_and(|watcher| !watcher.is_healthy())
        {
            let error = self
                .source_watcher
                .as_ref()
                .and_then(SearchRefreshSourceWatcher::error)
                .unwrap_or_else(|| "source watcher failed during inventory".to_owned());
            self.source_watcher = None;
            self.enter_watcher_degraded(error, now, now_ms, false);
        }
        self.inventory_progress = None;
        self.inventory_error = None;
        if completed_scoped {
            return;
        }
        self.last_inventory_completed_at_ms = Some(now_ms);
        if self.watcher_degraded
            && self.source_watcher.is_some()
            && self.watcher_recovery_pending
            && completed_reason == Some(SearchInventoryReason::WatcherRecovery)
        {
            self.watcher_degraded = false;
            self.watcher_recovery_pending = false;
            self.watcher_error = None;
            self.watcher_retry_failures = 0;
            self.next_watcher_retry_at = None;
            self.next_watcher_retry_at_ms = None;
            self.degraded_inventory_passes = 0;
        }
        if self.pending_inventory_reason.is_some() {
            self.next_inventory_at = None;
            self.next_inventory_at_ms = None;
            return;
        }
        let delay = if self.watcher_degraded {
            self.degraded_inventory_passes = self.degraded_inventory_passes.saturating_add(1);
            daemon_search_refresh_retry_delay(self.degraded_inventory_passes)
        } else {
            DAEMON_SEARCH_REFRESH_REINVENTORY_INTERVAL
        };
        self.next_inventory_at = now.checked_add(delay);
        self.next_inventory_at_ms = search_refresh_timestamp_after(now_ms, delay);
    }

    #[cfg(test)]
    fn force_source_change_for_test(&self, path: &Path) {
        if let Some(watcher) = &self.source_watcher {
            watcher.mark_path_dirty(path, path);
        }
    }

    #[cfg(test)]
    fn force_source_file_change_for_test(&self, source_path: &Path, changed_path: &Path) {
        if let Some(watcher) = &self.source_watcher {
            watcher.mark_path_dirty(source_path, changed_path);
        }
    }
}

fn daemon_search_refresh_retry_delay(failure: u32) -> StdDuration {
    let index = usize::try_from(failure.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(DAEMON_SEARCH_REFRESH_RETRY_DELAYS.len().saturating_sub(1));
    DAEMON_SEARCH_REFRESH_RETRY_DELAYS[index]
}

fn search_refresh_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

fn search_refresh_timestamp_after(now_ms: i64, delay: StdDuration) -> Option<i64> {
    let delay_ms = i64::try_from(delay.as_millis()).ok()?;
    now_ms.checked_add(delay_ms)
}

fn search_refresh_future_instant(now: Instant, now_ms: i64, at_ms: i64) -> Option<Instant> {
    let remaining_ms = at_ms.saturating_sub(now_ms);
    if remaining_ms <= 0 {
        return Some(now);
    }
    now.checked_add(StdDuration::from_millis(u64::try_from(remaining_ms).ok()?))
}

impl SearchRefreshSourceWatcher {
    fn new(watched_paths: Vec<PathBuf>) -> Result<Self> {
        let changes = Arc::new(Mutex::new(SearchRefreshSourceChanges::default()));
        let healthy = Arc::new(AtomicBool::new(true));
        let error = Arc::new(Mutex::new(None));
        let callback_changes = Arc::clone(&changes);
        let callback_healthy = Arc::clone(&healthy);
        let callback_error = Arc::clone(&error);
        let watches = search_refresh_watch_specs(&watched_paths);
        let registrations = search_refresh_watch_registrations(&watches)?;
        let callback_watches = watches.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                note_search_refresh_source_event(
                    &callback_changes,
                    &callback_healthy,
                    &callback_error,
                    &callback_watches,
                    event,
                );
            },
            NotifyConfig::default(),
        )
        .context("create daemon search refresh watcher")?;
        let directory_source_count = watches.iter().filter(|watch| watch.recursive).count();
        let registered_watch_count = registrations.len();
        for (watch_path, mode) in registrations {
            ctx_history_capture::pace_current_filesystem_operation(
                watch_path.as_os_str().len() as u64
            );
            watcher
                .watch(&watch_path, mode)
                .with_context(|| format!("watch search refresh source {}", watch_path.display()))?;
        }
        Ok(Self {
            changes,
            healthy,
            error,
            registered_watch_count,
            directory_source_count,
            _watcher: watcher,
        })
    }

    fn take_changes(&self) -> SearchRefreshSourceChanges {
        let mut changes = self
            .changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *changes)
    }

    #[cfg(test)]
    fn mark_path_dirty(&self, source_path: &Path, changed_path: &Path) {
        let mut changes = self
            .changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        record_search_refresh_dirty_path(
            &mut changes,
            SearchDirtyPath {
                source_path: source_path.to_path_buf(),
                changed_path: changed_path.to_path_buf(),
            },
        );
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn note_search_refresh_source_event(
    changes: &Mutex<SearchRefreshSourceChanges>,
    healthy: &AtomicBool,
    last_error: &Mutex<Option<String>>,
    watches: &[SearchRefreshWatch],
    event: notify::Result<notify::Event>,
) {
    let event = match event {
        Ok(event) => event,
        Err(error) => {
            healthy.store(false, Ordering::Release);
            *last_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.to_string());
            changes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .full_rebuild = true;
            return;
        }
    };
    if event.need_rescan() {
        healthy.store(false, Ordering::Release);
        *last_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some("watcher requested a full filesystem rescan".to_owned());
        changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .full_rebuild = true;
        return;
    }
    if search_refresh_event_is_non_mutating_access(event.kind) {
        return;
    }

    let mut changes = changes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut unclassified = event.paths.is_empty();
    for event_path in &event.paths {
        let mut covered = false;
        for watch in watches {
            covered |= search_refresh_event_path_is_covered_by_watch(event_path, watch);
            if search_refresh_event_path_matches_watch(event_path, watch) {
                let changed_path = if watch.recursive {
                    event_path
                        .strip_prefix(&watch.match_path)
                        .map(|relative| watch.source_path.join(relative))
                        .unwrap_or_else(|_| event_path.clone())
                } else {
                    event_path.clone()
                };
                record_search_refresh_dirty_path(
                    &mut changes,
                    SearchDirtyPath {
                        source_path: watch.source_path.clone(),
                        changed_path,
                    },
                );
            }
        }
        unclassified |= !covered;
    }
    if unclassified {
        changes.full_rebuild = true;
    }
}

fn record_search_refresh_dirty_path(
    changes: &mut SearchRefreshSourceChanges,
    dirty: SearchDirtyPath,
) {
    if changes.dirty_paths.len() >= MAX_PENDING_DIRTY_PATHS && !changes.dirty_paths.contains(&dirty)
    {
        changes.dirty_paths.clear();
        changes.full_rebuild = true;
        return;
    }
    if !changes.full_rebuild {
        changes.dirty_paths.insert(dirty);
    }
}

fn search_refresh_watch_specs(paths: &[PathBuf]) -> Vec<SearchRefreshWatch> {
    paths
        .iter()
        .map(|source_path| {
            ctx_history_capture::pace_current_filesystem_operation(
                source_path.as_os_str().len() as u64
            );
            let is_dir = source_path.is_dir();
            pace_search_refresh_path_resolution(source_path);
            let match_path = fs::canonicalize(source_path).unwrap_or_else(|_| source_path.clone());
            let watch_path = if is_dir {
                match_path.clone()
            } else {
                source_path
                    .parent()
                    .map(|parent| {
                        pace_search_refresh_path_resolution(parent);
                        fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf())
                    })
                    .unwrap_or_else(|| match_path.clone())
            };
            SearchRefreshWatch {
                source_path: source_path.clone(),
                match_path,
                watch_path,
                recursive: is_dir,
            }
        })
        .collect()
}

fn search_refresh_watch_registrations(
    watches: &[SearchRefreshWatch],
) -> Result<Vec<(PathBuf, RecursiveMode)>> {
    let mut watch_targets = BTreeSet::new();
    for watch in watches {
        watch_targets.insert(watch.watch_path.clone());
    }
    if watch_targets.len() > MAX_SEARCH_REFRESH_ROOT_WATCHES {
        return Err(anyhow!(
            "search refresh has {} distinct source roots, exceeding the bounded watcher registration limit of {}; bounded periodic reconciliation remains available",
            watch_targets.len(),
            MAX_SEARCH_REFRESH_ROOT_WATCHES
        ));
    }
    Ok(watch_targets
        .into_iter()
        .map(|path| (path, RecursiveMode::NonRecursive))
        .collect())
}

fn pace_search_refresh_path_resolution(path: &Path) {
    let operations = u64::try_from(path.components().count())
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    ctx_history_capture::pace_current_filesystem_operations(
        operations,
        (path.as_os_str().len() as u64).saturating_mul(operations),
    );
}

fn search_refresh_event_path_matches_watch(event_path: &Path, watch: &SearchRefreshWatch) -> bool {
    if watch.recursive {
        return event_path == watch.match_path || event_path.starts_with(&watch.match_path);
    }
    if event_path == watch.match_path {
        return true;
    }
    let Some(event_parent) = event_path.parent() else {
        return false;
    };
    if event_parent != watch.watch_path {
        return false;
    }
    let Some(source_name) = watch.match_path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(event_name) = event_path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let companion_prefix = format!("{source_name}-");
    #[cfg(windows)]
    {
        event_name.eq_ignore_ascii_case(source_name)
            || event_name
                .to_ascii_lowercase()
                .starts_with(&companion_prefix.to_ascii_lowercase())
    }
    #[cfg(not(windows))]
    {
        event_name == source_name || event_name.starts_with(&companion_prefix)
    }
}

fn search_refresh_event_path_is_covered_by_watch(
    event_path: &Path,
    watch: &SearchRefreshWatch,
) -> bool {
    if watch.recursive {
        return event_path == watch.watch_path || event_path.starts_with(&watch.watch_path);
    }
    event_path.parent() == Some(watch.watch_path.as_path())
}

fn search_refresh_event_is_non_mutating_access(kind: notify::EventKind) -> bool {
    use notify::event::{AccessKind, AccessMode};

    matches!(
        kind,
        notify::EventKind::Access(
            AccessKind::Read
                | AccessKind::Open(_)
                | AccessKind::Close(AccessMode::Read | AccessMode::Execute)
        )
    )
}

fn watched_source_paths(sources: &[SourceInfo]) -> Vec<PathBuf> {
    let mut unique = BTreeSet::new();
    for source in sources {
        unique.insert(source.path.clone());
    }
    unique.into_iter().collect()
}

fn watched_source_path_identities(paths: &[PathBuf]) -> Vec<WatchedSourcePathIdentity> {
    paths
        .iter()
        .map(|path| {
            ctx_history_capture::pace_current_filesystem_operation(path.as_os_str().len() as u64);
            match fs::metadata(path) {
                Ok(metadata) => {
                    let stable_id = watched_source_path_stable_id(path, &metadata);
                    WatchedSourcePathIdentity {
                        exists: true,
                        is_dir: metadata.is_dir(),
                        stable_id,
                        fallback_len: stable_id.is_none().then_some(metadata.len()),
                        fallback_modified_at: stable_id
                            .is_none()
                            .then(|| metadata.modified().ok())
                            .flatten(),
                    }
                }
                Err(_) => WatchedSourcePathIdentity {
                    exists: false,
                    is_dir: false,
                    stable_id: None,
                    fallback_len: None,
                    fallback_modified_at: None,
                },
            }
        })
        .collect()
}

#[cfg(unix)]
fn watched_source_path_stable_id(_path: &Path, metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    Some((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn watched_source_path_stable_id(path: &Path, _metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    ctx_history_capture::pace_current_filesystem_operations(
        2,
        (path.as_os_str().len() as u64).saturating_mul(2),
    );
    let file = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) };
    (ok != 0).then(|| {
        let file_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
        (u64::from(info.dwVolumeSerialNumber), file_index)
    })
}

#[cfg(not(any(unix, windows)))]
fn watched_source_path_stable_id(_path: &Path, _metadata: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

pub(crate) fn search_refresh_source_fingerprint(sources: &[SourceInfo]) -> String {
    let mut items = sources
        .iter()
        .map(|source| {
            format!(
                "{}|{}|{}",
                source.provider.as_str(),
                source.source_format,
                source.path.display()
            )
        })
        .collect::<Vec<_>>();
    items.sort();
    let mut hasher = Sha256::new();
    hasher.update(items.join("\n").as_bytes());
    format!("{:x}", hasher.finalize())
}

impl std::fmt::Display for SearchRefreshFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for SearchRefreshFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

pub(crate) fn search_refresh_failure_totals(error: &anyhow::Error) -> Option<ImportTotals> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SearchRefreshFailure>())
        .map(|failure| failure.totals.clone())
}

impl SearchRefreshReport {
    pub(crate) fn skipped(mode: RefreshArg, status: &'static str) -> Self {
        Self {
            mode,
            status,
            source_count: 0,
            totals: ImportTotals::default(),
            error: None,
        }
    }

    fn completed(mode: RefreshArg, source_count: usize, totals: ImportTotals) -> Self {
        Self {
            mode,
            status: "completed",
            source_count,
            totals,
            error: None,
        }
    }

    fn failed(mode: RefreshArg, source_count: usize, totals: ImportTotals, error: String) -> Self {
        Self {
            mode,
            status: "failed",
            source_count,
            totals,
            error: Some(error),
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        compact_json(json!({
            "mode": self.mode.as_str(),
            "status": self.status,
            "source_count": self.source_count,
            "totals": import_totals_json(&self.totals),
            "error": self.error,
        }))
    }
}

pub(crate) fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    analytics_properties: &mut AnalyticsProperties,
    config: &config::AppConfig,
) -> Result<()> {
    let query_spec = search_query_from_args(&args)?;

    let db_path = database_path(data_root.clone());
    let had_existing_store = db_path.exists();
    let indexed_content_before_search = if had_existing_store {
        existing_store_indexed_content(&db_path)
    } else {
        Some(false)
    };
    analytics::insert_bool(
        analytics_properties,
        "had_existing_store_before_search",
        had_existing_store,
    );
    analytics::insert_bool(
        analytics_properties,
        "indexed_content_before_search_known",
        indexed_content_before_search.is_some(),
    );
    analytics::insert_bool(
        analytics_properties,
        "had_indexed_content_before_search",
        indexed_content_before_search.unwrap_or(false),
    );
    let refresh_started = Instant::now();
    let refresh = refresh_before_search(&args, &data_root, config.daemon.enabled)?;
    analytics::insert_duration(
        analytics_properties,
        "refresh_duration",
        refresh_started.elapsed(),
    );
    analytics::insert_str(
        analytics_properties,
        "search_refresh_mode",
        refresh.mode.as_str(),
    );
    analytics::insert_str(
        analytics_properties,
        "search_refresh_status",
        refresh.status,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "search_refresh_source_count_bucket",
        refresh.source_count as u64,
    );
    let backend_override = args.backend;
    let requested_backend = resolve_search_backend(backend_override, config)?;
    let semantic_enabled = config.semantic_search_enabled();
    if args.refresh == RefreshArg::Background
        && semantic_enabled
        && semantic::semantic_query_service_supported()
        && matches!(
            requested_backend,
            SearchBackendArg::Semantic | SearchBackendArg::Hybrid
        )
    {
        semantic::maybe_autostart_daemon_for_search(&data_root, config);
        semantic::wait_for_daemon_query_service(&data_root, StdDuration::from_secs(3));
    }
    insert_db_size_bucket(analytics_properties, &db_path);
    if args.refresh == RefreshArg::Background
        && (refresh.status == "failed" || refresh.source_count > 0)
        && !has_usable_search_fallback(indexed_content_before_search)
        && !has_usable_search_fallback(existing_store_indexed_content(&db_path))
    {
        if refresh.status == "failed" {
            return Err(anyhow!(
                "search refresh failed and no usable existing ctx index is available; run `ctx import` first or retry with `--refresh wait`: {}",
                refresh.error.as_deref().unwrap_or("unknown refresh error")
            ));
        }
        return Err(anyhow!(
            "background search refresh has not produced a usable ctx index yet; indexing remains pending, so retry with `--refresh wait`"
        ));
    }
    let store = if args.refresh == RefreshArg::Off
        || refresh.status == "failed"
        || refresh.status == "completed"
        || had_existing_store
    {
        open_existing_store_read_only(&db_path, "ctx search")?
    } else {
        Store::open(&db_path)?
    };
    analytics::insert_bool(
        analytics_properties,
        "store_created_by_search",
        !had_existing_store && db_path.exists(),
    );
    insert_store_analytics_counts(analytics_properties, &store)?;
    analytics::insert_bool(
        analytics_properties,
        "has_indexed_content_after_search",
        indexed_history_item_count(&store)? > 0,
    );
    let source_identity = SourceIdentityFilterArgs::from(&args);
    let query_text = query_spec
        .as_ref()
        .map(ctx_protocol::SearchQuery::canonical_text)
        .unwrap_or_default();
    let query_term_count = query_spec.as_ref().map_or(0, |query| {
        query
            .clauses()
            .map(|clause| clause.value().split_whitespace().count())
            .sum()
    });
    analytics::insert_text_length_bucket(
        analytics_properties,
        "query_length_bucket",
        query_text.chars().count(),
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "query_term_count_bucket",
        query_term_count as u64,
    );
    let event_results = args.events || args.session.is_some();
    let options = ctx_history_search::PacketOptions {
        limit: args.limit,
        filters: search_filters(
            SearchFilterInput {
                session: args.session,
                provider: args.provider,
                source_identity,
                workspace: args.workspace.clone(),
                since: args.since.clone(),
                primary_only: args.primary_only,
                include_subagents: args.include_subagents,
                event_type: args.event_type.clone(),
                file: args.file.clone(),
                include_current_session: args.include_current_session,
            },
            Some(&store),
        )?,
        result_mode: if event_results {
            ctx_history_search::SearchResultMode::Events
        } else {
            ctx_history_search::SearchResultMode::Sessions
        },
        ..ctx_history_search::PacketOptions::default()
    };
    let uses_composed_terms = query_spec
        .as_ref()
        .is_some_and(|query| query.single_all_text().is_none());
    let query_started = Instant::now();
    let (packet, retrieval) = if let Some(query) = query_spec.as_ref() {
        search_packet_query_with_backend(
            &store,
            &data_root,
            query,
            &options,
            requested_backend,
            semantic_enabled,
            args.refresh,
            !args.json,
        )?
    } else {
        search_packet_file_filter_with_backend(&store, &options, requested_backend, !args.json)?
    };
    analytics::insert_duration(
        analytics_properties,
        "query_duration",
        query_started.elapsed(),
    );
    analytics::insert_str(
        analytics_properties,
        "search_backend_requested",
        requested_backend.as_str(),
    );
    analytics::insert_str(
        analytics_properties,
        "search_backend_effective",
        retrieval.effective_mode().as_str(),
    );
    let result_count = packet.results.len();
    let citation_count = packet
        .results
        .iter()
        .map(|result| result.citations.len())
        .sum::<usize>();
    analytics::insert_count_bucket(
        analytics_properties,
        "result_count_bucket",
        result_count as u64,
    );
    analytics::insert_count_bucket(
        analytics_properties,
        "citation_count_bucket",
        citation_count as u64,
    );
    analytics::insert_bool(analytics_properties, "zero_result", result_count == 0);
    let render_started = Instant::now();
    if args.json {
        let suggested_next_query = query_spec
            .as_ref()
            .and_then(ctx_protocol::SearchQuery::single_all_text)
            .filter(|_| !uses_composed_terms);
        print_json(SearchDto::packet(
            &store,
            &packet,
            &refresh,
            &retrieval,
            suggested_next_query,
        )?)?;
    } else {
        if refresh.status == "failed" && args.refresh == RefreshArg::Background {
            if let Some(error) = &refresh.error {
                eprintln!(
                    "warning: search refresh failed; serving existing index; use --refresh wait to fail instead: {error}"
                );
            }
        }
        if packet.results.is_empty() {
            if let Some(file) = args.file.as_deref().filter(|_| query_spec.is_none()) {
                println!("no indexed events touched {}", file.display());
                let indexed_items = indexed_history_item_count(&store)?;
                if indexed_items == 0 {
                    println!("next: ctx import --all");
                } else {
                    println!(
                        "next: ctx search {}",
                        shell_quote_arg(&file.display().to_string())
                    );
                }
            } else {
                println!(
                    "no results for {}",
                    search_no_results_target(&query_text, &[])
                );
                let indexed_items = indexed_history_item_count(&store)?;
                if indexed_items == 0 {
                    println!("next: ctx import --all");
                } else {
                    println!("next: try broader terms with ctx search --term \"<term>\"");
                }
            }
        }
        let suggested_next_query = query_spec
            .as_ref()
            .and_then(ctx_protocol::SearchQuery::single_all_text)
            .filter(|_| !uses_composed_terms);
        for (index, result) in packet.results.iter().enumerate() {
            if args.verbose {
                print_search_result_verbose(result, suggested_next_query);
            } else {
                print_search_result_compact(index + 1, result);
            }
        }
    }
    analytics::insert_duration(
        analytics_properties,
        "render_duration",
        render_started.elapsed(),
    );
    Ok(())
}

fn has_usable_search_fallback(indexed_content: Option<bool>) -> bool {
    indexed_content == Some(true)
}

pub(crate) fn resolve_search_backend(
    backend: Option<SearchBackendArg>,
    config: &config::AppConfig,
) -> Result<SearchBackendArg> {
    let semantic_enabled = config.semantic_search_enabled();
    match backend {
        Some(SearchBackendArg::Semantic) if !semantic_enabled => Err(semantic_backend_error(
            ctx_protocol::SearchSemanticReadiness::NotReady,
            "semantic search is disabled. Set [search] semantic = true in ctx config to enable the local semantic preview",
        )),
        Some(SearchBackendArg::Semantic) if !semantic::semantic_query_service_supported() => {
            Err(semantic_backend_error(
                ctx_protocol::SearchSemanticReadiness::Unsupported,
                "local semantic search is not supported on this platform yet. Set [search] semantic = false or use --backend lexical",
            ))
        }
        Some(SearchBackendArg::Semantic) if !config.daemon.enabled => Err(semantic_backend_error(
            ctx_protocol::SearchSemanticReadiness::Unavailable,
            "local semantic search requires the ctx daemon. Set [daemon] enabled = true, set [search] semantic = false, or use --backend lexical",
        )),
        Some(value) => Ok(value),
        None if semantic_enabled => Ok(SearchBackendArg::Hybrid),
        None => Ok(SearchBackendArg::Lexical),
    }
}

fn semantic_backend_error(
    readiness: ctx_protocol::SearchSemanticReadiness,
    message: &str,
) -> anyhow::Error {
    anyhow::Error::new(ctx_history_search::SearchError::SemanticNotReady { readiness })
        .context(message.to_owned())
}

fn existing_store_indexed_content(db_path: &Path) -> Option<bool> {
    open_existing_store_read_only(db_path, "ctx search analytics preflight")
        .and_then(|store| indexed_history_item_count(&store))
        .ok()
        .map(|indexed_items| indexed_items > 0)
}
