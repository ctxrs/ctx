use super::*;

fn private_data_root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("temporary data root");
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root)
        .expect("private data root");
    (temp, data_root)
}

fn fresh_request(request_id: &str) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "op": SOURCE_REFRESH_REQUEST_OP,
        "request_id": request_id,
        "mode": "wait",
        "operation": "refresh",
        "fresh_after_admitted_snapshot": true,
    }))
}

fn assert_retained_acknowledgement(response: &Value, request_id: &str) {
    assert_eq!(response["ok"], true);
    assert_eq!(response["request_id"], request_id);
    assert_eq!(response["request_state"], "admission_pending");
    assert_eq!(
        response["disconnect_policy"],
        "retain_after_durable_admission"
    );
    assert_retained_durability_fields(response);
}

fn assert_retained_durability_fields(response: &Value) {
    assert_eq!(
        response["admission_acknowledgement"],
        "retained_after_durability_error"
    );
    assert_eq!(
        response["admission_durability"],
        "replacement_visible_or_indeterminate"
    );
}

fn assert_reconfirmed_acknowledgement(response: &Value, request_id: &str) {
    assert_eq!(response["ok"], true);
    assert_eq!(response["request_id"], request_id);
    assert!(response.get("admission_acknowledgement").is_none());
    assert!(response.get("admission_durability").is_none());
}

#[test]
fn listener_ack_is_durable_before_admission_discovery_can_start() {
    let (_temp, data_root) = private_data_root();
    let fence_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_calls = Arc::clone(&fence_calls);
    let coordinator =
        CoreRefreshEngine::with_admission_fence_for_test(Arc::new(move |_data_root, _catalog| {
            observed_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(BTreeMap::new())
        }));
    let request_id = "019fcaaa-0000-7000-8000-000000000294";

    let response = coordinator
        .handle_listener_ipc_request(&data_root, &fresh_request(request_id))
        .unwrap()
        .expect("admission response");

    assert_eq!(response["request_id"], request_id);
    assert_eq!(response["request_state"], "admission_pending");
    assert_eq!(
        response["disconnect_policy"],
        "retain_after_durable_admission"
    );
    assert_eq!(fence_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let durable = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("durable admission before acknowledgement");
    assert_eq!(durable["request_id"], request_id);
    assert_eq!(durable["request_state"], "admission_pending");

    assert!(!coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(fence_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    coordinator.finish_listener_admission_response(request_id);
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(fence_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        coordinator.status(request_id).unwrap()["request_state"],
        "queued"
    );
}

#[test]
fn failed_durable_admission_rolls_back_the_reserved_request() {
    let (_temp, data_root) = private_data_root();
    let coordinator = CoreRefreshEngine::with_status_writer_for_test(
        Arc::new(CaptureOwnedSourceBackedRefreshExecutor),
        Arc::new(|_path, _job| bail!("injected durable admission failure")),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000295";

    let error = coordinator
        .handle_listener_ipc_request(&data_root, &fresh_request(request_id))
        .unwrap_err();

    assert!(format!("{error:#}").contains("persist durable source refresh admission"));
    assert!(coordinator.status(request_id).is_none());
    assert!(!coordinator.has_pending_request());
    assert!(!coordinator.has_pending_admission());
}

#[test]
fn post_replacement_chmod_error_retains_and_acknowledges_the_request() {
    let (_temp, data_root) = private_data_root();
    let fail_once = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let injected = Arc::clone(&fail_once);
    let coordinator = CoreRefreshEngine::with_runtime(
        Arc::new(CaptureOwnedSourceBackedRefreshExecutor),
        Arc::new(|_data_root, _catalog| Ok(BTreeMap::new())),
        Arc::new(move |path, job| {
            if injected.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return crate::semantic::paths_status::write_private_json_with_chmod_fault(
                    path, job,
                );
            }
            write_daemon_job_status(path, job)
        }),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000298";

    let response = coordinator
        .handle_listener_ipc_request(&data_root, &fresh_request(request_id))
        .unwrap()
        .expect("retained admission acknowledgement");

    assert_retained_acknowledgement(&response, request_id);
    assert!(coordinator.has_pending_admission());
    let retained = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("replacement-visible admission retains its durability marker");
    assert_eq!(retained["request_id"], request_id);
    assert_retained_durability_fields(&retained);
    let replay = coordinator
        .handle_listener_ipc_request(&data_root, &fresh_request(request_id))
        .unwrap()
        .expect("same-ID replay reconfirms admission durability");
    assert_reconfirmed_acknowledgement(&replay, request_id);
    coordinator.finish_listener_admission_response(request_id);
    coordinator.finish_listener_admission_response(request_id);
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(
        coordinator.status(request_id).unwrap()["request_state"],
        "queued"
    );
}

#[test]
fn post_replacement_parent_sync_error_retains_and_acknowledges_the_request() {
    let (_temp, data_root) = private_data_root();
    let coordinator =
        CoreRefreshEngine::with_admission_fence_for_test(Arc::new(|_data_root, _catalog| {
            Ok(BTreeMap::new())
        }));
    let request_id = "019fcaaa-0000-7000-8000-000000000299";
    super::durable_queue::fail_next_admission_parent_sync_for_test();

    let response = coordinator
        .handle_listener_ipc_request(&data_root, &fresh_request(request_id))
        .unwrap()
        .expect("retained admission acknowledgement");

    assert_retained_acknowledgement(&response, request_id);
    assert!(coordinator.has_pending_admission());
    let retained = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("parent-sync failure retains its durability marker");
    assert_eq!(retained["request_id"], request_id);
    assert_retained_durability_fields(&retained);
    let replay = coordinator
        .handle_listener_ipc_request(&data_root, &fresh_request(request_id))
        .unwrap()
        .expect("same-ID replay reconfirms admission durability");
    assert_reconfirmed_acknowledgement(&replay, request_id);
    coordinator.finish_listener_admission_response(request_id);
    coordinator.finish_listener_admission_response(request_id);
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(
        coordinator.status(request_id).unwrap()["request_state"],
        "queued"
    );
}

#[test]
fn same_id_replay_preserves_persistently_indeterminate_admission_durability() {
    let (_temp, data_root) = private_data_root();
    let coordinator = CoreRefreshEngine::with_status_writer_for_test(
        Arc::new(CaptureOwnedSourceBackedRefreshExecutor),
        Arc::new(crate::semantic::paths_status::write_private_json_with_chmod_fault),
    );
    let request_id = "019fcaaa-0000-7000-8000-0000000002a2";
    let request = fresh_request(request_id);

    let first = coordinator
        .handle_listener_ipc_request(&data_root, &request)
        .unwrap()
        .expect("retained first admission acknowledgement");
    let replay = coordinator
        .handle_listener_ipc_request(&data_root, &request)
        .unwrap()
        .expect("retained same-ID replay acknowledgement");

    assert_retained_acknowledgement(&first, request_id);
    assert_retained_acknowledgement(&replay, request_id);
    let durable = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("replacement-visible durable admission");
    assert_eq!(durable["request_id"], request_id);
    assert_eq!(
        durable["admission_acknowledgement"],
        "retained_after_durability_error"
    );
    assert_eq!(
        durable["admission_durability"],
        "replacement_visible_or_indeterminate"
    );
}

#[test]
fn stable_request_id_replay_requires_the_exact_same_payload() {
    let (_temp, data_root) = private_data_root();
    let coordinator = CoreRefreshEngine::new();
    let request_id = "019fcaaa-0000-7000-8000-000000000296";
    let request = fresh_request(request_id);

    let first = coordinator
        .handle_ipc_request(&data_root, &request)
        .unwrap()
        .expect("first response");
    let replay = coordinator
        .handle_ipc_request(&data_root, &request)
        .unwrap()
        .expect("idempotent replay");
    assert_eq!(replay, first);

    let mut changed = request;
    changed["fresh_after_admitted_snapshot"] = Value::Bool(false);
    let conflict = coordinator
        .handle_ipc_request(&data_root, &changed)
        .unwrap()
        .expect("typed request conflict");
    assert_eq!(conflict["ok"], false);
    assert_eq!(conflict["request_id"], request_id);
    assert_eq!(conflict["request_state"], "request_conflict");
    assert_eq!(conflict["error_code"], "request_id_conflict");
    assert_eq!(conflict["retryable"], false);
}

#[test]
fn restart_recovers_and_resumes_a_durable_pending_admission() {
    let (_temp, data_root) = private_data_root();
    let request_id = "019fcaaa-0000-7000-8000-000000000297";
    let first = CoreRefreshEngine::new();
    let response = first
        .handle_listener_ipc_request(&data_root, &fresh_request(request_id))
        .unwrap()
        .expect("durable admission response");
    assert_eq!(response["request_state"], "admission_pending");
    drop(first);

    let recovered =
        CoreRefreshEngine::with_admission_fence_for_test(Arc::new(|_data_root, _catalog| {
            Ok(BTreeMap::new())
        }));
    assert!(recovered
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        recovered.status(request_id).unwrap()["request_state"],
        "admission_pending"
    );
    assert!(recovered
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(
        recovered.status(request_id).unwrap()["request_state"],
        "queued"
    );
}

#[test]
fn crash_restart_preserves_a_logical_successors_non_null_generation_baseline() {
    let generation_zero = "10".repeat(32);
    let generation_one = "21".repeat(32);
    let mut predecessor = new_refresh_attempt(
        Some(generation_zero.clone()),
        SourceRefreshRuntimeMetadata::periodic(),
        None,
        SourceBackedRefreshScope::All,
    );
    predecessor.request_id = "019fcaaa-0000-7000-8000-0000000002a0".to_owned();
    predecessor.state = SourceBackedRefreshState::Published;
    predecessor.published_generation = Some(generation_one.clone());
    let mut successor = new_refresh_attempt(
        Some(generation_zero.clone()),
        SourceRefreshRuntimeMetadata::default(),
        None,
        SourceBackedRefreshScope::All,
    );
    successor.request_id = "019fcaaa-0000-7000-8000-0000000002a1".to_owned();
    successor.fresh_after_admitted_snapshot = true;
    let mut interrupted = predecessor.job_json();
    interrupted["queued_successors"] = Value::Array(vec![successor.job_json()]);

    let mut recovered = recover_queued_successors(&interrupted)
        .expect("recover successor after predecessor pointer publication")
        .pop()
        .expect("persisted logical successor");
    recovered.state = SourceBackedRefreshState::Published;
    recovered.published_generation = Some(generation_one.clone());
    recovered.receipt = Some(SourceBackedRefreshReceipt {
        previous_generation: recovered.previous_generation.clone(),
        published_generation: generation_one,
        generation_changed: true,
        published_explicit_source_catalog: None,
        current: SourceBackedRefreshCurrent::default(),
        route_results: Vec::new(),
        catalog_route_bindings: Vec::new(),
    });

    let terminal = recovered.to_json();
    assert_eq!(terminal["previous_generation"], generation_zero);
    assert_eq!(terminal["generation_changed"], true);
    assert_eq!(
        terminal["receipt"]["previous_generation"],
        terminal["previous_generation"]
    );
}
