use super::*;
use ctx_history_capture_runtime::{CaptureLifecycleOpenOutcome, CaptureLifecycleSink};

#[test]
fn newline_record_rejections_resume_after_malformed_and_oversized_rows() {
    for oversized in [false, true] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir_all(&root).unwrap();
        let mut fixture = br#"{"message":"first"}
"#
        .to_vec();
        if oversized {
            fixture.extend_from_slice(br#"{"message":"#);
            fixture.extend(std::iter::repeat_n(b'x', MAX_PROVIDER_JSONL_LINE_BYTES));
            fixture.extend_from_slice(b"\"}\n");
        } else {
            fixture.extend_from_slice(b"{\n");
        }
        fixture.extend_from_slice(b"{\"message\":\"last\"}\n");
        fs::write(root.join("boundaries.jsonl"), fixture).unwrap();

        let adapter = RecordRejectionTestAdapter;
        let inventory = adapter.discover(&root).unwrap();
        let leaf = inventory.accepted_leaves().next().unwrap();
        let writer = match TestLifecycle::open(&temp.path().join("index"), ()).unwrap() {
            CaptureLifecycleOpenOutcome::Ready(writer) => writer,
            CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
        };
        let mut emitted = 0;
        let mut emit = |event| {
            match event {
                JsonlLeafOutputEvent::Page { records, .. } => emitted += records.len(),
                JsonlLeafOutputEvent::Record { .. } => emitted += 1,
                JsonlLeafOutputEvent::Flush => {}
            }
            Ok(())
        };
        let mut output = JsonlLeafOutput::new(&mut emit);
        let mut worker = JsonlFamilyWorkerContext::default();
        let prepared = prepare_leaf(
            &adapter,
            leaf,
            None,
            &writer.base_event_identity_lookup(),
            &mut worker,
            &mut output,
            true,
        )
        .unwrap();

        assert_eq!(emitted, 2);
        let counts = prepared.certificate.counts();
        assert_eq!(counts.complete_records, 3);
        assert_eq!(counts.retained_records, 2);
        assert_eq!(counts.rejected_records, 1);
        assert_eq!(counts.ignored_records, 0);
        let (rejections, omitted) = prepared.record_rejections.into_parts();
        assert_eq!(omitted, 0);
        assert_eq!(rejections.len(), 1);
        assert_eq!(rejections[0].line_number, 2);
        assert_eq!(
            rejections[0].class,
            SourceBackedRecordRejectionClass::MalformedRecord
        );
    }
}

#[test]
fn rejected_leaf_exact_proof_rejects_change_since_discovery() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let transcript = root.join("rejected.jsonl");
    fs::write(&transcript, TEST_RECORD).unwrap();
    let inventory = TestAdapter.discover(&root).unwrap();
    let leaf = inventory.accepted_leaves().next().unwrap();
    fs::write(&transcript, b"{\"message\":\"repaired\"}\n").unwrap();

    let error = JsonlFamilyTerminalProof::exact_admitted_path(
        leaf.source_path.clone(),
        Arc::clone(&leaf.authority),
        leaf.authority_path.clone(),
        leaf.observation(),
    )
    .expect_err("changed rejected member must not receive an exact terminal proof");
    assert!(matches!(error, SourceIoError::SourceChangedDuringCapture));
}

#[test]
fn semantic_retry_restarts_as_replacement_before_emission_and_reports_shared_progress() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let transcript = root.join("semantic.jsonl");
    fs::write(&transcript, TEST_RECORD).unwrap();
    let observations = Arc::new(Mutex::new(SemanticLifecycleObservations::default()));
    let adapter = SemanticLifecycleTestAdapter {
        behavior: SemanticLifecycleBehavior::RetryAppend,
        observations: Arc::clone(&observations),
    };
    let index_root = temp.path().join("index");
    let cold = prepare_semantic_lifecycle_test(&adapter, &root, &index_root, None, &mut Vec::new())
        .unwrap();

    OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(TEST_RECORD)
        .unwrap();
    let mut publications = Vec::new();
    let replaced = prepare_semantic_lifecycle_test(
        &adapter,
        &root,
        &index_root,
        Some(&cold.certificate),
        &mut publications,
    )
    .unwrap();

    assert!(replaced.append.is_none());
    assert_eq!(replaced.certificate.counts().complete_records, 2);
    assert_eq!(
        publications,
        vec![
            (false, TEST_RECORD.len() as u64, 0),
            (false, TEST_RECORD.len() as u64, 0),
        ],
        "replacement retry must emit only replacement pages with shared-owned byte progress"
    );
    let observations = observations.lock().unwrap();
    assert_eq!(
        observations.constructed_modes,
        [
            JsonlFamilyProjectionMode::Cold,
            JsonlFamilyProjectionMode::CertifiedAppend,
            JsonlFamilyProjectionMode::Replacement,
        ]
    );
    assert_eq!(observations.preflight_modes, observations.constructed_modes);
    assert!(!observations
        .page_modes
        .contains(&JsonlFamilyProjectionMode::CertifiedAppend));
    assert_eq!(
        observations.finished_modes,
        [
            JsonlFamilyProjectionMode::Cold,
            JsonlFamilyProjectionMode::Replacement,
        ],
        "the pre-emission append executor must be discarded without finalization"
    );
}

#[test]
fn semantic_classification_cannot_exceed_shared_physical_records() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("semantic.jsonl"), TEST_RECORD).unwrap();
    let adapter = SemanticLifecycleTestAdapter {
        behavior: SemanticLifecycleBehavior::Overclassify,
        observations: Arc::new(Mutex::new(SemanticLifecycleObservations::default())),
    };
    let error = match prepare_semantic_lifecycle_test(
        &adapter,
        &root,
        &temp.path().join("index"),
        None,
        &mut Vec::new(),
    ) {
        Ok(_) => panic!("overclassified semantic scan unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("semantic classified count exceeds physical records"));
}

#[test]
fn semantic_executor_cannot_finalize_before_shared_terminal_input() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("semantic.jsonl"), TEST_RECORD).unwrap();
    let observations = Arc::new(Mutex::new(SemanticLifecycleObservations::default()));
    let adapter = SemanticLifecycleTestAdapter {
        behavior: SemanticLifecycleBehavior::StopBeforeTerminal,
        observations: Arc::clone(&observations),
    };
    let error = match prepare_semantic_lifecycle_test(
        &adapter,
        &root,
        &temp.path().join("index"),
        None,
        &mut Vec::new(),
    ) {
        Ok(_) => panic!("unterminated semantic scan unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("semantic scan has no terminal checkpoint"));
    assert_eq!(
        observations.lock().unwrap().finished_modes,
        [JsonlFamilyProjectionMode::Cold],
        "semantic finalization runs, but shared terminal authority still gates certification"
    );
}

#[test]
fn optimized_leaf_execution_keeps_publication_inside_the_shared_family() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("optimized.jsonl"), TEST_RECORD).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: false,
        emit_progress_records: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.accepted_leaves().next().unwrap();
    let writer = match TestLifecycle::open(&temp.path().join("index"), ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(writer) => writer,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
    };
    let mut publications = Vec::new();
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emit = |event| {
        if let JsonlLeafOutputEvent::Page {
            append, records, ..
        } = event
        {
            publications.push((append, records.len()));
        }
        Ok(())
    };
    let mut output = JsonlLeafOutput::new(&mut emit);
    let prepared = prepare_leaf(
        &adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut output,
        true,
    )
    .unwrap();

    assert_eq!(adapter.scans.load(Ordering::SeqCst), 1);
    assert_eq!(publications, vec![(false, 0)]);
    assert!(prepared.append.is_none());
    assert!(matches!(
        prepared.terminal_proof,
        JsonlFamilyTerminalProof::ExactFile { .. }
    ));
    assert_eq!(
        prepared.certificate.parser_revision(),
        adapter.parser_revision()
    );
}

#[test]
fn single_leaf_serial_jsonl_page_accounts_sessions_messages_and_tool_calls() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("progress.jsonl"), PROGRESS_TEST_RECORDS).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: false,
        emit_progress_records: true,
    };
    let resident = Mutex::new(FamilyResident::default());
    let mut writer = match IndexCaptureLifecycle::open(&temp.path().join("index"), ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => {
            panic!("serial progress test lifecycle unexpectedly requires recovery")
        }
    };
    let mut owners = HashMap::new();
    let mut complete_inventories = Vec::new();
    let mut logical_source_failures = SourceBackedLogicalSourceFailures::default();
    let mut record_rejections = SourceBackedRecordRejections::default();
    let mut applied_removals = Vec::new();
    let mut history_progress = AttemptHistoryProgress::default();
    let mut report_progress = |delta| {
        history_progress.advance(&delta);
        Ok(())
    };
    let mut sink = SourceBackedGenerationSink {
        core_record_preparer: writer.core_preparation(),
        lifecycle: &mut writer,
        owners: &mut owners,
        complete_inventories: &mut complete_inventories,
        route_index: 0,
        route_identity: test_route_identity(),
        base_route_aliases: BTreeSet::new(),
        base_route_control: None,
        resources: SourceBackedRouteResources::production(1),
        logical_source_failures: &mut logical_source_failures,
        record_rejections: &mut record_rejections,
        applied_removals: &mut applied_removals,
        record_progress: Some(&mut report_progress),
        current_source_progress: None,
        intermediate_progress_last_emitted_at: None,
        intermediate_progress_pending_stage: None,
        last_progress_session_id: None,
        exact_scan_total_bytes: None,
        exact_scan_accounting_enabled: false,
    };

    with_family_scanner_workers(1, || {
        capture(&adapter, &root, &resident, &mut sink).unwrap();
    });
    drop(sink);

    assert_eq!(
        history_progress.snapshot(),
        ctx_history_capture_model::AttemptHistoryProgressSnapshot {
            processed_sessions: 1,
            processed_messages: 2,
            processed_tool_calls: 1,
            processed_bytes: PROGRESS_TEST_RECORDS.len() as u64,
        },
        "the true one-leaf serial page path must preserve Core-record progress semantics"
    );
}

fn optimized_test_certificate(
    adapter: &JsonlFamilyAdapterObject,
    leaf: &JsonlFamilyLeaf,
    content_digest: [u8; 32],
) -> CertifiedSource {
    let observation =
        super::scanner::source_observation::<CaptureError>(leaf.source(), leaf.observation())
            .unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        adapter.parser_revision(),
        content_digest,
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 0,
            rejected_records: 0,
            ignored_records: 1,
            indexed_documents: 0,
            certified_bytes: TEST_RECORD.len() as u64,
        },
    )
    .unwrap()
}

#[test]
fn active_source_family_contract_jsonl_optimized_proof_rejects_cross_leaf_binding() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("optimized.jsonl"), TEST_RECORD).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: false,
        emit_progress_records: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let first = inventory.accepted_leaves().next().unwrap();
    let other_source = SourceKey::derive(
        adapter.provider().as_str(),
        TEST_SOURCE_FORMAT,
        TEST_SCHEMA,
        1,
        SourceAnchor::provider_native(
            "terminal-witness-file",
            TypedKey::utf8("other-optimized-leaf").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let other = JsonlFamilyLeaf::bind_observed(
        other_source,
        first.source_path.clone(),
        Arc::clone(&first.authority),
        first.authority_path.clone(),
        first.binding.clone(),
        first.observation.clone(),
    );
    let first_certificate =
        optimized_test_certificate(&adapter, first, Sha256::digest(TEST_RECORD).into());
    let other_certificate =
        optimized_test_certificate(&adapter, &other, Sha256::digest(TEST_RECORD).into());
    let proof = JsonlFamilyTerminalProof::exact_file(&adapter, first, &first_certificate).unwrap();
    let outcome = JsonlFamilyOptimizedLeafOutcome::replacement(other_certificate, proof);

    let error = super::leaf::validate_optimized_outcome(&adapter, &other, None, outcome)
        .err()
        .expect("proof from another optimized leaf must be rejected");
    assert!(error
        .to_string()
        .contains("bound to another leaf or certificate"));
}

#[test]
fn active_source_family_contract_jsonl_optimized_proof_rejects_mismatched_certificate() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("optimized.jsonl"), TEST_RECORD).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: false,
        emit_progress_records: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.accepted_leaves().next().unwrap();
    let certificate =
        optimized_test_certificate(&adapter, leaf, Sha256::digest(TEST_RECORD).into());
    let mismatched = optimized_test_certificate(&adapter, leaf, [9; 32]);
    let proof = JsonlFamilyTerminalProof::exact_file(&adapter, leaf, &certificate).unwrap();
    let outcome = JsonlFamilyOptimizedLeafOutcome::replacement(mismatched, proof);

    let error = super::leaf::validate_optimized_outcome(&adapter, leaf, None, outcome)
        .err()
        .expect("proof from another certificate must be rejected");
    assert!(error
        .to_string()
        .contains("bound to another leaf or certificate"));
}

#[test]
fn optimized_leaf_execution_rejects_records_owned_by_another_source() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("optimized.jsonl"), TEST_RECORD).unwrap();
    let adapter = OptimizedLeafTestAdapter {
        scans: AtomicUsize::new(0),
        emit_wrong_source: true,
        emit_progress_records: false,
    };
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.accepted_leaves().next().unwrap();
    let writer = match TestLifecycle::open(&temp.path().join("index"), ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(writer) => writer,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
    };
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emit = |_event| Ok(());
    let mut output = JsonlLeafOutput::new(&mut emit);
    let error = prepare_leaf(
        &adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut output,
        true,
    )
    .err()
    .expect("wrong-source optimized emission must fail");
    assert!(error
        .to_string()
        .contains("optimized JSONL leaf emitted a record for another source"));
}

fn project_framing_policy_fixture(
    adapter: &JsonlFamilyAdapterObject,
    root: &Path,
    index: &Path,
) -> CertifiedSource {
    let inventory = adapter.discover(root).unwrap();
    let leaf = inventory.accepted_leaves().next().unwrap();
    let writer = match TestLifecycle::open(index, ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(writer) => writer,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
    };
    let mut worker = JsonlFamilyWorkerContext::default();
    let mut emit = |_event| Ok(());
    let mut output = JsonlLeafOutput::new(&mut emit);
    prepare_leaf(
        adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut output,
        true,
    )
    .unwrap()
    .certificate
}

fn assert_framing_policy_fixture(
    message: &str,
    record_framing: JsonlRecordFraming,
    includes_terminal_padding: bool,
) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let record = format!(r#"{{"message":"{message}"}}"#).into_bytes();
    let mut fixture = record.clone();
    fixture.push(b'\n');
    fixture.extend_from_slice(&[0; 8]);
    fs::write(root.join("framing.jsonl"), &fixture).unwrap();
    let projected = Arc::new(Mutex::new(Vec::new()));
    let adapter = FramingPolicyTestAdapter {
        projected: Arc::clone(&projected),
        record_framing,
    };
    let certificate = project_framing_policy_fixture(&adapter, &root, &temp.path().join("index"));
    let expected = if includes_terminal_padding {
        vec![record, vec![0; 8]]
    } else {
        vec![record]
    };
    assert_eq!(projected.lock().unwrap().as_slice(), expected);
    let expected_count = u64::try_from(expected.len()).unwrap();
    assert_eq!(certificate.counts().complete_records, expected_count);
    assert_eq!(certificate.counts().ignored_records, expected_count);
    assert_eq!(
        certificate.counts().certified_bytes,
        if includes_terminal_padding {
            fixture.len() as u64
        } else {
            (fixture.len() - 8) as u64
        }
    );
}

#[test]
fn adapter_record_framing_defaults_to_ordinary_tail_compatibility() {
    assert_framing_policy_fixture("ordinary", JsonlRecordFraming::ordinary(), false);
}

#[test]
fn adapter_record_framing_can_select_terminal_nul_padding() {
    assert_framing_policy_fixture(
        "terminal",
        JsonlRecordFraming::terminal_nul_padded(MAX_PROVIDER_JSONL_LINE_BYTES),
        true,
    );
}

#[test]
fn generic_projection_streams_record_and_finish_fanout_before_record_65() {
    for finish_only in [false, true] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("fanout.jsonl"), TEST_RECORD).unwrap();
        let admitted = Arc::new(AtomicUsize::new(0));
        let observed_before_65 = Arc::new(AtomicUsize::new(usize::MAX));
        let adapter = EmissionTestAdapter {
            project_fanout: if finish_only { 0 } else { 129 },
            finish_fanout: if finish_only { 129 } else { 0 },
            admitted: Some(Arc::clone(&admitted)),
            observed_before_65: Some(Arc::clone(&observed_before_65)),
        };
        let inventory = adapter.discover(&root).unwrap();
        let leaf = inventory.accepted_leaves().next().unwrap();
        let writer = match TestLifecycle::open(&temp.path().join("index"), ()).unwrap() {
            CaptureLifecycleOpenOutcome::Ready(writer) => writer,
            CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
        };
        let mut emit = |event| {
            if matches!(event, JsonlLeafOutputEvent::Record { .. }) {
                admitted.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        };
        let mut output = JsonlLeafOutput::new(&mut emit);
        let mut worker = JsonlFamilyWorkerContext::default();
        let prepared = prepare_leaf(
            &adapter,
            leaf,
            None,
            &writer.base_event_identity_lookup(),
            &mut worker,
            &mut output,
            true,
        )
        .unwrap();

        assert_eq!(admitted.load(Ordering::SeqCst), 129);
        assert_eq!(observed_before_65.load(Ordering::SeqCst), 64);
        assert_eq!(prepared.certificate.counts().indexed_documents, 129);
    }
}

fn scoped_preflight_failure_fixture(
    behavior: ScopedPreflightTestBehavior,
) -> (CaptureError, usize) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("scoped.jsonl"), TEST_RECORD).unwrap();
    let adapter = ScopedPreflightTestAdapter { behavior };
    let inventory = adapter.discover(&root).unwrap();
    let leaf = inventory.accepted_leaves().next().unwrap();
    let writer = match TestLifecycle::open(&temp.path().join("index"), ()).unwrap() {
        CaptureLifecycleOpenOutcome::Ready(writer) => writer,
        CaptureLifecycleOpenOutcome::RecoveryRequired { .. } => unreachable!(),
    };
    let admitted = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&admitted);
    let mut emit = move |event| {
        if matches!(event, JsonlLeafOutputEvent::Record { .. }) {
            observed.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    };
    let mut output = JsonlLeafOutput::new(&mut emit);
    let mut worker = JsonlFamilyWorkerContext::default();
    let error = prepare_leaf(
        &adapter,
        leaf,
        None,
        &writer.base_event_identity_lookup(),
        &mut worker,
        &mut output,
        true,
    )
    .err()
    .expect("scoped preflight fixture must fail");
    (error, admitted.load(Ordering::SeqCst))
}

#[test]
fn preflight_wrong_source_and_generic_internal_claims_remain_fatal() {
    let (wrong_source, admitted) =
        scoped_preflight_failure_fixture(ScopedPreflightTestBehavior::WrongSource);
    assert_eq!(admitted, 0);
    assert!(wrong_source
        .to_string()
        .contains("JSONL projector failed another logical source"));

    let (generic, admitted) =
        scoped_preflight_failure_fixture(ScopedPreflightTestBehavior::GenericInternal);
    assert_eq!(admitted, 0);
    assert!(generic.to_string().contains("generic preflight failure"));
}

#[test]
fn failure_after_staging_cannot_be_reclassified_as_source_local() {
    let (error, admitted) =
        scoped_preflight_failure_fixture(ScopedPreflightTestBehavior::PostStagingFailure);
    assert!(admitted > 0, "fixture must cross the staging boundary");
    assert!(error.to_string().contains("post-staging generic failure"));
}
