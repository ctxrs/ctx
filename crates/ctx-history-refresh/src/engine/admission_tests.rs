use super::*;

fn private_data_root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("temporary data root");
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root)
        .expect("private data root");
    (temp, data_root)
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

#[derive(Debug)]
struct RetainingAdmissionJournal {
    job: Mutex<Option<Value>>,
    store_before_ack_calls: std::sync::atomic::AtomicUsize,
    message: &'static str,
}

impl RetainingAdmissionJournal {
    fn once(message: &'static str) -> Self {
        Self {
            job: Mutex::new(None),
            store_before_ack_calls: std::sync::atomic::AtomicUsize::new(0),
            message,
        }
    }
}

impl RefreshJournal for RetainingAdmissionJournal {
    fn load(&self, _data_root: &Path) -> Result<Option<Value>> {
        Ok(self.job.lock().unwrap().clone())
    }

    fn store(&self, _data_root: &Path, value: &Value) -> Result<()> {
        *self.job.lock().unwrap() = Some(value.clone());
        Ok(())
    }

    fn store_before_ack(&self, data_root: &Path, value: &Value) -> DurableAdmissionPersistence {
        if let Err(error) = self.store(data_root, value) {
            return DurableAdmissionPersistence::Failed(error);
        }
        let call = self
            .store_before_ack_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            DurableAdmissionPersistence::Retained(anyhow!(self.message))
        } else {
            DurableAdmissionPersistence::Confirmed
        }
    }
}

#[test]
fn listener_ack_is_durable_before_admission_discovery_can_start() {
    let (_temp, data_root) = private_data_root();
    let journal = Arc::new(TestRefreshJournal::default());
    let fence_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_calls = Arc::clone(&fence_calls);
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
        Arc::new(move |_discovery, _journal, _data_root, _catalog| {
            observed_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(BTreeMap::new())
        }),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000294";

    let admission = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let (response, response_barrier) = admission.into_parts();
    let response = response.schema_v1_fields();

    assert_eq!(response["request_id"], request_id);
    assert_eq!(response["request_state"], "admission_pending");
    assert_eq!(
        response["disconnect_policy"],
        "retain_after_durable_admission"
    );
    assert_eq!(fence_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let durable = journal
        .load(&data_root)
        .unwrap()
        .expect("durable admission before acknowledgement");
    assert_eq!(durable["request_id"], request_id);
    assert_eq!(durable["request_state"], "admission_pending");

    assert!(!coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(fence_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    response_barrier
        .expect("pending admission response barrier")
        .release(&coordinator);
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(fence_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        status_value(&coordinator, request_id)["request_state"],
        "queued"
    );
}

#[test]
fn failed_durable_admission_rolls_back_the_reserved_request() {
    let (_temp, data_root) = private_data_root();
    let coordinator = CoreRefreshEngine::new(
        Arc::new(TestRefreshJournal::failing_before_ack()),
        test_refresh_runtime(),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000295";

    let error = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap_err();

    assert!(format!("{error:#}").contains("persist durable source refresh admission"));
    assert!(coordinator.status(request_id).is_none());
    assert!(!coordinator.has_pending_request());
    assert!(!coordinator.has_pending_admission());
}

#[test]
fn post_replacement_chmod_error_retains_and_acknowledges_the_request() {
    let (_temp, data_root) = private_data_root();
    let journal = Arc::new(RetainingAdmissionJournal::once(
        "injected post-replacement chmod failure",
    ));
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
        Arc::new(|_discovery, _journal, _data_root, _catalog| Ok(BTreeMap::new())),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000298";

    let admission = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let (response, response_barrier) = admission.into_parts();

    assert_retained_acknowledgement(response.schema_v1_fields(), request_id);
    assert!(coordinator.has_pending_admission());
    let retained = journal
        .load(&data_root)
        .unwrap()
        .expect("replacement-visible admission retains its durability marker");
    assert_eq!(retained["request_id"], request_id);
    assert_retained_durability_fields(&retained);
    let replay = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let (replay, replay_barrier) = replay.into_parts();
    assert_reconfirmed_acknowledgement(replay.schema_v1_fields(), request_id);
    response_barrier
        .expect("retained admission response barrier")
        .release(&coordinator);
    replay_barrier
        .expect("replayed admission response barrier")
        .release(&coordinator);
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(
        status_value(&coordinator, request_id)["request_state"],
        "queued"
    );
}

#[test]
fn post_replacement_parent_sync_error_retains_and_acknowledges_the_request() {
    let (_temp, data_root) = private_data_root();
    let journal = Arc::new(RetainingAdmissionJournal::once(
        "injected durable admission parent sync failure",
    ));
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
        Arc::new(|_discovery, _journal, _data_root, _catalog| Ok(BTreeMap::new())),
    );
    let request_id = "019fcaaa-0000-7000-8000-000000000299";

    let admission = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let (response, response_barrier) = admission.into_parts();

    assert_retained_acknowledgement(response.schema_v1_fields(), request_id);
    assert!(coordinator.has_pending_admission());
    let retained = journal
        .load(&data_root)
        .unwrap()
        .expect("parent-sync failure retains its durability marker");
    assert_eq!(retained["request_id"], request_id);
    assert_retained_durability_fields(&retained);
    let replay = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let (replay, replay_barrier) = replay.into_parts();
    assert_reconfirmed_acknowledgement(replay.schema_v1_fields(), request_id);
    response_barrier
        .expect("retained admission response barrier")
        .release(&coordinator);
    replay_barrier
        .expect("replayed admission response barrier")
        .release(&coordinator);
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(
        status_value(&coordinator, request_id)["request_state"],
        "queued"
    );
}

#[test]
fn same_id_replay_preserves_persistently_indeterminate_admission_durability() {
    let (_temp, data_root) = private_data_root();
    let journal = Arc::new(TestRefreshJournal::retaining_after_ack_write(
        "injected persistent post-replacement durability failure",
    ));
    let coordinator = CoreRefreshEngine::with_admission_fence_for_test(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
        Arc::new(|_discovery, _journal, _data_root, _catalog| Ok(BTreeMap::new())),
    );
    let request_id = "019fcaaa-0000-7000-8000-0000000002a2";

    let first = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let replay = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();

    assert_retained_acknowledgement(first.status().schema_v1_fields(), request_id);
    assert_retained_acknowledgement(replay.status().schema_v1_fields(), request_id);
    let durable = journal
        .load(&data_root)
        .unwrap()
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
    let coordinator = test_refresh_engine();
    let request_id = "019fcaaa-0000-7000-8000-000000000296";

    let first = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    let replay = coordinator
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    assert_eq!(replay.status(), first.status());

    let changed = RefreshSubmission::new(
        request_id.to_owned(),
        RefreshOperation::Refresh,
        None,
        SourceBackedRefreshScope::All,
        false,
        false,
    );
    let conflict = coordinator.submit(&data_root, changed).unwrap();
    let conflict = conflict.status().schema_v1_fields();
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
    let journal = Arc::new(TestRefreshJournal::default());
    let first = CoreRefreshEngine::new(
        Arc::clone(&journal) as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
    );
    let admission = first
        .submit(&data_root, test_refresh_submission(request_id))
        .unwrap();
    assert_eq!(
        admission.status().schema_v1_fields()["request_state"],
        "admission_pending"
    );
    drop(first);

    let recovered = CoreRefreshEngine::with_admission_fence_for_test(
        journal as Arc<dyn RefreshJournal>,
        test_refresh_runtime(),
        Arc::new(|_discovery, _journal, _data_root, _catalog| Ok(BTreeMap::new())),
    );
    assert!(recovered
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        status_value(&recovered, request_id)["request_state"],
        "admission_pending"
    );
    assert!(recovered
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    assert_eq!(
        status_value(&recovered, request_id)["request_state"],
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
        zero_source_authority: Vec::new(),
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
