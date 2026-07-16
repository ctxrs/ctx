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
    disk_io_pacer: Option<(ImportExecutionPolicy, DiskIoPacer)>,
}

struct SearchRefreshWork {
    source_fingerprint: String,
    publication_owner: Option<ProviderFilePublicationInventoryOwner>,
    source_change_generation: u64,
    passes_since_reinventory: usize,
    plan: ImportPlan,
    execution_state: crate::commands::import::ImportExecutionState,
    inventory_failures: Vec<ImportSourceFailure>,
    failed_inventory_pending: (usize, usize),
    inventoried_source_count: usize,
    planned_total_bytes: u64,
}

struct SearchRefreshSourceWatcher {
    generation: Arc<AtomicU64>,
    watched_paths: Vec<PathBuf>,
    _watcher: RecommendedWatcher,
}

struct SearchRefreshExecution {
    totals: ImportTotals,
}

const DAEMON_SEARCH_REFRESH_REINVENTORY_PASSES: usize = 64;

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

    fn watcher_generation(&mut self, sources: &[SourceInfo]) -> Option<u64> {
        let watched_paths = watched_source_paths(sources);
        let rebuild = self
            .source_watcher
            .as_ref()
            .is_none_or(|watcher| watcher.watched_paths != watched_paths);
        if rebuild {
            self.cached_work = None;
            self.source_watcher = SearchRefreshSourceWatcher::new(watched_paths).ok();
        }
        self.source_watcher
            .as_ref()
            .map(SearchRefreshSourceWatcher::generation)
    }

    #[cfg(test)]
    fn force_source_change_for_test(&self) {
        if let Some(watcher) = &self.source_watcher {
            watcher.generation.fetch_add(1, Ordering::AcqRel);
        }
    }
}

impl SearchRefreshSourceWatcher {
    fn new(watched_paths: Vec<PathBuf>) -> Result<Self> {
        let generation = Arc::new(AtomicU64::new(0));
        let callback_generation = Arc::clone(&generation);
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                note_search_refresh_source_event(&callback_generation, event);
            },
            NotifyConfig::default(),
        )
        .context("create daemon search refresh watcher")?;
        for path in &watched_paths {
            let mode = if path.is_dir() {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            watcher
                .watch(path, mode)
                .with_context(|| format!("watch search refresh source {}", path.display()))?;
        }
        Ok(Self {
            generation,
            watched_paths,
            _watcher: watcher,
        })
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

fn note_search_refresh_source_event(generation: &AtomicU64, _event: notify::Result<notify::Event>) {
    // A watcher error also invalidates the cache: after an error we can no
    // longer prove that no source change was missed.
    generation.fetch_add(1, Ordering::AcqRel);
}

fn watched_source_paths(sources: &[SourceInfo]) -> Vec<PathBuf> {
    let mut unique = BTreeSet::new();
    for source in sources {
        unique.insert(source.path.clone());
    }
    unique.into_iter().collect()
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
