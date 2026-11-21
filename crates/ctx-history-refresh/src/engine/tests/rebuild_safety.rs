use super::*;
use crate::engine::read_model::SourceBackedAutomaticRetryCheckpoint;
use ctx_history_capture::SourceBackedProviderRegistry;

fn copy_fixture_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(to, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_fixture_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            let mut permissions = fs::metadata(&target).unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(0o600);
            }
            if matches!(
                target.extension().and_then(|ext| ext.to_str()),
                Some("fast" | "fieldnorm" | "idx" | "pos" | "store" | "term")
            ) {
                permissions.set_readonly(true);
            }
            fs::set_permissions(&target, permissions).unwrap();
        }
    }
}

fn write_rebuild_marker(index_root: &Path) {
    let pointer: Value =
        serde_json::from_slice(&fs::read(index_root.join("active-generation.json")).unwrap())
            .unwrap();
    let generation_id = pointer["active"]["generation_id"].as_str().unwrap();
    let directory = pointer["active"]["directory"].as_str().unwrap();
    let bytes =
        format!(r#"{{"version":1,"generation_id":"{generation_id}","directory":"{directory}"}}"#);
    let marker_path = index_root.join("active-generation-rebuild-required.json");
    fs::write(&marker_path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&marker_path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn schema_22_fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ctx-history-index/tests/fixtures/schema-22")
}

#[derive(Debug, Clone)]
struct RebuildTestRuntime {
    home: PathBuf,
    cwd: PathBuf,
}

impl RefreshRuntime for RebuildTestRuntime {
    fn metadata(&self, _data_root: &Path, operation: RefreshOperation) -> RefreshRuntimeMetadata {
        RefreshRuntimeMetadata {
            operation,
            ..RefreshRuntimeMetadata::default()
        }
    }

    fn discovery_context(&self, _data_root: &Path) -> Result<DiscoveryContext> {
        Ok(DiscoveryContext::new(
            &self.home,
            &self.cwd,
            DiscoveryPlatform::Linux,
            DiscoveryPlatformDirs::default(),
        ))
    }
}

fn test_route(
    path: PathBuf,
    provider: CaptureProvider,
    source_format: &'static str,
) -> (ctx_history_capture::SourceBackedRoute, SourceRouteIdentity) {
    let source = ProviderSource {
        provider,
        path,
        exists: true,
        source_format,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ctx_history_capture_model::ProviderSourceStatus::Available,
        unsupported_reason: None,
        route_provenance: Default::default(),
    };
    let route = ctx_history_capture::SourceBackedRoute::automatic(
        source,
        ctx_history_capture::SourceBackedSelectorAuthority::DiscoveredWinner,
        ctx_history_capture::SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )
    .unwrap();
    let identity = route.metadata().route_identity.clone().unwrap();
    (route, identity)
}

fn hermes_overdue_index(
    index_root: &Path,
    route: &SourceRouteIdentity,
    profile_source_descriptor: [u8; 32],
    exact_due_at_ms: i64,
) {
    let source = SourceKey::derive_provider_native(
        CaptureProvider::Hermes.as_str(),
        "hermes_state_sqlite",
        "hermes-state-session-v1",
        1,
        "hermes-test-profile\u{1f}session-1",
        TypedKey::U64(1),
    )
    .unwrap();
    let database_identity = [1_u8; 32];
    let physical_revision = [3_u8; 32];
    let schema_evidence = [2_u8; 32];
    let revision = serde_json::to_vec(&json!({
        "kind": "hermes-route-control-v1",
        "version": 3,
        "parser_revision": "hermes-source-backed-v5-optional-admission",
        "profile_source_descriptor": profile_source_descriptor,
        "database_identity": database_identity,
        "physical_revision": physical_revision,
        "schema_evidence": schema_evidence,
        "session_rowid": 4,
        "message_rowid": 9,
        "last_successful_exhaustive_at_ms": 100,
        "exact_due_at_ms": exact_due_at_ms,
        "exhaustive_sequence": 1,
        "mode": "exhaustive",
        "outcome": "successful",
    }))
    .unwrap();
    let observation = SourceObservation::new(
        source.clone(),
        "hermes-source-backed-v3",
        b"session-revision".to_vec(),
    )
    .unwrap();
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        "hermes-source-backed-v3",
        [3; 32],
        ScannedSourceCounts::default(),
    )
    .unwrap();
    let mut writer = ctx_history_index::GenerationWriter::open(
        index_root,
        ctx_history_index::WriterOptions::default(),
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.certify_source(certificate).unwrap();
    writer
        .set_present_source_routes(vec![ctx_history_index::SourceRouteSnapshot::present(
            route.clone(),
            vec![source],
        )
        .unwrap()])
        .unwrap();
    let request_route = route.clone();
    writer
        .commit_with_generation_state(
            |_| true,
            |_| false,
            move |_| {
                SourceBackedGenerationState::new(
                    None,
                    Vec::new(),
                    BTreeMap::new(),
                    BTreeMap::from([(request_route.clone(), revision.clone())]),
                    Vec::new(),
                )?
                .envelope()
            },
            |_| Ok(()),
        )
        .unwrap();
}

#[test]
fn overdue_hermes_reconciliation_does_not_enqueue_exact_against_rebuild_required_generation() {
    let base = std::env::temp_dir().canonicalize().unwrap();
    let temp = tempfile::tempdir_in(base).unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();

    let hermes_db = temp.path().join("hermes-state.db");
    fs::write(&hermes_db, b"hermes").unwrap();
    let hermes_source = ProviderSource {
        provider: CaptureProvider::Hermes,
        path: hermes_db,
        exists: true,
        source_format: "hermes_state_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ctx_history_capture_model::ProviderSourceStatus::Available,
        unsupported_reason: None,
        route_provenance: Default::default(),
    };
    let profile_descriptor = [7_u8; 32];
    let expectation = ctx_history_capture::SourceBackedRouteControlExpectation::new(
        "hermes-route-control-v1",
        profile_descriptor,
        ctx_history_capture::hermes_route_control_exact_due_for_profile,
        Some(ctx_history_capture::hermes_route_control_database_identity),
    );
    let driver = ctx_history_capture::SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true)
        .with_route_control_expectation(expectation);
    let hermes_route = ctx_history_capture::SourceBackedRoute::automatic(
        hermes_source,
        ctx_history_capture::SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )
    .unwrap();
    let route_id = hermes_route.metadata().route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(hermes_route);
    let catalog = registry.watch_catalog();

    hermes_overdue_index(&index_root, &route_id, profile_descriptor, 1_000);

    // Positive control: a healthy schema-23 generation with an overdue Hermes
    // route enqueues Exact reconciliation.
    let healthy_engine = CoreRefreshEngine::new();
    healthy_engine.install_watch_catalog(catalog.clone());
    assert!(healthy_engine
        .enqueue_overdue_hermes_exact_reconciliation(&data_root, &catalog, 2_000)
        .unwrap());
    assert!(healthy_engine.has_pending_request());

    // Once marked for rebuild, overdue Hermes reconciliation must NOT enqueue
    // a Hermes-only Exact publish that would drop non-Hermes routes.
    write_rebuild_marker(&index_root);
    let rebuild_engine = CoreRefreshEngine::new();
    rebuild_engine.install_watch_catalog(catalog.clone());
    assert!(!rebuild_engine
        .enqueue_overdue_hermes_exact_reconciliation(&data_root, &catalog, 2_000)
        .unwrap());
    assert!(!rebuild_engine.has_pending_request());

    // Genuine schema-22 predecessor must also refuse Hermes Exact reconciliation.
    let schema_22_data = temp.path().join("schema-22-data");
    ctx_history_platform::platform_security::establish_private_data_root(&schema_22_data).unwrap();
    copy_fixture_dir(
        &schema_22_fixture_root().join("index"),
        &source_backed_index_root(&schema_22_data),
    );
    let schema_22_engine = CoreRefreshEngine::new();
    schema_22_engine.install_watch_catalog(catalog.clone());
    assert!(!schema_22_engine
        .enqueue_overdue_hermes_exact_reconciliation(&schema_22_data, &catalog, 2_000)
        .unwrap());
    assert!(!schema_22_engine.has_pending_request());
}

#[test]
fn paused_route_scheduled_and_periodic_admission_refuse_exact_against_rebuild_required_generation()
{
    let base = std::env::temp_dir().canonicalize().unwrap();
    let temp = tempfile::tempdir_in(base).unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    copy_fixture_dir(
        &schema_22_fixture_root().join("index"),
        &source_backed_index_root(&data_root),
    );

    let paused_path = temp.path().join("paused.jsonl");
    let healthy_path = temp.path().join("healthy.jsonl");
    fs::write(&paused_path, b"paused\n").unwrap();
    fs::write(&healthy_path, b"healthy\n").unwrap();
    let (paused_source, paused_route) = test_route(
        paused_path,
        CaptureProvider::Codex,
        "codex_session_jsonl_tree",
    );
    let (healthy_source, healthy_route) = test_route(
        healthy_path,
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
    );
    assert_ne!(paused_route, healthy_route);

    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(paused_source);
    registry.register(healthy_source);
    let catalog = registry.watch_catalog();
    let observation = catalog.certify_route_observation(&paused_route).unwrap();
    let outcome = RefreshTerminalOutcome::with_route_dispositions(
        RefreshOutcomeCode::SourceRefreshFailed,
        true,
        BTreeSet::from([paused_route.clone()]),
        BTreeSet::new(),
        "paused-attempt".to_owned(),
        None,
        None,
        Some(RefreshRetryAdvice::RetryAffectedRoutes),
        None,
    )
    .unwrap();
    let mut checkpoint = SourceBackedAutomaticRetryCheckpoint::confirming(
        &outcome,
        &paused_route,
        &observation,
        "stable internal failure",
    );
    checkpoint.pause();

    let coordinator = CoreRefreshEngine::new();
    coordinator.install_watch_catalog(catalog);
    let prior_id = {
        let mut state = coordinator.lock_state();
        let mut prior = new_refresh_attempt(
            None,
            SourceRefreshRuntimeMetadata::periodic(),
            RefreshIntent::AutomaticMaintenance,
            SourceBackedRefreshScope::All,
        );
        prior.state = SourceBackedRefreshState::Failed;
        prior.automatic_retry_checkpoints =
            BTreeMap::from([(paused_route.clone(), checkpoint.clone())]);
        let prior_id = prior.request_id.clone();
        state.attempts.push_back(prior);
        state
            .automatic_retry_checkpoints
            .insert(paused_route.clone(), checkpoint);
        state.dirty_routes.seed_exact_routes(
            [paused_route.clone()],
            EventWatermark::new(80, 1),
            source_route_ledger_now_ms().saturating_sub(1_000),
        );
        state.dirty_routes.block_exact_routes([&paused_route]);
        prior_id
    };
    coordinator.schedule_startup_route_reconciliation(
        [healthy_route.clone()],
        EventWatermark::new(81, 1),
        source_route_ledger_now_ms().saturating_sub(1_000),
    );

    // Scheduled refresh, provider-root refresh, and periodic refresh must ALL
    // refuse to fall back to Exact([healthy_route]) against a schema-22 index.
    assert!(!coordinator
        .enqueue_next_scheduled_refresh(&data_root, source_route_ledger_now_ms())
        .unwrap());
    assert!(!coordinator.has_pending_request());

    assert!(!coordinator
        .enqueue_provider_root_refresh(&data_root, source_route_ledger_now_ms())
        .unwrap());
    assert!(!coordinator.has_pending_request());

    let periodic = coordinator.enqueue_periodic(&data_root).unwrap();
    assert_eq!(periodic["request_id"], prior_id);
    assert!(!coordinator.has_pending_request());

    // Once the paused route is cleared, scheduled admission emits All + Exhaustive.
    {
        let mut state = coordinator.lock_state();
        state.automatic_retry_checkpoints.clear();
        state.dirty_routes.seed_exact_routes(
            [paused_route],
            EventWatermark::new(82, 1),
            source_route_ledger_now_ms().saturating_sub(1_000),
        );
    }
    assert!(coordinator
        .enqueue_next_scheduled_refresh(&data_root, source_route_ledger_now_ms())
        .unwrap());
    let state = coordinator.lock_state();
    let active_id = state.active_request_id.as_deref().unwrap();
    let attempt = find_attempt(&state, active_id).unwrap();
    assert_eq!(attempt.refresh_scope, SourceBackedRefreshScope::All);
    assert_eq!(
        attempt.reconciliation_demand,
        SourceBackedReconciliationDemand::Exhaustive
    );
}

#[test]
fn scoped_provider_and_exact_source_admissions_widen_to_complete_catalog_during_rebuild() {
    let base = std::env::temp_dir().canonicalize().unwrap();
    let temp = tempfile::tempdir_in(base).unwrap();
    let data_root = temp.path().join("data");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    copy_fixture_dir(
        &schema_22_fixture_root().join("index"),
        &source_backed_index_root(&data_root),
    );

    let codex_sessions = home.join(".codex/sessions");
    fs::create_dir_all(&codex_sessions).unwrap();
    fs::copy(
        schema_22_fixture_root().join("source.jsonl"),
        codex_sessions.join("rollout-019fb000-0000-7000-8000-000000000100.jsonl"),
    )
    .unwrap();
    let claude_projects = home.join(".claude/projects/example");
    fs::create_dir_all(&claude_projects).unwrap();
    fs::write(
        claude_projects.join("019fb000-0000-7000-8000-000000000200.jsonl"),
        concat!(
            r#"{"type":"user","uuid":"019fb000-0000-7000-8000-000000000201","sessionId":"019fb000-0000-7000-8000-000000000200","timestamp":"2026-08-17T00:00:00Z","cwd":"/example","message":{"role":"user","content":"claudewidenmarker"}}"#,
            "\n",
        ),
    )
    .unwrap();
    let external_codex = temp.path().join("external-codex-sessions");
    fs::create_dir_all(&external_codex).unwrap();
    fs::copy(
        schema_22_fixture_root().join("source.jsonl"),
        external_codex.join("rollout-019fb000-0000-7000-8000-000000000199.jsonl"),
    )
    .unwrap();

    let runtime: Arc<dyn RefreshRuntime> = Arc::new(RebuildTestRuntime { home, cwd });

    // Case 1: Scoped `--provider codex` widens to CompleteCatalog covering Codex + Claude.
    let journal = Arc::new(TestRefreshJournal::default());
    let engine = super::super::CoreRefreshEngine::new(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        Arc::clone(&runtime),
    );
    let provider_req_id = "019fcaaa-0000-7000-8000-000000000901".to_owned();
    let admission = engine
        .submit(
            &data_root,
            RefreshRequest::selected_import(
                provider_req_id.clone(),
                RefreshSelection::Provider(CaptureProvider::Codex),
                SourceBackedReconciliationDemand::Incremental,
            ),
        )
        .unwrap();
    let (_, barrier) = admission.into_parts();
    if let Some(barrier) = barrier {
        barrier.release(&engine);
    }
    assert!(engine
        .resolve_pending_admission(&data_root)
        .unwrap()
        .is_none());
    {
        let state = engine.lock_state();
        let attempt = find_attempt(&state, &provider_req_id).unwrap();
        assert_eq!(attempt.state, SourceBackedRefreshState::Queued);
        assert_eq!(attempt.refresh_scope, SourceBackedRefreshScope::All);
        assert_eq!(
            attempt.reconciliation_demand,
            SourceBackedReconciliationDemand::Exhaustive
        );
        let authority = attempt.admitted_authority.as_ref().unwrap();
        assert_eq!(
            authority.coverage(),
            ctx_history_refresh_execution::AdmittedRefreshCoverage::CompleteCatalog
        );
        let catalog = authority.discovery().watch_catalog();
        assert!(
            !catalog
                .route_ids_for_provider(CaptureProvider::Codex)
                .is_empty(),
            "widened admission must retain Codex routes"
        );
        assert!(
            !catalog
                .route_ids_for_provider(CaptureProvider::Claude)
                .is_empty(),
            "widened admission must include Claude routes"
        );
    }
    // Crash recovery of a queued widened scoped import against schema 22 must succeed.
    let recovered = super::super::CoreRefreshEngine::new(
        journal as Arc<dyn RefreshJournal>,
        Arc::clone(&runtime),
    );
    assert!(recovered
        .recover_interrupted_publication(&data_root)
        .unwrap());

    // Case 2: Scoped `--path` explicit source widens to CompleteCatalog covering
    // automatic Codex, automatic Claude, AND the requested explicit path.
    let explicit_source = ctx_history_refresh_execution::explicit_source_for_path(
        &data_root,
        &external_codex,
        Some(CaptureProvider::Codex),
        false,
    )
    .unwrap();
    let upsert =
        ctx_history_refresh_execution::upsert_explicit_source(&data_root, &explicit_source)
            .unwrap();
    let engine = super::super::CoreRefreshEngine::new(
        Arc::new(TestRefreshJournal::default()),
        Arc::clone(&runtime),
    );
    let exact_req_id = "019fcaaa-0000-7000-8000-000000000902".to_owned();
    let admission = engine
        .submit(
            &data_root,
            RefreshRequest::selected_import(
                exact_req_id.clone(),
                RefreshSelection::ExactSource(upsert.authority),
                SourceBackedReconciliationDemand::Incremental,
            ),
        )
        .unwrap();
    let (_, barrier) = admission.into_parts();
    if let Some(barrier) = barrier {
        barrier.release(&engine);
    }
    assert!(engine
        .resolve_pending_admission(&data_root)
        .unwrap()
        .is_none());
    let state = engine.lock_state();
    let attempt = find_attempt(&state, &exact_req_id).unwrap();
    assert_eq!(attempt.state, SourceBackedRefreshState::Queued);
    assert_eq!(attempt.refresh_scope, SourceBackedRefreshScope::All);
    assert_eq!(
        attempt.reconciliation_demand,
        SourceBackedReconciliationDemand::Exhaustive
    );
    let authority = attempt.admitted_authority.as_ref().unwrap();
    assert_eq!(
        authority.coverage(),
        ctx_history_refresh_execution::AdmittedRefreshCoverage::CompleteCatalog
    );
    assert!(
        authority.route_worksets().is_empty(),
        "rebuild-widened explicit import must scan exhaustively with empty worksets"
    );
    let catalog = authority.discovery().watch_catalog();
    assert!(!catalog
        .route_ids_for_provider(CaptureProvider::Claude)
        .is_empty());
    assert!(
        catalog.route_ids_for_provider(CaptureProvider::Codex).len() >= 2,
        "widened explicit import must include both automatic Codex and explicit Codex routes"
    );
}

#[test]
fn exact_automatic_maintenance_admission_is_rejected_against_rebuild_required_generation() {
    let base = std::env::temp_dir().canonicalize().unwrap();
    let temp = tempfile::tempdir_in(base).unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    copy_fixture_dir(
        &schema_22_fixture_root().join("index"),
        &source_backed_index_root(&data_root),
    );

    let route_path = temp.path().join("route.jsonl");
    fs::write(&route_path, b"route\n").unwrap();
    let (route_entry, route_id) = test_route(
        route_path,
        CaptureProvider::Codex,
        "codex_session_jsonl_tree",
    );
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route_entry);
    let catalog = registry.watch_catalog();

    let engine = CoreRefreshEngine::new();
    engine.install_watch_catalog(catalog);
    {
        let mut state = engine.lock_state();
        let mut attempt = new_refresh_attempt(
            None,
            SourceRefreshRuntimeMetadata::periodic(),
            RefreshIntent::AutomaticMaintenance,
            SourceBackedRefreshScope::exact([route_id]),
        );
        attempt.state = SourceBackedRefreshState::AdmissionPending;
        attempt.progress.phase = "admission_pending".to_owned();
        let req_id = attempt.request_id.clone();
        state.active_request_id = Some(req_id);
        state.attempts.push_back(attempt);
    }

    let run = engine
        .resolve_pending_admission(&data_root)
        .unwrap()
        .expect("exact maintenance against schema 22 must fail admission");
    assert!(run.failed);
    assert!(
        run.job["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("requires a full rebuild"),
        "{:#}",
        run.job
    );
}

#[test]
fn widened_scoped_import_recovers_after_active_generation_advances_to_current_schema() {
    let base = std::env::temp_dir().canonicalize().unwrap();
    let temp = tempfile::tempdir_in(base).unwrap();
    let data_root = temp.path().join("data");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    copy_fixture_dir(
        &schema_22_fixture_root().join("index"),
        &source_backed_index_root(&data_root),
    );

    let codex_sessions = home.join(".codex/sessions/2026/01/01");
    fs::create_dir_all(&codex_sessions).unwrap();
    fs::copy(
        schema_22_fixture_root().join("source.jsonl"),
        codex_sessions.join("rollout-019fb000-0000-7000-8000-000000000100.jsonl"),
    )
    .unwrap();

    let runtime: Arc<dyn RefreshRuntime> = Arc::new(RebuildTestRuntime { home, cwd });

    // First run a real schema-22 -> schema-23 upgrade once to produce a valid
    // schema-23 index directory with canonical generation-owned state.
    let upgraded_index_template = temp.path().join("upgraded-template-data");
    ctx_history_platform::platform_security::establish_private_data_root(&upgraded_index_template)
        .unwrap();
    copy_fixture_dir(
        &schema_22_fixture_root().join("index"),
        &source_backed_index_root(&upgraded_index_template),
    );
    let seed_engine = super::super::CoreRefreshEngine::new(
        Arc::new(TestRefreshJournal::default()),
        Arc::clone(&runtime),
    );
    let seed_admission = seed_engine
        .submit(
            &upgraded_index_template,
            RefreshRequest::selected_import(
                "019fcbbb-0000-7000-8000-000000000900".to_owned(),
                RefreshSelection::Provider(CaptureProvider::Codex),
                SourceBackedReconciliationDemand::Incremental,
            ),
        )
        .unwrap();
    let (_, seed_barrier) = seed_admission.into_parts();
    if let Some(barrier) = seed_barrier {
        barrier.release(&seed_engine);
    }
    assert!(seed_engine
        .resolve_pending_admission(&upgraded_index_template)
        .unwrap()
        .is_none());
    let seed_run = seed_engine
        .run_next(&upgraded_index_template)
        .expect("seed upgrade must publish schema 23");
    assert!(!seed_run.failed, "{:#}", seed_run.job);

    for simulated_state in ["queued", "running"] {
        let journal = Arc::new(TestRefreshJournal::default());
        let engine = super::super::CoreRefreshEngine::new(
            Arc::clone(&journal) as Arc<dyn RefreshJournal>,
            Arc::clone(&runtime),
        );
        let req_id = format!(
            "019fcbbb-0000-7000-8000-00000000090{}",
            if simulated_state == "queued" { 1 } else { 2 }
        );
        let admission = engine
            .submit(
                &data_root,
                RefreshRequest::selected_import(
                    req_id.clone(),
                    RefreshSelection::Provider(CaptureProvider::Codex),
                    SourceBackedReconciliationDemand::Incremental,
                ),
            )
            .unwrap();
        let (_, barrier) = admission.into_parts();
        if let Some(barrier) = barrier {
            barrier.release(&engine);
        }
        assert!(engine
            .resolve_pending_admission(&data_root)
            .unwrap()
            .is_none());

        // Simulate the crash window: durable core-refresh.json holds the widened
        // `refresh_scope: {"kind": "all"}` in `queued` or `running`, while
        // active-generation.json on disk has already been upgraded to schema 23.
        let mut persisted_job = journal.load(&data_root).unwrap().unwrap();
        persisted_job["request_state"] = json!(simulated_state);
        assert_eq!(persisted_job["refresh_scope"]["kind"], "all");

        let upgraded_root = temp.path().join(format!("upgraded-{simulated_state}"));
        ctx_history_platform::platform_security::establish_private_data_root(&upgraded_root)
            .unwrap();
        copy_fixture_dir(
            &source_backed_index_root(&upgraded_index_template),
            &source_backed_index_root(&upgraded_root),
        );
        let upgraded_journal = Arc::new(TestRefreshJournal::default());
        upgraded_journal
            .store(&upgraded_root, &persisted_job)
            .unwrap();

        let recovered = super::super::CoreRefreshEngine::new(
            upgraded_journal as Arc<dyn RefreshJournal>,
            Arc::clone(&runtime),
        );
        assert!(
            recovered
                .recover_interrupted_publication(&upgraded_root)
                .unwrap(),
            "recovery must not wedge when widened {simulated_state} job recovers against schema 23"
        );
        if let Some(failed) = recovered.resolve_pending_admission(&upgraded_root).unwrap() {
            panic!(
                "expected recovered {simulated_state} job admission to succeed, got: {:#}",
                failed.job
            );
        }
        let state = recovered.lock_state();
        let attempt = find_attempt(&state, &req_id).unwrap();
        assert_eq!(attempt.state, SourceBackedRefreshState::Queued);
        assert!(
            matches!(attempt.refresh_scope, SourceBackedRefreshScope::Exact(_)),
            "recovered scoped import must re-narrow back to Exact against upgraded schema 23"
        );
    }
}

#[test]
fn provider_rebuild_admission_rejects_provider_with_no_executable_routes_even_when_catalog_is_empty(
) {
    let base = std::env::temp_dir().canonicalize().unwrap();
    let temp = tempfile::tempdir_in(base).unwrap();
    let data_root = temp.path().join("data");
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    copy_fixture_dir(
        &schema_22_fixture_root().join("index"),
        &source_backed_index_root(&data_root),
    );

    let runtime: Arc<dyn RefreshRuntime> = Arc::new(RebuildTestRuntime { home, cwd });
    let engine = super::super::CoreRefreshEngine::new(
        Arc::new(TestRefreshJournal::default()),
        Arc::clone(&runtime),
    );
    let req_id = "019fcccc-0000-7000-8000-000000000903".to_owned();
    let admission = engine
        .submit(
            &data_root,
            RefreshRequest::selected_import(
                req_id,
                RefreshSelection::Provider(CaptureProvider::Gemini),
                SourceBackedReconciliationDemand::Incremental,
            ),
        )
        .unwrap();
    let (_, barrier) = admission.into_parts();
    if let Some(barrier) = barrier {
        barrier.release(&engine);
    }
    let run = engine
        .resolve_pending_admission(&data_root)
        .unwrap()
        .expect("empty provider rebuild admission must fail instead of publishing empty");
    assert!(run.failed);
    assert!(
        run.job["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("produced no executable source routes"),
        "{:#}",
        run.job
    );
}
