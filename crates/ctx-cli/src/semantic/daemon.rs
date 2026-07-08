#[derive(Debug)]
struct DaemonIteration {
    did_work: bool,
    failed: bool,
}

#[derive(Default)]
struct DaemonRuntime {
    semantic_embedder: Arc<Mutex<Option<SemanticEmbedder>>>,
    semantic_bootstrap_passes_since_refresh: usize,
}

fn daemon_runtime_embedder_loaded(runtime: &DaemonRuntime) -> bool {
    runtime
        .semantic_embedder
        .lock()
        .map(|embedder| embedder.is_some())
        .unwrap_or(false)
}

fn lock_daemon_runtime_embedder(
    runtime: &DaemonRuntime,
) -> Result<std::sync::MutexGuard<'_, Option<SemanticEmbedder>>> {
    runtime
        .semantic_embedder
        .lock()
        .map_err(|_| anyhow!("semantic embedder lock is poisoned"))
}

#[cfg(unix)]
struct DaemonQueryService {
    path: PathBuf,
}

#[cfg(unix)]
impl Drop for DaemonQueryService {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn start_daemon_query_service(
    data_root: &Path,
    embedder: Arc<Mutex<Option<SemanticEmbedder>>>,
) -> Result<DaemonQueryService> {
    let root = daemon_root_path(data_root);
    create_private_dir_all(&root)?;
    let path = daemon_query_socket_path(data_root);
    let _ = fs::remove_file(&path);
    let listener =
        UnixListener::bind(&path).with_context(|| format!("bind daemon query socket {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set daemon query socket permissions {}", path.display()))?;
    let thread_data_root = data_root.to_path_buf();
    std::thread::Builder::new()
        .name("ctx-daemon-query".to_owned())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => handle_daemon_query_stream(&thread_data_root, &embedder, stream),
                    Err(_) => break,
                }
            }
        })
        .context("start daemon query service thread")?;
    Ok(DaemonQueryService { path })
}

#[cfg(unix)]
fn handle_daemon_query_stream(
    data_root: &Path,
    embedder: &Arc<Mutex<Option<SemanticEmbedder>>>,
    stream: UnixStream,
) {
    let mut stream = stream;
    let result = handle_daemon_query_stream_inner(data_root, embedder, &mut stream);
    if let Err(error) = result {
        let _ = writeln!(
            stream,
            "{}",
            serde_json::to_string(&compact_json(json!({
                "ok": false,
                "error": format!("{error:#}"),
            })))
            .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"query failed\"}".to_owned())
        );
    }
}

#[cfg(unix)]
fn handle_daemon_query_stream_inner(
    data_root: &Path,
    embedder: &Arc<Mutex<Option<SemanticEmbedder>>>,
    stream: &mut UnixStream,
) -> Result<()> {
    let mut body = String::new();
    stream
        .take(256 * 1024)
        .read_to_string(&mut body)
        .context("read daemon query request")?;
    let request: Value = serde_json::from_str(&body).context("parse daemon query request")?;
    let op = request.get("op").and_then(Value::as_str).unwrap_or("");
    if op == "ping" {
        writeln!(
            stream,
            "{}",
            serde_json::to_string(&compact_json(json!({
                "ok": true,
                "schema_version": 1,
            })))?
        )?;
        return Ok(());
    }
    if op != "embed_query" {
        return Err(anyhow!("unknown daemon query operation `{op}`"));
    }
    let model_key = request.get("model_key").and_then(Value::as_str).unwrap_or("");
    if model_key != SEMANTIC_MODEL_KEY {
        return Err(anyhow!("daemon query model key mismatch"));
    }
    let text = request
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return Err(anyhow!("daemon query text is empty"));
    }
    let started = Instant::now();
    let mut guard = embedder
        .lock()
        .map_err(|_| anyhow!("semantic embedder lock is poisoned"))?;
    if guard.is_none() {
        let cache_dir = semantic_worker_cache_dir(data_root);
        if !semantic_model_cache_available(&cache_dir) {
            return Err(anyhow!("semantic model cache is not available to daemon query service"));
        }
        *guard = Some(new_semantic_embedder(&cache_dir)?);
    }
    let embedder = guard
        .as_mut()
        .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?;
    let mut embeddings = embed_texts(embedder, vec![text.to_owned()])?;
    let embedding = embeddings
        .pop()
        .ok_or_else(|| anyhow!("semantic query embedding was empty"))?;
    let query_embed_ms = started.elapsed().as_millis() as u64;
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&compact_json(json!({
            "ok": true,
            "model_key": SEMANTIC_MODEL_KEY,
            "query_embed_ms": query_embed_ms,
            "embedding": embedding,
        })))?
    )?;
    Ok(())
}

#[cfg(test)]
#[derive(Clone)]
struct DaemonTestJobHooks {
    calls: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    history_refresh: Option<Value>,
    semantic_index: Option<Value>,
}

#[cfg(test)]
thread_local! {
    static DAEMON_TEST_JOB_HOOKS: std::cell::RefCell<Option<DaemonTestJobHooks>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
struct DaemonTestJobHookGuard;

#[cfg(test)]
impl Drop for DaemonTestJobHookGuard {
    fn drop(&mut self) {
        DAEMON_TEST_JOB_HOOKS.with(|hooks| {
            *hooks.borrow_mut() = None;
        });
    }
}

#[cfg(test)]
fn install_daemon_test_job_hooks(hooks: DaemonTestJobHooks) -> DaemonTestJobHookGuard {
    DAEMON_TEST_JOB_HOOKS.with(|slot| {
        assert!(slot.borrow().is_none(), "daemon test job hook already installed");
        *slot.borrow_mut() = Some(hooks);
    });
    DaemonTestJobHookGuard
}

#[cfg(test)]
fn daemon_test_job(job: &'static str) -> Option<Value> {
    DAEMON_TEST_JOB_HOOKS.with(|slot| {
        let hooks = slot.borrow();
        let hooks = hooks.as_ref()?;
        hooks.calls.borrow_mut().push(job);
        match job {
            "history_refresh" => hooks.history_refresh.clone(),
            "semantic_index" => hooks.semantic_index.clone(),
            _ => None,
        }
    })
}

#[derive(Debug, Clone)]
struct SemanticWorkerArgs {
    max_chunks: Option<usize>,
    max_seconds: Option<u64>,
}

pub(crate) fn run_daemon_command(
    args: DaemonArgs,
    data_root: PathBuf,
    config: &AppConfig,
) -> Result<()> {
    match args.command {
        DaemonCommand::Run(args) => run_daemon(args, data_root, config),
        DaemonCommand::Status(args) => run_daemon_status(args, data_root),
        DaemonCommand::Enable(args) => run_daemon_enabled_update(args, data_root, true),
        DaemonCommand::Disable(args) => run_daemon_enabled_update(args, data_root, false),
    }
}

fn run_daemon_status(args: JsonArgs, data_root: PathBuf) -> Result<()> {
    let semantic_report = semantic_worker_report_for_daemon(&data_root);
    let daemon = daemon_report(&data_root, &semantic_report);
    if args.json {
        print_json(json!({
            "schema_version": 1,
            "daemon": daemon,
            "local_only": true,
        }))?;
    } else {
        print_daemon_status_human(&daemon);
    }
    Ok(())
}

fn run_daemon_enabled_update(args: JsonArgs, data_root: PathBuf, enabled: bool) -> Result<()> {
    config::set_daemon_enabled(&data_root, enabled)?;
    if args.json {
        print_json(json!({
            "schema_version": 1,
            "daemon_enabled": enabled,
            "config_path": data_root.join(CONFIG_FILE),
            "local_only": true,
        }))?;
    } else {
        println!("daemon_enabled: {enabled}");
        println!("config_path: {}", data_root.join(CONFIG_FILE).display());
    }
    Ok(())
}

fn print_daemon_status_human(daemon: &Value) {
    println!(
        "daemon_enabled: {}",
        daemon
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    );
    println!(
        "daemon_status: {}",
        daemon
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "daemon_running: {}",
        daemon
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    if let Some(reason) = daemon.get("reason").and_then(Value::as_str) {
        println!("daemon_reason: {reason}");
    }
    if daemon
        .get("recoverable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        println!("daemon_recoverable: true");
    }
    println!(
        "history_refresh_status: {}",
        daemon
            .get("jobs")
            .and_then(|jobs| jobs.get("history_refresh"))
            .and_then(|job| job.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "semantic_index_status: {}",
        daemon
            .get("jobs")
            .and_then(|jobs| jobs.get("semantic_index"))
            .and_then(|job| job.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "cloud_sync_status: {}",
        daemon
            .get("jobs")
            .and_then(|jobs| jobs.get("cloud_sync"))
            .and_then(|cloud| cloud.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("disabled")
    );
}

fn run_daemon(args: DaemonRunArgs, data_root: PathBuf, config: &AppConfig) -> Result<()> {
    if (args.start_mode.is_some() || args.trigger_command.is_some())
        && !semantic_env_flag(DAEMON_BACKGROUND_CHILD_ENV)
    {
        return Err(anyhow!(
            "daemon autostart metadata flags are internal; run `ctx daemon run` without --start-mode or --trigger-command"
        ));
    }
    if config.semantic_search_enabled() && !semantic_query_service_supported() {
        return Err(anyhow!(
            "local semantic search is not supported on this platform yet. Set [search] semantic = false"
        ));
    }
    lower_semantic_worker_priority();
    let report = match run_daemon_inner(
        args.clone(),
        &data_root,
        config.daemon.enabled,
        config.semantic_search_enabled(),
    ) {
        Ok(report) => report,
        Err(error) => {
            let message = format!("{error:#}");
            let now = utc_now().timestamp_millis();
            let _ = write_daemon_status(
                &data_root,
                &json!({
                    "schema_version": 1,
                    "status": "failed",
                    "pid": process::id(),
                    "heartbeat_at_ms": now,
                    "finished_at_ms": now,
                    "start_mode": daemon_run_start_mode(&args).as_str(),
                    "trigger_command": args.trigger_command.map(DaemonTriggerCommandArg::as_str),
                    "last_error": message,
                }),
            );
            return Err(error);
        }
    };
    if args.json {
        print_json(report)?;
    } else {
        print_daemon_status_human(&report);
    }
    Ok(())
}

fn run_daemon_inner(
    args: DaemonRunArgs,
    data_root: &Path,
    daemon_enabled: bool,
    semantic_enabled: bool,
) -> Result<Value> {
    if !daemon_enabled && !args.force {
        let semantic_report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_report(data_root, &semantic_report));
    }
    let Some(_lock) = DaemonLock::acquire(data_root)? else {
        let semantic_report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_report(data_root, &semantic_report));
    };

    let run_once = args.once;
    let idle_exit = StdDuration::from_secs(
        args.idle_exit_seconds
            .unwrap_or(DAEMON_IDLE_EXIT_SECONDS_DEFAULT),
    );
    let loop_interval = StdDuration::from_secs(
        args.loop_interval_seconds
            .unwrap_or(DAEMON_LOOP_INTERVAL_SECONDS_DEFAULT),
    );
    let started_at_ms = utc_now().timestamp_millis();
    let mut failed = false;
    write_daemon_lifecycle_status(data_root, &args, "running", started_at_ms, None, None)?;

    let mut runtime = DaemonRuntime::default();
    #[cfg(unix)]
    let _query_service = if semantic_enabled {
        Some(start_daemon_query_service(data_root, runtime.semantic_embedder.clone())?)
    } else {
        None
    };
    let mut idle_since: Option<Instant> = None;
    loop {
        if idle_since.is_some_and(|idle| idle.elapsed() >= idle_exit) {
            break;
        }
        let iteration = run_daemon_once(&args, data_root, &mut runtime, None, semantic_enabled)?;
        write_daemon_lifecycle_status(data_root, &args, "running", started_at_ms, None, None)?;
        if iteration.failed {
            failed = true;
            break;
        }
        if run_once {
            break;
        }
        if iteration.did_work {
            idle_since = None;
        } else if idle_since.is_none() {
            idle_since = Some(Instant::now());
        }
        std::thread::sleep(loop_interval);
    }

    write_daemon_lifecycle_status(
        data_root,
        &args,
        if failed { "failed" } else { "completed" },
        started_at_ms,
        Some(utc_now().timestamp_millis()),
        failed.then_some("one or more daemon jobs failed".to_owned()),
    )?;
    drop(_lock);
    let semantic_report = semantic_worker_report_for_daemon(data_root);
    Ok(daemon_report_with_disabled_status(
        data_root,
        &semantic_report,
        !args.force,
    ))
}

fn run_daemon_once(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
) -> Result<DaemonIteration> {
    if semantic_enabled && semantic_bootstrap_should_run_first(data_root, runtime)? {
        let history_refresh_job =
            daemon_history_refresh_skipped_job("semantic_bootstrap_in_progress");
        write_daemon_job_status(&daemon_history_refresh_job_path(data_root), &history_refresh_job)?;
        let semantic_job = run_daemon_semantic_job(args, data_root, runtime, deadline, semantic_enabled)
            .unwrap_or_else(|error| daemon_semantic_failed_job(data_root, format!("{error:#}")));
        let semantic_did_work = daemon_semantic_job_did_work(&semantic_job);
        runtime.semantic_bootstrap_passes_since_refresh =
            runtime.semantic_bootstrap_passes_since_refresh.saturating_add(1);
        write_daemon_job_status_unless_deadline_skip(
            &daemon_semantic_job_path(data_root),
            &semantic_job,
        )?;
        let cloud_sync_job = daemon_cloud_sync_disabled_job(Some(utc_now().timestamp_millis()));
        write_daemon_job_status(&daemon_cloud_sync_job_path(data_root), &cloud_sync_job)?;
        return Ok(DaemonIteration {
            did_work: semantic_did_work,
            failed: daemon_job_failed(&semantic_job),
        });
    }

    let history_refresh_job =
        if daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
            run_daemon_history_refresh_job(data_root)
        } else {
            Ok(daemon_history_refresh_skipped_job("daemon_deadline"))
        };
    let history_refresh_job = match history_refresh_job {
        Ok(value) => value,
        Err(error) => daemon_history_refresh_failed_job(format!("{error:#}")),
    };
    let history_refresh_did_work = daemon_history_refresh_job_did_work(&history_refresh_job);
    runtime.semantic_bootstrap_passes_since_refresh = 0;
    write_daemon_job_status_unless_deadline_skip(
        &daemon_history_refresh_job_path(data_root),
        &history_refresh_job,
    )?;

    let semantic_job = if daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
        run_daemon_semantic_job(args, data_root, runtime, deadline, semantic_enabled)
    } else {
        Ok(daemon_semantic_deadline_skipped_job(data_root))
    };
    let semantic_job = match semantic_job {
        Ok(value) => value,
        Err(error) => daemon_semantic_failed_job(data_root, format!("{error:#}")),
    };
    let semantic_did_work = daemon_semantic_job_did_work(&semantic_job);
    write_daemon_job_status_unless_deadline_skip(
        &daemon_semantic_job_path(data_root),
        &semantic_job,
    )?;

    let cloud_sync_job = daemon_cloud_sync_disabled_job(Some(utc_now().timestamp_millis()));
    write_daemon_job_status(&daemon_cloud_sync_job_path(data_root), &cloud_sync_job)?;

    Ok(DaemonIteration {
        did_work: history_refresh_did_work || semantic_did_work,
        failed: daemon_job_failed(&history_refresh_job) || daemon_job_failed(&semantic_job),
    })
}

fn semantic_bootstrap_should_run_first(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
) -> Result<bool> {
    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        return Ok(false);
    }
    if runtime.semantic_bootstrap_passes_since_refresh
        >= DAEMON_SEMANTIC_BOOTSTRAP_PASSES_BEFORE_REFRESH
    {
        return Ok(false);
    }
    let store = Store::open(&db_path).context("open ctx store for daemon semantic bootstrap")?;
    refresh_semantic_document_count_cache(&store)?;
    let report = semantic_worker_report(data_root, Some(&store))?;
    Ok(report.searchable_items > 0
        && report.queued_items_estimate > 0
        && report.model_cache_available)
}

fn semantic_report_should_queue_recent_work(report: &SemanticWorkerReport) -> bool {
    report.searchable_items > 0
        && report.embedded_items >= report.searchable_items
        && report.dirty_items == 0
}

fn refresh_semantic_document_count_cache(store: &Store) -> Result<()> {
    store.refresh_event_embedding_document_count_cache()?;
    Ok(())
}

fn daemon_semantic_job_did_work(value: &Value) -> bool {
    value
        .get("indexed_chunks")
        .and_then(Value::as_u64)
        .is_some_and(|chunks| chunks > 0)
}

fn daemon_run_start_mode(args: &DaemonRunArgs) -> DaemonStartModeArg {
    args.start_mode.unwrap_or(DaemonStartModeArg::Manual)
}

fn daemon_job_failed(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("failed")
}

fn write_daemon_job_status_unless_deadline_skip(path: &Path, value: &Value) -> Result<()> {
    if daemon_job_skipped_for_deadline(value) && path.exists() {
        return Ok(());
    }
    write_daemon_job_status(path, value)
}

fn daemon_job_skipped_for_deadline(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("skipped")
        && value.get("reason").and_then(Value::as_str) == Some("daemon_deadline")
}

fn daemon_deadline_remaining(deadline: Option<Instant>) -> Option<StdDuration> {
    deadline.and_then(|deadline| deadline.checked_duration_since(Instant::now()))
}

fn daemon_deadline_has_min_budget(deadline: Option<Instant>, min_secs: u64) -> bool {
    let Some(remaining) = daemon_deadline_remaining(deadline) else {
        return deadline.is_none();
    };
    remaining >= StdDuration::from_secs(min_secs)
}

fn run_daemon_history_refresh_job(data_root: &Path) -> Result<Value> {
    #[cfg(test)]
    if let Some(value) = daemon_test_job("history_refresh") {
        return Ok(value);
    }

    let last_run_at_ms = utc_now().timestamp_millis();
    let sources = search_refresh_sources(None);
    let plugin_sources = search_refresh_plugin_sources(
        data_root,
        None,
        &crate::search_filters::SourceIdentityFilters::default(),
    )?;
    let source_count = sources.len().saturating_add(plugin_sources.len());
    if source_count == 0 {
        return Ok(daemon_history_refresh_job_json(
            "skipped",
            0,
            ImportTotals::default(),
            last_run_at_ms,
            Some("no_sources"),
            None,
        ));
    }
    let source_fingerprint = search_refresh_source_fingerprint(&sources);
    let mut job = match refresh_sources_for_search(
        data_root,
        sources,
        plugin_sources,
        RefreshArg::Background,
        true,
    ) {
        Ok(totals) => daemon_history_refresh_job_json(
            "completed",
            source_count,
            totals,
            last_run_at_ms,
            None,
            None,
        ),
        Err(error) => daemon_history_refresh_job_json(
            "failed",
            source_count,
            ImportTotals::default(),
            last_run_at_ms,
            None,
            Some(error_summary(&error)),
        ),
    };
    if let Some(map) = job.as_object_mut() {
        map.insert("source_fingerprint".to_owned(), json!(source_fingerprint));
        map.insert("passes".to_owned(), json!(1));
    }
    Ok(job)
}

fn daemon_history_refresh_skipped_job(reason: &str) -> Value {
    daemon_history_refresh_job_json(
        "skipped",
        0,
        ImportTotals::default(),
        utc_now().timestamp_millis(),
        Some(reason),
        None,
    )
}

fn daemon_history_refresh_failed_job(message: String) -> Value {
    daemon_history_refresh_job_json(
        "failed",
        0,
        ImportTotals::default(),
        utc_now().timestamp_millis(),
        None,
        Some(message),
    )
}

fn daemon_history_refresh_job_json(
    status: &str,
    source_count: usize,
    totals: ImportTotals,
    last_run_at_ms: i64,
    reason: Option<&str>,
    last_error: Option<String>,
) -> Value {
    compact_json(json!({
        "mode": RefreshArg::Background.as_str(),
        "status": status,
        "source_count": source_count,
        "totals": import_totals_json(&totals),
        "reason": reason,
        "last_run_at_ms": last_run_at_ms,
        "last_error": last_error,
    }))
}

fn daemon_history_refresh_job_did_work(value: &Value) -> bool {
    let Some(totals) = value.get("totals") else {
        return false;
    };
    ["imported_sessions", "imported_events", "imported_edges"]
        .into_iter()
        .any(|key| totals.get(key).and_then(Value::as_u64).unwrap_or(0) > 0)
}

fn search_refresh_source_fingerprint(sources: &[crate::provider_sources::SourceInfo]) -> String {
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
    semantic_text_hash(&items.join("\n"))
}

fn run_daemon_semantic_job(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
) -> Result<Value> {
    let last_run_at_ms = utc_now().timestamp_millis();
    if !semantic_enabled {
        let report = semantic_worker_report_best_effort(data_root);
        return Ok(daemon_semantic_job_json(
            "disabled",
            Some("semantic_disabled"),
            last_run_at_ms,
            &report,
            None,
            None,
        ));
    }

    #[cfg(test)]
    if let Some(value) = daemon_test_job("semantic_index") {
        return Ok(value);
    }

    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        let report = semantic_worker_report_best_effort(data_root);
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("store_missing"),
            last_run_at_ms,
            &report,
            None,
            None,
        ));
    }

    let store = Store::open(&db_path).context("open ctx store for daemon semantic job")?;
    refresh_semantic_document_count_cache(&store)?;
    let mut before = semantic_worker_report(data_root, Some(&store))?;
    if semantic_report_should_queue_recent_work(&before)
        && queue_recent_semantic_work(data_root, &store, "daemon_recent").unwrap_or(0) > 0
    {
        before = semantic_worker_report(data_root, Some(&store))?;
    }
    if before.searchable_items == 0 {
        return Ok(daemon_semantic_job_json(
            "empty",
            Some("no_searchable_items"),
            last_run_at_ms,
            &before,
            None,
            None,
        ));
    }
    if before.queued_items_estimate == 0 {
        return Ok(daemon_semantic_job_json(
            "ready",
            None,
            last_run_at_ms,
            &before,
            None,
            None,
        ));
    }
    let min_remaining_secs = if daemon_runtime_embedder_loaded(runtime) {
        DAEMON_MIN_REMAINING_FOR_JOB_SECS
    } else {
        SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS
    }
    .saturating_add(DAEMON_SEMANTIC_RESERVE_GRACE_SECS);
    if !daemon_deadline_has_min_budget(deadline, min_remaining_secs) {
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("daemon_deadline"),
            last_run_at_ms,
            &before,
            None,
            None,
        ));
    }
    if !before.model_cache_available && !daemon_runtime_embedder_loaded(runtime) {
        let cache_dir = semantic_worker_cache_dir(data_root);
        let _ = write_semantic_model_acquisition_status(data_root, "acquiring_model", None);
        match acquire_semantic_embedder(&cache_dir) {
            Ok(embedder) => {
                *lock_daemon_runtime_embedder(runtime)? = Some(embedder);
            }
            Err(error) => {
                let message = format!("{error:#}");
                let _ = write_semantic_model_acquisition_status(
                    data_root,
                    "model_acquisition_failed",
                    Some(message.clone()),
                );
                return Ok(daemon_semantic_job_json(
                    "skipped",
                    Some("model_acquisition_failed"),
                    last_run_at_ms,
                    &before,
                    None,
                    Some(message),
                ));
            }
        }
    }
    drop(store);

    let worker_max_seconds = daemon_semantic_worker_seconds_budget(args, deadline);
    if worker_max_seconds == 0 {
        let report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_semantic_job_json(
            "skipped",
            Some("daemon_deadline"),
            last_run_at_ms,
            &report,
            None,
            None,
        ));
    }
    let worker_args = SemanticWorkerArgs {
        max_chunks: args.max_chunks,
        max_seconds: Some(worker_max_seconds),
    };
    let worker_result = {
        let mut embedder = lock_daemon_runtime_embedder(runtime)?;
        run_semantic_worker_inner_with_embedder(worker_args, data_root, None, &mut embedder)
    };
    if let Err(error) = worker_result {
        let message = format!("{error:#}");
        let _ = write_semantic_worker_failure_status(data_root, message.clone());
        let report = semantic_worker_report_for_daemon(data_root);
        return Ok(daemon_semantic_job_json(
            "failed",
            None,
            last_run_at_ms,
            &report,
            None,
            Some(message),
        ));
    }
    let report = semantic_worker_report_for_daemon(data_root);
    let indexed_chunks_now = report
        .embedded_chunks
        .saturating_sub(before.embedded_chunks);
    let indexed_chunks = (indexed_chunks_now > 0).then_some(indexed_chunks_now);
    let status = if report.running {
        "running"
    } else if report.queued_items_estimate == 0 {
        "ready"
    } else if indexed_chunks_now > 0 {
        "budget_exhausted"
    } else {
        report.status.as_str()
    };
    Ok(daemon_semantic_job_json(
        status,
        None,
        last_run_at_ms,
        &report,
        indexed_chunks,
        None,
    ))
}

fn daemon_semantic_requested_seconds(args: &DaemonRunArgs) -> u64 {
    semantic_worker_seconds_budget(&SemanticWorkerArgs {
        max_chunks: args.max_chunks,
        max_seconds: args.max_seconds,
    })
}

fn daemon_semantic_worker_seconds_budget(args: &DaemonRunArgs, deadline: Option<Instant>) -> u64 {
    let requested = daemon_semantic_requested_seconds(args);
    let Some(remaining) = daemon_deadline_remaining(deadline) else {
        return if deadline.is_none() { requested } else { 0 };
    };
    let remaining_secs = remaining
        .as_secs()
        .saturating_sub(DAEMON_SEMANTIC_RESERVE_GRACE_SECS);
    requested.min(remaining_secs)
}

fn daemon_semantic_deadline_skipped_job(data_root: &Path) -> Value {
    let report = semantic_worker_report_for_daemon(data_root);
    daemon_semantic_job_json(
        "skipped",
        Some("daemon_deadline"),
        utc_now().timestamp_millis(),
        &report,
        None,
        None,
    )
}

fn daemon_semantic_failed_job(data_root: &Path, message: String) -> Value {
    let report = semantic_worker_report_for_daemon(data_root);
    daemon_semantic_job_json(
        "failed",
        None,
        utc_now().timestamp_millis(),
        &report,
        None,
        Some(message),
    )
}

fn daemon_semantic_job_json(
    status: &str,
    reason: Option<&str>,
    last_run_at_ms: i64,
    report: &SemanticWorkerReport,
    indexed_chunks: Option<usize>,
    last_error: Option<String>,
) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "status": status,
        "model_key": SEMANTIC_MODEL_KEY,
        "enabled": true,
        "reason": reason,
        "last_run_at_ms": last_run_at_ms,
        "last_error": last_error,
        "indexed_chunks": indexed_chunks,
        "model_cache_available": report.model_cache_available,
        "embed_policy": report.embed_policy.clone(),
        "worker_status": report.status,
        "coverage": {
            "searchable_items": report.searchable_items,
            "completed_items": report.embedded_items,
            "embedded_items": report.embedded_items,
            "embedded_chunks": report.embedded_chunks,
            "dirty_items": report.dirty_items,
            "queued_items_estimate": report.queued_items_estimate,
        },
    }))
}

fn daemon_cloud_sync_disabled_job(last_run_at_ms: Option<i64>) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "status": "disabled",
        "enabled": false,
        "reason": "not_configured",
        "network_allowed": false,
        "last_run_at_ms": last_run_at_ms,
        "last_upload_at_ms": Value::Null,
        "queued_items_estimate": 0,
        "last_error": Value::Null,
    }))
}

fn write_daemon_lifecycle_status(
    data_root: &Path,
    args: &DaemonRunArgs,
    status: &str,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    last_error: Option<String>,
) -> Result<()> {
    write_daemon_status(
        data_root,
        &compact_json(json!({
            "schema_version": 1,
            "status": status,
            "pid": process::id(),
            "started_at_ms": started_at_ms,
            "heartbeat_at_ms": utc_now().timestamp_millis(),
            "finished_at_ms": finished_at_ms,
            "start_mode": daemon_run_start_mode(args).as_str(),
            "trigger_command": args.trigger_command.map(DaemonTriggerCommandArg::as_str),
            "last_error": last_error,
        })),
    )
}

fn semantic_worker_report_for_daemon(data_root: &Path) -> SemanticWorkerReport {
    let db_path = database_path(data_root.to_path_buf());
    if db_path.exists() {
        match open_existing_store_snapshot_read_only(&db_path, "ctx daemon status") {
            Ok(store) => {
                return semantic_worker_report_cached(data_root, Some(&store)).unwrap_or_else(|error| {
                    SemanticWorkerReport::unavailable(data_root, format!("{error:#}"))
                });
            }
            Err(error) => {
                return SemanticWorkerReport::unavailable(data_root, format!("{error:#}"));
            }
        }
    }
    semantic_worker_report_best_effort(data_root)
}

fn write_semantic_worker_failure_status(data_root: &Path, message: String) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": "failed",
            "model_key": SEMANTIC_MODEL_KEY,
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "last_error": message,
            "embed_policy": semantic_embed_policy_status_json(),
        }),
    )
}

fn write_semantic_model_acquisition_status(
    data_root: &Path,
    status: &str,
    message: Option<String>,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": status,
            "model_key": SEMANTIC_MODEL_KEY,
            "pid": process::id(),
            "heartbeat_at_ms": now,
            "finished_at_ms": (status == "model_acquisition_failed").then_some(now),
            "last_error": message,
            "embed_policy": semantic_embed_policy_status_json(),
        }),
    )
}

fn run_semantic_worker_inner_with_embedder(
    args: SemanticWorkerArgs,
    data_root: &Path,
    query_hint: Option<String>,
    embedder: &mut Option<SemanticEmbedder>,
) -> Result<()> {
    let Some(_lock) = SemanticWorkerLock::acquire(data_root)? else {
        return Ok(());
    };

    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        return Err(anyhow!(
            "ctx index does not exist yet; run `ctx import --all` or `ctx setup` first"
        ));
    }
    let cache_dir = semantic_worker_cache_dir(data_root);
    if embedder.is_none() && !semantic_model_cache_available(&cache_dir) {
        return Err(anyhow!(
            "semantic model is not available in the local cache; background indexing will not initialize or download {SEMANTIC_MODEL_ID}"
        ));
    }
    let store = Store::open(&db_path).context("open ctx store for semantic worker")?;
    refresh_semantic_document_count_cache(&store)?;
    let vector_path = semantic_vector_path(data_root);
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    let prune_outcome = vector_store.prune_ineligible_events(&store)?;
    let started_at_ms = utc_now().timestamp_millis();
    let initial_stats = vector_store
        .cached_stats()?
        .unwrap_or_else(SemanticSidecarStats::default);
    let initial_dirty_items = vector_store.dirty_event_count()?;
    let searchable_items = store.event_embedding_document_count_cached_or_exact()?;
    let initial_queued_items_estimate = searchable_items
        .saturating_sub(initial_stats.embedded_items)
        .max(initial_dirty_items);
    let was_ready_before_worker =
        semantic_worker_status_was_ready_for_stats(data_root, initial_stats);
    let continue_past_indexed_pages = !was_ready_before_worker
        || initial_queued_items_estimate > SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT;
    let starting_embed_policy = semantic_embedder_policy_status_json(embedder);
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": "running",
            "model_key": SEMANTIC_MODEL_KEY,
            "pid": process::id(),
            "started_at_ms": started_at_ms,
            "heartbeat_at_ms": started_at_ms,
            "indexed_chunks": 0,
            "pruned_chunks": prune_outcome.deleted_chunks,
            "stale_events_queued": prune_outcome.queued_stale_events,
            "searchable_items": searchable_items,
            "embedded_items": initial_stats.embedded_items,
            "embedded_chunks": initial_stats.embedded_chunks,
            "dirty_items": initial_dirty_items,
            "embed_policy": starting_embed_policy,
            "last_error": null,
        }),
    )?;
    let max_chunks = semantic_worker_chunk_budget(&args);
    let max_seconds = semantic_worker_seconds_budget(&args);
    let started = Instant::now();
    let deadline = started + StdDuration::from_secs(max_seconds);
    let mut model_init_ms = None;
    let indexed_chunks = if Instant::now() >= deadline {
        0
    } else {
        backfill_semantic_embeddings(
            &store,
            &mut vector_store,
            embedder,
            &mut model_init_ms,
            &cache_dir,
            query_hint.as_deref(),
            max_chunks,
            true,
            continue_past_indexed_pages,
            Some(deadline),
        )?
    };
    let elapsed = started.elapsed();
    let finished_embed_policy = semantic_embedder_policy_status_json(embedder);
    let elapsed_ms = elapsed.as_millis() as u64;
    let final_stats = vector_store
        .cached_stats()?
        .unwrap_or_else(SemanticSidecarStats::default);
    let final_dirty_items = vector_store.dirty_event_count()?;
    refresh_semantic_document_count_cache(&store)?;
    let searchable_items = store.event_embedding_document_count_cached_or_exact()?;
    let status = if searchable_items > 0
        && final_stats.embedded_items >= searchable_items
        && final_dirty_items == 0
    {
        vector_store.set_backfill_cursor(None)?;
        "ready"
    } else if elapsed >= StdDuration::from_secs(max_seconds) {
        "budget_exhausted"
    } else {
        "completed"
    };
    let finished_at_ms = utc_now().timestamp_millis();
    write_semantic_worker_status(
        data_root,
        &json!({
            "schema_version": 1,
            "status": status,
            "model_key": SEMANTIC_MODEL_KEY,
            "pid": process::id(),
            "started_at_ms": started_at_ms,
            "heartbeat_at_ms": finished_at_ms,
            "finished_at_ms": finished_at_ms,
            "indexed_chunks": indexed_chunks,
            "pruned_chunks": prune_outcome.deleted_chunks,
            "stale_events_queued": prune_outcome.queued_stale_events,
            "elapsed_ms": elapsed_ms,
            "model_init_ms": model_init_ms,
            "searchable_items": searchable_items,
            "embedded_items": final_stats.embedded_items,
            "embedded_chunks": final_stats.embedded_chunks,
            "dirty_items": final_dirty_items,
            "embed_policy": finished_embed_policy,
            "last_error": null,
        }),
    )?;
    drop(_lock);
    Ok(())
}

fn semantic_worker_chunk_budget(args: &SemanticWorkerArgs) -> usize {
    args.max_chunks
        .or_else(|| env_usize("CTX_SEMANTIC_WORKER_MAX_CHUNKS"))
        .map(|value| value.min(SEMANTIC_WORKER_BATCH_MAX))
        .unwrap_or(SEMANTIC_WORKER_BATCH_DEFAULT)
}

fn semantic_worker_seconds_budget(args: &SemanticWorkerArgs) -> u64 {
    args.max_seconds
        .or_else(|| {
            env::var("CTX_SEMANTIC_WORKER_MAX_SECONDS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
        })
        .map(|value| value.min(SEMANTIC_WORKER_MAX_SECONDS_CAP))
        .unwrap_or(SEMANTIC_WORKER_MAX_SECONDS_DEFAULT)
}

fn semantic_worker_status_was_ready_for_stats(
    data_root: &Path,
    stats: SemanticSidecarStats,
) -> bool {
    let Some(value) = read_semantic_worker_status(data_root) else {
        return false;
    };
    if !semantic_status_file_model_matches(Some(&value)) {
        return false;
    }
    let status_ready = json_string(&value, "status").is_some_and(|status| status == "ready");
    let dirty_items = json_usize(&value, "dirty_items").unwrap_or(usize::MAX);
    let embedded_items = json_usize(&value, "embedded_items").unwrap_or(0);
    let searchable_items = json_usize(&value, "searchable_items").unwrap_or(usize::MAX);
    status_ready
        && dirty_items == 0
        && embedded_items == stats.embedded_items
        && embedded_items >= searchable_items
}

fn queue_recent_semantic_work(data_root: &Path, store: &Store, reason: &str) -> Result<usize> {
    let vector_path = semantic_vector_path(data_root);
    if !vector_path.exists()
        && !semantic_model_cache_available(&semantic_worker_cache_dir(data_root))
    {
        return Ok(0);
    }
    let docs = store.recent_event_embedding_documents(None, SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT)?;
    if docs.is_empty() {
        return Ok(0);
    }
    let mut vector_store = SemanticVectorStore::open(&vector_path)?;
    let existing_hashes = vector_store
        .existing_hashes_for_event_ids(&docs.iter().map(|doc| doc.event_id).collect::<Vec<_>>())?;
    let docs = docs
        .into_iter()
        .filter(|doc| {
            let source_text = semantic_source_text(&doc.text);
            let hash = semantic_document_hash(doc, &source_text);
            existing_hashes
                .get(&doc.event_id)
                .map(|existing| existing != &hash)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    vector_store.enqueue_dirty_documents(&docs, reason)
}

pub(crate) fn maybe_autostart_daemon(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
    json_output: bool,
) {
    maybe_autostart_daemon_inner(data_root, config, trigger, json_output, false);
}

pub(crate) fn maybe_autostart_daemon_for_search(data_root: &Path, config: &AppConfig) {
    maybe_autostart_daemon_inner(
        data_root,
        config,
        DaemonTriggerCommandArg::Search,
        false,
        true,
    );
}

fn maybe_autostart_daemon_inner(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
    json_output: bool,
    allow_json_output: bool,
) {
    if semantic_env_flag(DAEMON_BACKGROUND_CHILD_ENV) {
        return;
    }
    if !database_path(data_root.to_path_buf()).exists() {
        return;
    }
    if !config.daemon.enabled {
        return;
    }
    if semantic_env_flag(DAEMON_AUTOSTART_OFF_ENV) {
        return;
    }
    if json_output && !allow_json_output {
        return;
    }
    if semantic_env_flag("CI") {
        return;
    }
    let lock_path = daemon_lock_path(data_root);
    if lock_path.exists() && !daemon_lock_is_stale(&lock_path) {
        return;
    }
    let exe = match daemon_autostart_exe() {
        Ok(exe) => exe,
        Err(error) => {
            let _ = write_daemon_autostart_status(
                data_root,
                trigger,
                "failed",
                Some("current_exe"),
                Some(format!("{error:#}")),
                None,
            );
            return;
        }
    };
    let idle_exit = daemon_autostart_u64_env(
        "CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS",
        DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT,
        DAEMON_IDLE_EXIT_SECONDS_CAP,
    );
    let loop_interval = daemon_autostart_u64_env(
        "CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS",
        DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT,
        3_600,
    );
    match Command::new(exe)
        .arg("--data-root")
        .arg(data_root)
        .arg("daemon")
        .arg("run")
        .arg("--idle-exit-seconds")
        .arg(idle_exit.to_string())
        .arg("--loop-interval-seconds")
        .arg(loop_interval.to_string())
        .arg("--start-mode")
        .arg(DaemonStartModeArg::Auto.as_str())
        .arg("--trigger-command")
        .arg(trigger.as_str())
        .arg("--json")
        .env(DAEMON_BACKGROUND_CHILD_ENV, "1")
        .env("CTX_ANALYTICS_OFF", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_child) => {}
        Err(error) => {
            let _ = write_daemon_autostart_status(
                data_root,
                trigger,
                "failed",
                Some("spawn_failed"),
                Some(error.to_string()),
                None,
            );
        }
    }
}

pub(crate) fn semantic_query_service_supported() -> bool {
    cfg!(unix)
}

#[cfg(unix)]
pub(crate) fn daemon_query_service_available(data_root: &Path) -> bool {
    let path = daemon_query_socket_path(data_root);
    if !path.exists() {
        return false;
    }
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(StdDuration::from_secs(1)));
    let _ = stream.set_write_timeout(Some(StdDuration::from_secs(1)));
    if writeln!(
        stream,
        "{}",
        serde_json::to_string(&compact_json(json!({
            "schema_version": 1,
            "op": "ping",
        })))
        .unwrap_or_else(|_| "{\"schema_version\":1,\"op\":\"ping\"}".to_owned())
    )
    .is_err()
    {
        return false;
    }
    let _ = stream.shutdown(Shutdown::Write);
    let mut body = String::new();
    if stream
        .take(1024)
        .read_to_string(&mut body)
        .is_err()
    {
        return false;
    }
    serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| value.get("ok").and_then(Value::as_bool))
        == Some(true)
}

#[cfg(not(unix))]
pub(crate) fn daemon_query_service_available(_data_root: &Path) -> bool {
    false
}

pub(crate) fn wait_for_daemon_query_service(data_root: &Path, timeout: StdDuration) -> bool {
    if !semantic_query_service_supported() {
        return false;
    }
    let started = Instant::now();
    loop {
        if daemon_query_service_available(data_root) {
            return true;
        }
        if started.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(StdDuration::from_millis(100));
    }
}

fn daemon_autostart_exe() -> Result<PathBuf> {
    env::var("CTX_DAEMON_AUTOSTART_EXE")
        .ok()
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| env::current_exe().context("resolve ctx daemon autostart executable"))
}

fn write_daemon_autostart_status(
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    status: &str,
    reason: Option<&str>,
    last_error: Option<String>,
    pid: Option<u32>,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_daemon_status(
        data_root,
        &compact_json(json!({
            "schema_version": 1,
            "status": status,
            "reason": reason,
            "pid": pid,
            "started_at_ms": Value::Null,
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "start_mode": DaemonStartModeArg::Auto.as_str(),
            "trigger_command": trigger.as_str(),
            "last_error": last_error,
        })),
    )
}

fn daemon_autostart_u64_env(name: &str, default: u64, max: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(max))
        .unwrap_or(default)
}
