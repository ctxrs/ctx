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

#[derive(Default)]
pub(crate) struct SearchRefreshRuntime {
    cached_work: Option<SearchRefreshWork>,
    source_watcher: Option<SearchRefreshSourceWatcher>,
    source_watcher_paths: Option<Vec<PathBuf>>,
    source_watcher_path_identities: Option<Vec<WatchedSourcePathIdentity>>,
    last_watcher_fallback_reinventory_at: Option<Instant>,
    disk_io_pacer: Option<(ImportExecutionPolicy, DiskIoPacer)>,
}

struct SearchRefreshWork {
    source_fingerprint: String,
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
    _watcher: RecommendedWatcher,
}

#[derive(Debug, Clone)]
struct SearchRefreshWatch {
    source_path: PathBuf,
    match_path: PathBuf,
    watch_path: PathBuf,
    recursive: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SearchRefreshSourceChanges {
    full_rebuild: bool,
    dirty_paths: BTreeSet<PathBuf>,
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
const DAEMON_SEARCH_REFRESH_WATCHER_FALLBACK_INTERVAL: StdDuration = StdDuration::from_secs(30);

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

    fn watcher_changes(&mut self, sources: &[SourceInfo]) -> Option<SearchRefreshSourceChanges> {
        let watched_paths = watched_source_paths(sources);
        let watched_path_identities = watched_source_path_identities(&watched_paths);
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
        if paths_changed || path_identities_changed || watcher_unhealthy {
            self.cached_work = None;
            self.source_watcher = None;
            self.source_watcher_paths = Some(watched_paths.clone());
            self.source_watcher_path_identities = Some(watched_path_identities);
        }

        let retry_due = daemon_search_refresh_watcher_retry_due(self.source_watcher.is_some());
        if paths_changed || path_identities_changed || watcher_unhealthy || retry_due {
            if let Ok(watcher) = SearchRefreshSourceWatcher::new(watched_paths) {
                // A recovered watcher cannot account for changes made while it
                // was unavailable, so rebuild once before trusting it.
                if !paths_changed {
                    self.cached_work = None;
                }
                self.source_watcher = Some(watcher);
            }
        }
        if self.source_watcher.is_some() {
            self.last_watcher_fallback_reinventory_at = None;
        } else if daemon_search_refresh_watcher_fallback_due(
            self.last_watcher_fallback_reinventory_at,
        ) {
            self.cached_work = None;
            self.last_watcher_fallback_reinventory_at = Some(Instant::now());
        }
        self.source_watcher
            .as_ref()
            .map(SearchRefreshSourceWatcher::take_changes)
    }

    #[cfg(test)]
    fn force_source_change_for_test(&self, path: &Path) {
        if let Some(watcher) = &self.source_watcher {
            watcher.mark_path_dirty(path);
        }
    }
}

fn daemon_search_refresh_watcher_retry_due(watcher_available: bool) -> bool {
    !watcher_available
}

fn daemon_search_refresh_watcher_fallback_due(last_reinventory_at: Option<Instant>) -> bool {
    last_reinventory_at
        .is_none_or(|last| last.elapsed() >= DAEMON_SEARCH_REFRESH_WATCHER_FALLBACK_INTERVAL)
}

impl SearchRefreshSourceWatcher {
    fn new(watched_paths: Vec<PathBuf>) -> Result<Self> {
        let changes = Arc::new(Mutex::new(SearchRefreshSourceChanges::default()));
        let healthy = Arc::new(AtomicBool::new(true));
        let callback_changes = Arc::clone(&changes);
        let callback_healthy = Arc::clone(&healthy);
        let watches = search_refresh_watch_specs(&watched_paths);
        let callback_watches = watches.clone();
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                note_search_refresh_source_event(
                    &callback_changes,
                    &callback_healthy,
                    &callback_watches,
                    event,
                );
            },
            NotifyConfig::default(),
        )
        .context("create daemon search refresh watcher")?;
        let mut watch_targets = Vec::<(PathBuf, bool)>::new();
        for watch in &watches {
            if let Some((_, recursive)) = watch_targets
                .iter_mut()
                .find(|(path, _)| *path == watch.watch_path)
            {
                *recursive |= watch.recursive;
            } else {
                watch_targets.push((watch.watch_path.clone(), watch.recursive));
            }
        }
        for (watch_path, recursive) in watch_targets {
            let mode = if recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            watcher
                .watch(&watch_path, mode)
                .with_context(|| format!("watch search refresh source {}", watch_path.display()))?;
        }
        Ok(Self {
            changes,
            healthy,
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

    fn mark_path_dirty(&self, path: &Path) {
        self.changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .dirty_paths
            .insert(path.to_path_buf());
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }
}

fn note_search_refresh_source_event(
    changes: &Mutex<SearchRefreshSourceChanges>,
    healthy: &AtomicBool,
    watches: &[SearchRefreshWatch],
    event: notify::Result<notify::Event>,
) {
    let Ok(event) = event else {
        healthy.store(false, Ordering::Release);
        changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .full_rebuild = true;
        return;
    };
    if event.need_rescan() {
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
                changes.dirty_paths.insert(watch.source_path.clone());
            }
        }
        unclassified |= !covered;
    }
    if unclassified {
        changes.full_rebuild = true;
    }
}

fn search_refresh_watch_specs(paths: &[PathBuf]) -> Vec<SearchRefreshWatch> {
    paths
        .iter()
        .map(|source_path| {
            let is_dir = source_path.is_dir();
            let match_path = fs::canonicalize(source_path).unwrap_or_else(|_| source_path.clone());
            let watch_path = if is_dir {
                match_path.clone()
            } else {
                source_path
                    .parent()
                    .map(|parent| fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf()))
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
        .map(|path| match fs::metadata(path) {
            Ok(metadata) => {
                let stable_id = watched_source_path_stable_id(path, &metadata);
                WatchedSourcePathIdentity {
                    exists: true,
                    is_dir: metadata.is_dir(),
                    stable_id,
                    fallback_len: stable_id.is_none().then(|| metadata.len()),
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
    if !search_has_intent(SearchIntentInput {
        query: args.query.as_deref(),
        terms: &args.term,
        file: args.file.as_deref(),
    }) {
        return Err(missing_search_intent_error());
    }

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
    let query = args.query.unwrap_or_default();
    let query_term_count = query
        .split_whitespace()
        .filter(|term| !term.trim().is_empty())
        .count()
        .saturating_add(
            args.term
                .iter()
                .filter(|term| !term.trim().is_empty())
                .count(),
        );
    analytics::insert_text_length_bucket(
        analytics_properties,
        "query_length_bucket",
        query.chars().count(),
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
    let uses_composed_terms = args.term.iter().any(|term| !term.trim().is_empty());
    let query_started = Instant::now();
    let (packet, retrieval) = search_packet_with_backend(
        &store,
        &data_root,
        &query,
        &args.term,
        &options,
        requested_backend,
        semantic_enabled,
        args.semantic_weight,
        args.refresh,
        !args.json,
    )?;
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
        let suggested_next_query = (!uses_composed_terms).then_some(query.as_str());
        print_json(SearchDto::packet(
            &store,
            &packet,
            &refresh,
            &retrieval,
            suggested_next_query,
        ))?;
    } else {
        if refresh.status == "failed" && args.refresh == RefreshArg::Background {
            if let Some(error) = &refresh.error {
                eprintln!(
                    "warning: search refresh failed; serving existing index; use --refresh wait to fail instead: {error}"
                );
            }
        }
        if packet.results.is_empty() {
            if let Some(file) = args
                .file
                .as_deref()
                .filter(|_| query.trim().is_empty() && !uses_composed_terms)
            {
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
                    search_no_results_target(&query, &args.term)
                );
                let indexed_items = indexed_history_item_count(&store)?;
                if indexed_items == 0 {
                    println!("next: ctx import --all");
                } else {
                    println!("next: try broader terms with ctx search --term \"<term>\"");
                }
            }
        }
        let suggested_next_query = (!uses_composed_terms).then_some(query.as_str());
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
        Some(SearchBackendArg::Semantic) if !semantic_enabled => Err(anyhow!(
            "semantic search is disabled. Set [search] semantic = true in ctx config to enable the local semantic preview"
        )),
        Some(SearchBackendArg::Semantic) if !semantic::semantic_query_service_supported() => Err(
            anyhow!(
                "local semantic search is not supported on this platform yet. Set [search] semantic = false or use --backend lexical"
            ),
        ),
        Some(SearchBackendArg::Semantic) if !config.daemon.enabled => Err(anyhow!(
            "local semantic search requires the ctx daemon. Set [daemon] enabled = true, set [search] semantic = false, or use --backend lexical"
        )),
        value
            if semantic_enabled
                && semantic::semantic_query_service_supported()
                && !config.daemon.enabled
                && !matches!(value, Some(SearchBackendArg::Lexical)) =>
        {
            Err(anyhow!(
                "local semantic search requires the ctx daemon. Set [daemon] enabled = true, set [search] semantic = false, or use --backend lexical"
            ))
        }
        Some(value) => Ok(value),
        None if semantic_enabled => Ok(SearchBackendArg::Hybrid),
        None => Ok(SearchBackendArg::Lexical),
    }
}

fn existing_store_indexed_content(db_path: &Path) -> Option<bool> {
    open_existing_store_read_only(db_path, "ctx search analytics preflight")
        .and_then(|store| indexed_history_item_count(&store))
        .ok()
        .map(|indexed_items| indexed_items > 0)
}
