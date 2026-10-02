use super::super::observation_recovery::SourceRefreshObservationRecoveryFailed;
use super::super::transport_recovery_tests::foreground_transport_fixture;
use super::*;
use crate::query_service::{write_daemon_service_endpoint, DaemonIpcService, DaemonQueryEndpoint};
use ctx_history_refresh::SourceBackedReconciliationDemand as Demand;
use std::{
    io::{Read as _, Write as _},
    os::unix::net::UnixListener,
    sync::Mutex,
};

fn root() -> Result<tempfile::TempDir> {
    let root = tempfile::Builder::new()
        .prefix("ctx-ack-")
        .tempdir_in("/tmp")?;
    ctx_history_platform::platform_security::establish_private_data_root(root.path())?;
    Ok(root)
}

fn endpoint(root: &Path) -> DaemonQueryEndpoint {
    DaemonQueryEndpoint::Unix {
        path: root.join("worker.sock"),
        token: "0123456789abcdef0123456789abcdef".to_owned(),
    }
}

fn selected_source(root: &Path) -> Result<(tempfile::TempDir, ExplicitSourceCatalogAuthority)> {
    let source_root = tempfile::tempdir()?;
    let path = source_root.path().join("synthetic.jsonl");
    std::fs::write(
        &path,
        "{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v2\"}\n",
    )?;
    let source = ctx_history_refresh::explicit_source_for_path(root, &path, None, true)?;
    let authority = ctx_history_refresh::upsert_explicit_source(root, &source)?.authority;
    Ok((source_root, authority))
}

fn import_policy(
    authority: ExplicitSourceCatalogAuthority,
    demand: Demand,
    autostart: bool,
) -> SourceBackedRefreshRequestPolicy {
    SourceBackedRefreshRequestPolicy::import(
        RefreshSelection::ExactSource(authority),
        demand,
        autostart,
    )
}

fn assert_import_request(
    request: &Value,
    authority: &ExplicitSourceCatalogAuthority,
    demand: Demand,
) {
    assert_eq!(request["op"], SOURCE_REFRESH_REQUEST_OP);
    assert_eq!(request["mode"], "wait");
    assert_eq!(request["trigger"], "import");
    Uuid::parse_str(
        request["request_id"]
            .as_str()
            .expect("canonical client UUID"),
    )
    .unwrap();
    let mut expected = json!({
        "kind": "selected_import",
        "selection": {"kind": "exact_source", "authority": authority.to_json()},
    });
    if demand == Demand::Incremental {
        expected["reconciliation_demand"] = json!("incremental");
    }
    assert_eq!(request["refresh_intent"], expected);
}

#[test]
fn real_engine_lost_acks_beyond_three_return_terminal_without_an_extra_status_request() -> Result<()>
{
    use std::sync::atomic::{AtomicBool, Ordering};
    for demand in [Demand::Exhaustive, Demand::Incremental] {
        for (lost_acks, fail) in [(0, false), (4, false), (4, true)] {
            let root = root()?;
            let (_source_root, authority) = selected_source(root.path())?;
            let _owner = ctx_daemon_runtime::DaemonLock::acquire(root.path())?.context("owner")?;
            let listener = UnixListener::bind(root.path().join("worker.sock"))?;
            listener.set_nonblocking(true)?;
            write_daemon_service_endpoint(
                root.path(),
                DaemonIpcService::SourceRefresh,
                &endpoint(root.path()),
            )?;
            let engine = Arc::new(if fail {
                CoreRefreshEngine::with_executor(Arc::new(
                    |_: ctx_history_refresh::SourceBackedRefreshExecution<'_>| {
                        Err(anyhow!("synthetic terminal capture failure"))
                    },
                ))
            } else {
                CoreRefreshEngine::new()
            });
            let server_engine = engine.clone();
            let server_root = root.path().to_owned();
            let finished = Arc::new(AtomicBool::new(false));
            let server_finished = finished.clone();
            let server = std::thread::spawn(move || -> Result<Vec<(Value, Value)>> {
                let deadline = StdInstant::now() + StdDuration::from_secs(30);
                let mut exchanges = Vec::new();
                while exchanges.len() <= lost_acks && !server_finished.load(Ordering::Acquire) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(connection) => connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if StdInstant::now() >= deadline {
                                bail!("lost-ACK fixture exceeded its bounded wait");
                            }
                            std::thread::sleep(StdDuration::from_millis(1));
                            continue;
                        }
                        Err(error) => return Err(error.into()),
                    };
                    stream.set_read_timeout(Some(StdDuration::from_secs(2)))?;
                    stream.set_write_timeout(Some(StdDuration::from_secs(2)))?;
                    let mut bytes = Vec::new();
                    stream.read_to_end(&mut bytes)?;
                    let request: Value = serde_json::from_slice(&bytes)?;
                    assert_eq!(request["op"], SOURCE_REFRESH_REQUEST_OP);
                    let mut response = server_engine
                        .handle_ipc_request(&server_root, &request)?
                        .context("real admission")?;
                    let durable = crate::paths_status::read_daemon_job_status(
                        &crate::paths_status::daemon_source_backed_refresh_job_path(&server_root),
                    )
                    .context("durable admission before ACK loss")?;
                    assert_eq!(durable["request_id"], request["request_id"]);
                    if exchanges.len() == lost_acks {
                        assert!(server_engine.prepare_next_pending_admission(&server_root)?);
                        let run = server_engine
                            .run_next(&server_root)
                            .context("real terminal")?;
                        assert_eq!(run.failed, fail, "{:#}", run.job);
                        response = server_engine
                            .handle_ipc_request(&server_root, &request)?
                            .context("idempotent terminal")?;
                        if !fail {
                            // A newer active generation cannot replace the request's receipt/pin.
                            use ctx_history_index::{
                                GenerationWriter, SourceRouteIdentity, SourceRouteSnapshot,
                                WriterOptions,
                            };
                            let prior = pin_active_verified_generation(&server_root)?;
                            let mut routes =
                                prior.verified_index().manifest().source_routes().to_vec();
                            let added_route = SourceRouteIdentity::from_sha256("ab".repeat(32))?;
                            assert!(!routes
                                .iter()
                                .any(|route| route.route_identity() == &added_route));
                            // Retained certified sources keep their actual owning routes.
                            // Adding an empty route changes the generation without discarding proof.
                            routes.push(SourceRouteSnapshot::present(added_route, Vec::new())?);
                            let mut writer = GenerationWriter::open(
                                source_backed_index_root(&server_root),
                                WriterOptions::default(),
                            )?
                            .into_writer()
                            .map_err(crate::committed_generation_recovery_error)?;
                            writer.set_present_source_routes(routes)?;
                            let newer = writer.commit(|_| true)?;
                            assert_ne!(newer.generation_id, prior.generation_id());
                        }
                        serde_json::to_writer(&mut stream, &response)?;
                        stream.write_all(b"\n")?;
                    }
                    exchanges.push((request, response));
                    // Drop the accepted stream after durable processing, without writing an ACK.
                }
                std::fs::remove_file(crate::query_service::daemon_service_endpoint_path(
                    &server_root,
                    DaemonIpcService::SourceRefresh,
                ))?;
                Ok(exchanges)
            });
            let availability = RecordingAvailability::default();
            let result = coordinate_source_backed_refresh_with_policy(
                &availability,
                root.path(),
                SourceBackedRefreshMode::Wait,
                import_policy(authority.clone(), demand, false),
                false,
                None,
            );
            finished.store(true, Ordering::Release);
            let exchanges = server.join().expect("real engine ACK server")?;
            assert_eq!(exchanges.len(), lost_acks + 1);
            assert_import_request(&exchanges[0].0, &authority, demand);
            assert!(exchanges.windows(2).all(|pair| pair[0].0 == pair[1].0));
            assert!(exchanges
                .windows(2)
                .all(|pair| pair[0].1["request_fingerprint"] == pair[1].1["request_fingerprint"]));
            assert_eq!(exchanges[0].1["reconciliation_demand"], demand.as_str());
            assert!(!engine.has_pending_request());
            assert!(availability.0.lock().unwrap().is_empty());
            let terminal = &exchanges.last().unwrap().1;
            if fail {
                let error = result.err().context("real failure must fail")?;
                assert!(error.is::<SourceBackedRefreshTerminalError>());
                assert!(format!("{error:#}").contains("synthetic terminal capture failure"));
            } else {
                let observation = result?;
                assert_eq!(
                    observation.request_id.as_deref(),
                    terminal["request_id"].as_str()
                );
                assert_eq!(
                    observation.pin.generation_id(),
                    terminal["published_generation"].as_str().unwrap()
                );
                let receipt = observation.receipt.context("exact terminal receipt")?;
                assert_eq!(receipt.published_explicit_source_catalog, Some(authority));
                assert_eq!(
                    receipt.published_generation,
                    observation.pin.generation_id()
                );
                assert_ne!(
                    pin_active_verified_generation(root.path())?.generation_id(),
                    observation.pin.generation_id()
                );
            }
        }
    }
    Ok(())
}

#[test]
fn response_side_json_utf8_and_size_errors_are_not_retried_after_real_admission() -> Result<()> {
    for response in [
        b"{\n".to_vec(),
        vec![0xff, b'\n'],
        vec![b'x'; SOURCE_REFRESH_RESPONSE_MAX_BYTES as usize + 1],
    ] {
        let root = root()?;
        let (_source_root, authority) = selected_source(root.path())?;
        let _owner = ctx_daemon_runtime::DaemonLock::acquire(root.path())?.context("owner")?;
        let listener = UnixListener::bind(root.path().join("worker.sock"))?;
        write_daemon_service_endpoint(
            root.path(),
            DaemonIpcService::SourceRefresh,
            &endpoint(root.path()),
        )?;
        let engine = Arc::new(CoreRefreshEngine::new());
        let server_engine = engine.clone();
        let server_root = root.path().to_owned();
        let server = std::thread::spawn(move || -> Result<Value> {
            let (mut stream, _) = listener.accept()?;
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes)?;
            let request: Value = serde_json::from_slice(&bytes)?;
            server_engine
                .handle_ipc_request(&server_root, &request)?
                .context("durable admission")?;
            // Oversize rejection may close the stream while the server finishes writing.
            let _ = stream.write_all(&response);
            Ok(request)
        });
        let availability = RecordingAvailability::default();
        let error = coordinate_source_backed_refresh_with_policy(
            &availability,
            root.path(),
            SourceBackedRefreshMode::Wait,
            import_policy(authority, Demand::Exhaustive, false),
            false,
            None,
        )
        .err()
        .expect("malformed response must fail admission observation");
        let request = server.join().expect("malformed-response server")?;
        assert!(!error.is::<SourceRefreshObservationRecoveryFailed>());
        assert!(!error.is::<SourceRefreshAdmissionRecoveryFailed>());
        assert!(engine
            .status(request["request_id"].as_str().unwrap())
            .is_some());
        assert!(availability.0.lock().unwrap().is_empty());
    }
    Ok(())
}

// The previous decoder accepted exactly kind + selection for selected imports.
// Keep that old boundary independent of the new intent decoder under test.
fn legacy_import_selection(intent: &Value) -> Result<RefreshSelection> {
    let fields = intent
        .as_object()
        .context("legacy intent is not an object")?;
    match fields.get("kind").and_then(Value::as_str) {
        Some("selected_import") if fields.len() == 2 => RefreshSelection::from_json(
            fields
                .get("selection")
                .context("legacy import has no selection")?,
        ),
        _ => bail!("legacy selected import intent is malformed"),
    }
}

#[test]
fn strict_legacy_owner_rejects_incremental_once_and_accepts_ordinary_exhaustive() -> Result<()> {
    for demand in [Demand::Exhaustive, Demand::Incremental] {
        let root = root()?;
        let (_source_root, authority) = selected_source(root.path())?;
        let engine = Arc::new(CoreRefreshEngine::new());
        let server_engine = engine.clone();
        let server_root = root.path().to_owned();
        let availability = RecordingAvailability::default();
        let (result, exchanges) = foreground_transport_fixture(
            root.path(),
            move |request| {
                assert_eq!(request["op"], SOURCE_REFRESH_REQUEST_OP);
                if let Err(error) = legacy_import_selection(&request["refresh_intent"]) {
                    return Ok(json!({
                        "ok": false, "schema_version": 1, "owner": "daemon",
                        "request_id": request["request_id"], "error": error.to_string(),
                    }));
                }
                server_engine
                    .handle_ipc_request(&server_root, request)?
                    .context("legacy admission")?;
                assert!(server_engine.prepare_next_pending_admission(&server_root)?);
                assert!(
                    !server_engine
                        .run_next(&server_root)
                        .context("legacy terminal")?
                        .failed
                );
                server_engine
                    .handle_ipc_request(&server_root, request)?
                    .context("legacy terminal response")
            },
            || {
                coordinate_source_backed_refresh_with_policy(
                    &availability,
                    root.path(),
                    SourceBackedRefreshMode::Wait,
                    import_policy(authority.clone(), demand, false),
                    false,
                    None,
                )
            },
        )?;
        assert_eq!(
            exchanges.len(),
            1,
            "no retry, status query or exhaustive fallback"
        );
        assert_import_request(&exchanges[0].0, &authority, demand);
        assert!(
            availability.0.lock().unwrap().is_empty(),
            "no replacement owner"
        );
        assert!(!engine.has_pending_request());
        if demand == Demand::Incremental {
            let error = result.err().context("legacy incremental rejection")?;
            assert!(format!("{error:#}").contains("legacy selected import intent is malformed"));
            assert!(!error.is::<SourceRefreshObservationRecoveryFailed>());
            assert!(!error.is::<SourceRefreshAdmissionRecoveryFailed>());
            assert!(engine
                .status(exchanges[0].0["request_id"].as_str().unwrap())
                .is_none());
        } else {
            let observation = result?;
            assert_eq!(
                observation.request_id.as_deref(),
                exchanges[0].0["request_id"].as_str()
            );
            assert_eq!(
                observation
                    .receipt
                    .context("legacy exhaustive receipt")?
                    .published_explicit_source_catalog,
                Some(authority)
            );
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeathPoint {
    Pending,
    Running,
    Terminal,
}

impl DeathPoint {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Terminal => "terminal",
        }
    }
}

fn snapshot_job(root: &Path, name: &str) -> Result<()> {
    let job = crate::paths_status::read_daemon_job_status(
        &crate::paths_status::daemon_source_backed_refresh_job_path(root),
    )
    .context("real durable child job")?;
    std::fs::write(root.join(name), serde_json::to_vec(&job)?)?;
    Ok(())
}

// This normal test is also the child entrypoint. Without its task-owned
// environment it does nothing; the parent fixture selects this exact test.
#[test]
fn finite_worker_child_fixture() -> Result<()> {
    let Some(root) = std::env::var_os("CTX_ADMISSION_FIXTURE_ROOT") else {
        return Ok(());
    };
    let root = std::path::PathBuf::from(root);
    let first = std::env::var_os("CTX_ADMISSION_FIXTURE_FIRST").is_some();
    let death_point = std::env::var("CTX_ADMISSION_FIXTURE_DEATH_POINT")?;
    let _owner = ctx_daemon_runtime::DaemonLock::acquire(&root)?.context("child owner")?;
    let engine = if first && death_point == "running" {
        CoreRefreshEngine::with_executor(Arc::new(
            |execution: ctx_history_refresh::SourceBackedRefreshExecution<'_>| -> Result<ctx_history_refresh::SourceBackedRefreshPublication> {
                snapshot_job(execution.data_root, "before-death.json")?;
                let job: Value = serde_json::from_slice(&std::fs::read(execution.data_root.join("before-death.json"))?)?;
                assert_eq!(job["request_state"], "running");
                assert_eq!(job["progress"]["phase"], "discovering");
                // Crash only after the engine has durably entered execution.
                std::process::exit(0);
            },
        ))
    } else {
        CoreRefreshEngine::new()
    };
    engine.recover_interrupted_publication(&root)?;
    if !first {
        snapshot_job(&root, "after-recovery.json")?;
    }
    let socket = root.join("worker.sock");
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    listener.set_nonblocking(true)?;
    write_daemon_service_endpoint(&root, DaemonIpcService::SourceRefresh, &endpoint(&root))?;
    let deadline = StdInstant::now() + StdDuration::from_secs(20);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && StdInstant::now() < deadline =>
            {
                std::thread::sleep(StdDuration::from_millis(1))
            }
            Err(error) => return Err(error.into()),
        }
    };
    stream.set_read_timeout(Some(StdDuration::from_secs(2)))?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    let request: Value = serde_json::from_slice(&bytes)?;
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("wire.jsonl"))?;
    serde_json::to_writer(&mut log, &request)?;
    log.write_all(b"\n")?;
    if first {
        assert_eq!(request["op"], SOURCE_REFRESH_REQUEST_OP);
        engine
            .handle_ipc_request(&root, &request)?
            .context("real child admission")?;
        if death_point != "pending" {
            assert!(engine.prepare_next_pending_admission(&root)?);
            assert!(!engine.run_next(&root).context("first terminal")?.failed);
        }
        snapshot_job(&root, "before-death.json")?;
        // Exit without unwinding or writing the ACK. The OS releases its
        // actual guard, while durable owner metadata remains unreleased.
        std::process::exit(0);
    }
    assert_eq!(
        request["op"], SOURCE_REFRESH_STATUS_OP,
        "replacement must observe before replay"
    );
    if engine.has_pending_request() {
        assert!(engine.prepare_next_pending_admission(&root)?);
        assert!(!engine.run_next(&root).context("restored terminal")?.failed);
    }
    let response = engine
        .handle_ipc_request(&root, &request)?
        .context("replacement terminal")?;
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")?;
    Ok(())
}

struct FixtureChild {
    child: std::process::Child,
    owner_id: String,
}

fn spawn_worker(root: &Path, first: bool, death_point: DeathPoint) -> Result<FixtureChild> {
    let test = concat!(module_path!(), "::finite_worker_child_fixture");
    let test = test.split_once("::").context("crate-qualified fixture")?.1;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(["--exact", test, "--nocapture"])
        .env("CTX_ADMISSION_FIXTURE_ROOT", root)
        .env_remove("CTX_ADMISSION_FIXTURE_FIRST")
        .env("CTX_ADMISSION_FIXTURE_DEATH_POINT", death_point.as_str())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit());
    if first {
        command.env("CTX_ADMISSION_FIXTURE_FIRST", "1");
    }
    let mut child = command.spawn()?;
    let deadline = StdInstant::now() + StdDuration::from_secs(5);
    loop {
        let owner =
            ctx_daemon_runtime::read_pid_lock_json(&ctx_daemon_runtime::daemon_lock_path(root));
        let endpoint = crate::query_service::read_daemon_service_endpoint_identity(
            root,
            DaemonIpcService::SourceRefresh,
        )?;
        if endpoint.is_some_and(|endpoint| endpoint.owner_pid == child.id()) {
            if let Some(owner_id) = owner
                .as_ref()
                .filter(|owner| owner["pid"] == child.id())
                .and_then(|owner| owner["owner_id"].as_str())
            {
                return Ok(FixtureChild {
                    child,
                    owner_id: owner_id.to_owned(),
                });
            }
        }
        if child.try_wait()?.is_some() || StdInstant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("child fixture did not establish its endpoint");
        }
        std::thread::sleep(StdDuration::from_millis(1));
    }
}

struct WorkerAvailability {
    root: std::path::PathBuf,
    child: Mutex<FixtureChild>,
    ensures: std::sync::atomic::AtomicUsize,
    death_point: DeathPoint,
}

impl crate::DaemonAvailabilityPort for WorkerAvailability {
    fn ensure_available(
        &self,
        root: &Path,
        _: crate::DaemonTrigger,
        _: crate::DaemonAvailabilityDemand,
    ) -> Result<crate::DaemonAvailability> {
        use std::sync::atomic::Ordering;
        assert_eq!(root, self.root);
        let call = self.ensures.fetch_add(1, Ordering::SeqCst);
        if call == 1 {
            let mut child = self.child.lock().unwrap();
            assert!(
                child.child.wait()?.success(),
                "first child must die after durable processing"
            );
            *child = spawn_worker(root, false, self.death_point)?;
        } else {
            assert_eq!(call, 0, "at most one replacement");
        }
        Ok(crate::DaemonAvailability::Available)
    }

    fn source_refresh_owner_is_live(
        &self,
        root: &Path,
        owner_id: &str,
        pid: u32,
    ) -> Result<Option<bool>> {
        let mut child = self.child.lock().unwrap();
        if root != self.root || child.owner_id != owner_id || child.child.id() != pid {
            return Ok(None);
        }
        Ok(Some(child.child.try_wait()?.is_none()))
    }
}

impl Drop for WorkerAvailability {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap();
        if !matches!(child.child.try_wait(), Ok(Some(_))) {
            let _ = child.child.kill();
        }
        let _ = child.child.wait();
    }
}

#[test]
fn real_worker_death_restores_original_status_before_any_replay() -> Result<()> {
    for demand in [Demand::Exhaustive, Demand::Incremental] {
        for death_point in [
            DeathPoint::Pending,
            DeathPoint::Running,
            DeathPoint::Terminal,
        ] {
            let root = root()?;
            let (_source_root, authority) = selected_source(root.path())?;
            let child = spawn_worker(root.path(), true, death_point)?;
            let availability = WorkerAvailability {
                root: root.path().to_owned(),
                child: Mutex::new(child),
                ensures: std::sync::atomic::AtomicUsize::new(0),
                death_point,
            };
            let observation = coordinate_source_backed_refresh_with_policy(
                &availability,
                root.path(),
                SourceBackedRefreshMode::Wait,
                import_policy(authority.clone(), demand, true),
                false,
                None,
            )?;
            assert!(
                availability.child.lock().unwrap().child.wait()?.success(),
                "replacement naturally exits"
            );
            let requests = std::fs::read_to_string(root.path().join("wire.jsonl"))?
                .lines()
                .map(serde_json::from_str::<Value>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            assert_eq!(requests.len(), 2);
            assert_import_request(&requests[0], &authority, demand);
            assert_eq!(requests[0]["op"], SOURCE_REFRESH_REQUEST_OP);
            assert_eq!(requests[1]["op"], SOURCE_REFRESH_STATUS_OP);
            assert_eq!(requests[0]["request_id"], requests[1]["request_id"]);
            let before: Value =
                serde_json::from_slice(&std::fs::read(root.path().join("before-death.json"))?)?;
            let after: Value =
                serde_json::from_slice(&std::fs::read(root.path().join("after-recovery.json"))?)?;
            assert_eq!(before["request_id"], requests[0]["request_id"]);
            assert_eq!(before["refresh_intent"], requests[0]["refresh_intent"]);
            assert_eq!(before["trigger"], "import");
            assert_eq!(before["reconciliation_demand"], demand.as_str());
            assert!(before["request_fingerprint"].as_str().is_some());
            for field in [
                "request_id",
                "refresh_intent",
                "request_fingerprint",
                "trigger",
            ] {
                assert_eq!(
                    before[field], after[field],
                    "{death_point:?}: retained {field}"
                );
            }
            let effective = if death_point == DeathPoint::Running {
                Demand::Exhaustive
            } else {
                demand
            };
            assert_eq!(
                after["reconciliation_demand"],
                effective.as_str(),
                "{death_point:?}"
            );
            assert_eq!(
                observation.request_id.as_deref(),
                requests[0]["request_id"].as_str()
            );
            assert_eq!(
                observation
                    .receipt
                    .context("replacement receipt")?
                    .published_explicit_source_catalog,
                Some(authority)
            );
            assert_eq!(
                availability
                    .ensures
                    .load(std::sync::atomic::Ordering::SeqCst),
                2,
                "initial availability plus one restoration"
            );
        }
    }
    Ok(())
}

struct ControlledAvailability {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    dead: bool,
}

impl crate::DaemonAvailabilityPort for ControlledAvailability {
    fn ensure_available(
        &self,
        _: &Path,
        _: crate::DaemonTrigger,
        _: crate::DaemonAvailabilityDemand,
    ) -> Result<crate::DaemonAvailability> {
        panic!("joined cancellation/no-start loss must not create a replacement")
    }
    fn checkpoint(&self) -> Result<()> {
        if self.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            bail!("test cancelled IPC");
        }
        Ok(())
    }
    fn source_refresh_owner_is_live(&self, _: &Path, _: &str, _: u32) -> Result<Option<bool>> {
        Ok(if self.dead { Some(false) } else { None })
    }
}

#[test]
fn real_admission_cancellation_and_exact_dead_proof_do_not_signal_a_joined_owner() -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    for dead in [false, true] {
        let root = root()?;
        let (_source_root, authority) = selected_source(root.path())?;
        let _owner =
            ctx_daemon_runtime::DaemonLock::acquire(root.path())?.context("joined owner")?;
        let listener = UnixListener::bind(root.path().join("worker.sock"))?;
        write_daemon_service_endpoint(
            root.path(),
            DaemonIpcService::SourceRefresh,
            &endpoint(root.path()),
        )?;
        let engine = Arc::new(CoreRefreshEngine::new());
        let server_engine = engine.clone();
        let server_root = root.path().to_owned();
        let cancelled = Arc::new(AtomicBool::new(false));
        let server_cancelled = cancelled.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let server_finished = finished.clone();
        let server = std::thread::spawn(move || -> Result<Value> {
            let (mut stream, _) = listener.accept()?;
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes)?;
            let request: Value = serde_json::from_slice(&bytes)?;
            server_engine
                .handle_ipc_request(&server_root, &request)?
                .context("durable joined admission")?;
            if !dead {
                server_cancelled.store(true, Ordering::SeqCst);
                // Keep the response read blocked until the cancellable IPC
                // checkpoint returns. There is no response/EOF to wake it.
                let deadline = StdInstant::now() + StdDuration::from_secs(3);
                while !server_finished.load(Ordering::SeqCst) && StdInstant::now() < deadline {
                    std::thread::sleep(StdDuration::from_millis(1));
                }
            }
            Ok(request)
        });
        let availability = ControlledAvailability { cancelled, dead };
        let result = coordinate_source_backed_refresh_with_policy(
            &availability,
            root.path(),
            SourceBackedRefreshMode::Wait,
            import_policy(authority, Demand::Exhaustive, false),
            false,
            None,
        );
        finished.store(true, Ordering::SeqCst);
        let request = server.join().expect("cancelled/dead proof fixture")?;
        let error = result.err().context("cancellation/loss must fail")?;
        if dead {
            assert!(
                error.is::<SourceRefreshObservationRecoveryFailed>(),
                "exact dead proof overrides the held guard"
            );
        } else {
            assert!(format!("{error:#}").contains("test cancelled IPC"));
        }
        assert_eq!(
            ctx_daemon_runtime::observe_pid_advisory_guard(&ctx_daemon_runtime::daemon_lock_path(
                root.path()
            )),
            Some(true)
        );
        assert!(engine
            .status(request["request_id"].as_str().unwrap())
            .is_some());
        assert!(engine.has_pending_request());
    }
    Ok(())
}
