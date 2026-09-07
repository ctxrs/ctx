use super::*;
use ctx_history_refresh::{DurableAdmissionPersistence, RefreshJournal, RefreshRuntime};

#[derive(Default)]
struct AdmissionJournal {
    writes: Mutex<Vec<Value>>,
    inner: DaemonRefreshJournal,
}
impl RefreshJournal for AdmissionJournal {
    fn load(&self, root: &Path) -> Result<Option<Value>> {
        self.inner.load(root)
    }
    fn store(&self, root: &Path, value: &Value) -> Result<()> {
        self.inner.store(root, value)
    }
    fn store_before_ack(&self, root: &Path, value: &Value) -> DurableAdmissionPersistence {
        let outcome = self.inner.store_before_ack(root, value);
        self.writes.lock().unwrap().push(value.clone());
        outcome
    }
}

struct DiscoveredRuntime {
    discovery: ctx_history_capture::DiscoveryContext,
    after_capture: Mutex<Option<LateAdmission>>,
    captures: std::sync::atomic::AtomicUsize,
}
impl RefreshRuntime for DiscoveredRuntime {
    fn metadata(
        &self,
        data_root: &Path,
        operation: ctx_history_refresh::RefreshOperation,
    ) -> ctx_history_refresh::RefreshRuntimeMetadata {
        DaemonRefreshRuntime::new(&crate::test_support::CONFIG).metadata(data_root, operation)
    }
    fn discovery_context(&self, _: &Path) -> Result<ctx_history_capture::DiscoveryContext> {
        Ok(self.discovery.clone())
    }
    fn refresh_execution_finished(&self) {
        self.captures
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(hook) = self.after_capture.lock().unwrap().take() {
            hook.run();
        }
    }
}

struct LateAdmission {
    engine: std::sync::Weak<CoreRefreshEngine>,
    data_root: std::path::PathBuf,
    source_path: std::path::PathBuf,
    routes: BTreeSet<ctx_history_index::SourceRouteIdentity>,
    request_id: String,
    append: bool,
}
impl LateAdmission {
    fn run(self) {
        let engine = self.engine.upgrade().expect("capture owner remains alive");
        if self.append {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&self.source_path)
                .unwrap();
            writeln!(file, "{}", json!({"timestamp":"2026-07-30T12:00:02Z","type":"response_item",
                "payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"late source content must have its own newer generation"}]}})).unwrap();
        }
        if self.append {
            engine.record_watch_routes_requiring_exhaustive_reconciliation(
                self.routes
                    .iter()
                    .cloned()
                    .map(|route| (route, EventWatermark::new(10_000, 1))),
                10_000,
            );
            assert!(self
                .routes
                .is_subset(&engine.scheduled_route_ids_for_test()));
        }
        // Existing runtime boundary always runs after real capture returns,
        // including catalogs with no bounded watcher-observation tokens.
        let response = engine
            .handle_ipc_request(
                &self.data_root,
                &json!({
                    "op":SOURCE_REFRESH_REQUEST_OP,"mode":"background","trigger":"search",
                    "request_id":self.request_id,"refresh_intent":{"kind":"automatic_maintenance"},
                }),
            )
            .unwrap()
            .expect("late actual admission");
        assert_eq!(response["ok"], true);
    }
}

struct DiscoveredFixture {
    data_root: tempfile::TempDir,
    _providers: tempfile::TempDir,
    source_path: std::path::PathBuf,
    fixture_routes: BTreeSet<ctx_history_index::SourceRouteIdentity>,
    input_bytes: u64,
    runtime: Arc<DiscoveredRuntime>,
    journal: Arc<AdmissionJournal>,
    engine: Arc<CoreRefreshEngine>,
    generation: String,
}
impl DiscoveredFixture {
    fn new() -> Result<Self> {
        let data_root = short_data_root()?;
        ctx_history_platform::platform_security::establish_private_data_root(data_root.path())?;
        let providers = tempfile::tempdir()?;
        let home = providers.path().join("home");
        let cwd = providers.path().join("cwd");
        let codex_home = home.join(".codex");
        std::fs::create_dir_all(codex_home.join("sessions"))?;
        std::fs::create_dir_all(&cwd)?;
        let discovery = ctx_history_capture::DiscoveryContext::new(
            &home,
            &cwd,
            ctx_history_capture::DiscoveryPlatform::Linux,
            ctx_history_capture::DiscoveryPlatformDirs::default(),
        )
        .with_env("CODEX_HOME", codex_home.as_os_str());
        let source_path =
            codex_home.join("sessions/rollout-019f1111-1111-7111-8111-111111111111.jsonl");
        // Synthetic Core input: this exercises discovery and refresh, not a
        // provider harness's native output or conformance.
        let mut input = format!(
            "{}\n",
            json!({"timestamp":"2026-07-30T12:00:00Z","type":"session_meta",
            "payload":{"id":"019f1111-1111-7111-8111-111111111111","timestamp":"2026-07-30T12:00:00Z",
                "cwd":"/fixed-refresh-workspace","originator":"codex_cli_rs","cli_version":"0.20.0",
                "source":"cli","model_provider":"openai"}})
        );
        for index in 0..128 {
            input.push_str(&format!(
                "{}\n",
                json!({"timestamp":"2026-07-30T12:00:01Z","type":"response_item",
                "payload":{"type":"message","role":"user","content":[{"type":"input_text",
                    "text":format!("message {index}: {}", "bounded refresh work ".repeat(200))}]}})
            ));
        }
        std::fs::write(&source_path, &input)?;
        let catalog =
            ctx_history_refresh::source_backed_watch_catalog(data_root.path(), &discovery)?;
        let fixture_routes = catalog.routes_overlapping_path(&source_path);
        assert!(
            !fixture_routes.is_empty(),
            "source must belong to automatic catalog"
        );
        let runtime = Arc::new(DiscoveredRuntime {
            discovery,
            after_capture: Mutex::new(None),
            captures: Default::default(),
        });
        let journal = Arc::new(AdmissionJournal::default());
        let engine = Arc::new(CoreRefreshEngine(ctx_history_refresh::RefreshEngine::new(
            journal.clone(),
            runtime.clone(),
        )));
        engine.install_watch_catalog(catalog);
        let accepted = engine
            .handle_ipc_request(
                data_root.path(),
                &json!({
                    "op":SOURCE_REFRESH_REQUEST_OP,"mode":"wait","trigger":"search",
                    "refresh_intent":{"kind":"automatic_maintenance"}
                }),
            )?
            .context("warm admission")?;
        assert_eq!(accepted["ok"], true);
        let warm = engine
            .run_next(data_root.path())
            .context("warm publication")?;
        assert!(!warm.failed, "{:#}", warm.job);
        let generation = warm.job["published_generation"]
            .as_str()
            .context("warm generation")?
            .to_owned();
        let fixture = Self {
            data_root,
            _providers: providers,
            source_path,
            fixture_routes,
            input_bytes: input.len() as u64,
            runtime,
            journal,
            engine,
            generation,
        };
        fixture.assert_capture(&warm.job, 128)?;
        fixture
            .runtime
            .captures
            .store(0, std::sync::atomic::Ordering::SeqCst);
        fixture.journal.writes.lock().unwrap().clear();
        Ok(fixture)
    }
    fn assert_capture(&self, job: &Value, documents: u64) -> Result<()> {
        for route in &self.fixture_routes {
            assert!(
                job["receipt"]["route_results"]
                    .get(route.as_str())
                    .is_some(),
                "fixture route omitted"
            );
        }
        assert_eq!(
            job["receipt"]["current"]["current_indexed_documents"],
            documents
        );
        assert_eq!(
            job["progress"]["processed_bytes"],
            std::fs::metadata(&self.source_path)?.len()
        );
        Ok(())
    }
    fn captures(&self) -> usize {
        self.runtime
            .captures
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn assert_burst(background: bool) -> Result<()> {
    let fixture = DiscoveredFixture::new()?;
    let callers = if background { 16 } else { 8 };
    let mode = if background {
        SourceBackedRefreshMode::Background
    } else {
        SourceBackedRefreshMode::Wait
    };
    let engine = fixture.engine.clone();
    let root = fixture.data_root.path().to_owned();
    let runs = Arc::new(Mutex::new(Vec::new()));
    let server_runs = runs.clone();
    let mut admitted = 0;
    let (observations, exchanges) = foreground_transport_fixture(
        fixture.data_root.path(),
        move |request| {
            if request["op"] == SOURCE_REFRESH_REQUEST_OP {
                admitted += 1;
            } else if !background && admitted == callers && engine.has_pending_request() {
                let run = engine.run_next(&root).context("burst publication")?;
                assert!(!run.failed, "{:#}", run.job);
                server_runs.lock().unwrap().push(run.job);
            }
            engine
                .handle_ipc_request(&root, request)?
                .context("burst response")
        },
        || {
            if background {
                (0..callers)
                    .map(|_| {
                        coordinate_source_backed_refresh(
                            &RecordingAvailability::default(),
                            fixture.data_root.path(),
                            mode,
                        )
                    })
                    .collect::<Vec<_>>()
            } else {
                std::thread::scope(|scope| {
                    let clients = (0..callers)
                        .map(|_| {
                            scope.spawn(|| {
                                coordinate_source_backed_refresh(
                                    &RecordingAvailability::default(),
                                    fixture.data_root.path(),
                                    mode,
                                )
                            })
                        })
                        .collect::<Vec<_>>();
                    clients
                        .into_iter()
                        .map(|client| client.join().unwrap())
                        .collect()
                })
            }
        },
    )?;
    let mut accepted = BTreeSet::new();
    let mut rejected = 0;
    for observation in observations {
        let observation = observation?;
        assert_eq!(observation.pin.generation_id(), fixture.generation);
        assert!(observation.daemon_available);
        if observation.status == "admission_rejected" {
            assert!(observation.request_id.is_none() && observation.receipt.is_none());
            rejected += 1;
        } else {
            assert!(accepted.insert(observation.request_id.context("accepted identity")?));
        }
    }
    assert_eq!(accepted.len(), 8);
    assert_eq!(rejected, if background { 8 } else { 0 });
    assert_eq!(
        fixture
            .journal
            .writes
            .lock()
            .unwrap()
            .iter()
            .filter(|job| matches!(
                job["request_state"].as_str(),
                Some("admission_pending" | "queued")
            ))
            .count(),
        16,
        "two writes per admission"
    );
    for (request, response) in exchanges
        .iter()
        .filter(|(q, _)| q["op"] == SOURCE_REFRESH_REQUEST_OP)
    {
        if response["ok"] == true {
            assert_eq!(request["request_id"], response["request_id"]);
        }
    }
    if background {
        assert_eq!(
            fixture.captures(),
            0,
            "background only admits before returning"
        );
        let run = fixture
            .engine
            .run_next(fixture.data_root.path())
            .context("background publication")?;
        assert!(!run.failed, "{:#}", run.job);
        runs.lock().unwrap().push(run.job);
    }
    assert_eq!(
        fixture.captures(),
        1,
        "eight callers share one physical capture"
    );
    let writes = fixture.journal.writes.lock().unwrap();
    assert_eq!(
        writes
            .iter()
            .filter(|job| job["request_state"] == "published")
            .count(),
        8
    );
    assert_eq!(
        writes.len(),
        24,
        "sixteen admission and eight terminal durable writes"
    );
    let bytes: usize = writes
        .iter()
        .map(|job| serde_json::to_vec_pretty(job).unwrap().len() + 1)
        .sum();
    eprintln!("terminal_durability shared_capture=1 caller_ids=8 admission_writes=16 terminal_writes=8 durable_serialized_bytes={bytes}");
    drop(writes);

    assert!(!fixture.engine.has_pending_request());
    let runs = runs.lock().unwrap();
    assert_eq!(runs.len(), 1);
    fixture.assert_capture(&runs[0], 128)?;
    for id in accepted {
        let status = fixture.engine.status(&id).context("retained caller")?;
        assert_eq!(status["request_state"], "published");
        assert_eq!(status["published_generation"], fixture.generation);
        assert_eq!(status["receipt"], runs[0]["receipt"]);
    }
    assert_eq!(
        std::fs::metadata(&fixture.source_path)?.len(),
        fixture.input_bytes
    );
    Ok(())
}

#[test]
fn discovered_wait_burst_keeps_eight_ids_and_one_capture() -> Result<()> {
    assert_burst(false)
}
#[test]
fn discovered_background_burst_admits_eight_and_rejects_overflow() -> Result<()> {
    assert_burst(true)
}

fn assert_late_publication(append: bool) -> Result<()> {
    let fixture = DiscoveredFixture::new()?;
    let data_root = &fixture.data_root;
    let engine = &fixture.engine;
    let warm_generation = &fixture.generation;
    let source_path = &fixture.source_path;
    let fixture_routes = &fixture.fixture_routes;
    let request = |id: String| json!({"op":SOURCE_REFRESH_REQUEST_OP,"mode":"background","trigger":"search", "request_id":id,"refresh_intent":{"kind":"automatic_maintenance"}});
    let first_id = Uuid::now_v7().to_string();
    let peer_id = Uuid::now_v7().to_string();
    let late_id = Uuid::now_v7().to_string();
    for id in [&first_id, &peer_id] {
        let response = engine
            .handle_ipc_request(data_root.path(), &request(id.clone()))?
            .context("late fixture admission")?;
        assert_eq!(response["ok"], true);
    }
    while engine.prepare_next_pending_admission(data_root.path())? {}
    *fixture.runtime.after_capture.lock().unwrap() = Some(LateAdmission {
        engine: Arc::downgrade(engine),
        data_root: data_root.path().to_owned(),
        source_path: source_path.clone(),
        routes: fixture_routes.clone(),
        request_id: late_id.clone(),
        append,
    });
    let first = engine
        .run_next(data_root.path())
        .context("first real publication")?;
    assert!(!first.failed, "{:#}", first.job);
    assert_eq!(first.job["published_generation"], *warm_generation);
    for id in [&first_id, &peer_id] {
        assert_eq!(
            engine.status(id).context("first peer status")?["request_state"],
            "published"
        );
    }
    assert_eq!(
        engine.status(&late_id).context("late retained status")?["request_state"],
        "admission_pending"
    );
    let second = engine
        .run_next(data_root.path())
        .context("late real execution")?;
    assert!(!second.failed, "{:#}", second.job);
    assert_eq!(second.job["request_id"], late_id);
    for route in fixture_routes {
        assert!(
            first.job["receipt"]["route_results"]
                .get(route.as_str())
                .is_some(),
            "first fixture route omitted"
        );
        assert!(
            second.job["receipt"]["route_results"]
                .get(route.as_str())
                .is_some(),
            "second fixture route omitted"
        );
    }
    assert_eq!(
        second.job["receipt"]["current"]["current_indexed_documents"],
        if append { 129 } else { 128 }
    );
    assert_eq!(second.job["generation_changed"], append);
    let late_generation = second.job["published_generation"]
        .as_str()
        .context("late generation")?;
    assert_eq!(late_generation != warm_generation.as_str(), append);
    // Earlier callers remain bound to the earlier terminal, even after the
    // current pointer advances for the late request.
    for id in [&first_id, &peer_id] {
        assert_eq!(
            engine.status(id).unwrap()["published_generation"],
            warm_generation.as_str()
        );
    }
    assert!(!engine.has_pending_request());
    assert_eq!(fixture.captures(), 2);
    assert_eq!(
        first.job["progress"]["processed_bytes"],
        fixture.input_bytes
    );
    fixture.assert_capture(&second.job, if append { 129 } else { 128 })?;
    Ok(())
}

#[test]
fn discovered_late_callers_require_a_new_capture_and_keep_earlier_pins() -> Result<()> {
    assert_late_publication(false)?;
    assert_late_publication(true)
}
#[test]
fn background_marker_recovery_progresses_without_caller_replay() -> Result<()> {
    let fixture = DiscoveredFixture::new()?;
    let id = Uuid::now_v7().to_string();
    let response = fixture
        .engine
        .handle_ipc_request(
            fixture.data_root.path(),
            &json!({
                "op":SOURCE_REFRESH_REQUEST_OP,"mode":"background","trigger":"search",
                "request_id":id,"refresh_intent":{"kind":"automatic_maintenance"}
            }),
        )?
        .context("marker admission")?;
    assert_eq!(response["ok"], true);
    let writes = fixture.journal.writes.lock().unwrap();
    assert_eq!(writes.len(), 2);
    let marker = writes[0].clone();
    drop(writes);
    assert_eq!(marker["request_id"], id);
    assert_eq!(
        marker["admission_durability"],
        "replacement_visible_or_indeterminate"
    );
    let fingerprint = marker["request_fingerprint"].clone();
    drop(fixture.engine);
    // Restore the actual first persisted image to exercise restart before the
    // marker-clear write. This is journal recovery, not power-loss simulation.
    DaemonRefreshJournal::default().store(fixture.data_root.path(), &marker)?;
    let restarted = CoreRefreshEngine(ctx_history_refresh::RefreshEngine::new(
        fixture.journal.clone(),
        fixture.runtime.clone(),
    ));
    assert!(restarted.recover(fixture.data_root.path())?);
    let recovered = restarted.status(&id).context("recovered marker identity")?;
    assert_eq!(recovered["request_fingerprint"], fingerprint);
    assert_eq!(recovered["request_state"], "admission_pending");
    let run = restarted
        .run_next(fixture.data_root.path())
        .context("marker recovery")?;
    assert!(!run.failed, "{:#}", run.job);
    assert!(!restarted.has_pending_request());
    assert_eq!(run.job["request_id"], id);
    assert_eq!(run.job["published_generation"], fixture.generation);
    assert_eq!(restarted.status(&id).unwrap()["request_state"], "published");
    assert_eq!(
        fixture
            .runtime
            .captures
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    Ok(())
}
