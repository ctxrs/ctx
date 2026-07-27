use super::*;

fn checkpoint() -> KimiNativeCheckpoint {
    KimiNativeCheckpoint {
        version: KIMI_NATIVE_CURSOR_VERSION,
        route_sha256: [1; 32],
        physical_device: None,
        physical_inode: None,
        observed_file_len: 0,
        wire_revision: String::new(),
        auxiliary_revision: 0,
        admission_scope_revision: String::new(),
        complete_offset: 0,
        next_ordinal: 0,
        committed_prefix_sha256: [2; 32],
        started_at: None,
        emitted_session: false,
        accepted_events: 0,
        accepted_file_touches: 0,
        rejected_records: 0,
        rejected_outputs: 0,
        terminal: true,
        retired: false,
    }
}

fn output_observation(content_bytes: usize) -> ProOutputObservation {
    ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: "unit".to_owned(),
            native_sequence: 0,
            native_record_id: Some("record".to_owned()),
            source_record_ordinal: Some(0),
            source_record_subrecord_index: Some(0),
            byte_start: Some(0),
            byte_end_exclusive: Some(1),
        },
        occurred_at_unix_ms: Some(0),
        associations: OutputAssociations {
            direct_session_id: "session".to_owned(),
            root_session_id: "session".to_owned(),
            parent_session_id: None,
            provider_session_id: Some("session".to_owned()),
            agent_id: Some("main".to_owned()),
            repository: None,
        },
        call_id: Some("call".to_owned()),
        command: None,
        outcome: OutputOutcomeMetadata {
            outcome: OutputOutcome::Success,
            exit_code: Some(0),
            duration_ms: None,
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: KIMI_OUTPUT_LOCATOR_KIND.to_owned(),
            payload: Vec::new(),
        },
        content: vec![b'x'; content_bytes],
    }
}

#[test]
fn core_exact_and_over_eight_mib_retained_sizes_are_complete() {
    let checkpoint = checkpoint();
    let mut page = KimiCorePage {
        session_first_observed: false,
        units: vec![KimiCoreUnit::Rejection {
            line: 1,
            reason: String::new(),
        }],
    };
    let fixed = core_page_retained_bytes(&checkpoint, &page).unwrap();
    let exact_reason_bytes = NATIVE_PATH_MAX_RETAINED_PAGE_BYTES
        .checked_sub(fixed)
        .unwrap();
    match &mut page.units[0] {
        KimiCoreUnit::Rejection { reason, .. } => {
            *reason = "x".repeat(exact_reason_bytes);
        }
        _ => unreachable!(),
    }
    assert_eq!(
        core_page_retained_bytes(&checkpoint, &page).unwrap(),
        NATIVE_PATH_MAX_RETAINED_PAGE_BYTES
    );
    assert!(NativePathGroupAccounting::new(
        1,
        1,
        core_page_retained_bytes(&checkpoint, &page).unwrap()
    )
    .is_ok());
    match &mut page.units[0] {
        KimiCoreUnit::Rejection { reason, .. } => reason.push('x'),
        _ => unreachable!(),
    }
    assert_eq!(
        core_page_retained_bytes(&checkpoint, &page).unwrap(),
        NATIVE_PATH_MAX_RETAINED_PAGE_BYTES + 1
    );
    assert!(NativePathGroupAccounting::new(
        1,
        1,
        core_page_retained_bytes(&checkpoint, &page).unwrap()
    )
    .is_err());
}

#[test]
fn pro_exact_and_over_eight_mib_owned_sizes_match_shared_validation() {
    let checkpoint = checkpoint();
    let state = KimiOutputState {
        source: OutputSourceIdentity {
            provider: CaptureProvider::KimiCodeCli.as_str().to_owned(),
            namespace_id: "root".to_owned(),
            source_id: "wire".to_owned(),
        },
        source_epoch: 0,
        expected_source_epoch: None,
        expected_sink_frontier: None,
        disposition: ProOutputSourceDisposition::NewSource,
    };
    let empty = output_observation(0);
    let fixed = kimi_output_page_owned_bytes(
        &state,
        "revision",
        "materializer",
        &checkpoint,
        &[],
        Some(&empty),
    )
    .unwrap();
    let exact_content_bytes = NATIVE_INGESTION_PAGE_MAX_BYTES.checked_sub(fixed).unwrap();
    let exact_observation = output_observation(exact_content_bytes);
    let exact = kimi_output_page_owned_bytes(
        &state,
        "revision",
        "materializer",
        &checkpoint,
        &[],
        Some(&exact_observation),
    )
    .unwrap();
    assert_eq!(exact, NATIVE_INGESTION_PAGE_MAX_BYTES);
    let expected_frontier = checkpoint.safe_frontier().unwrap();
    let next_frontier = checkpoint.safe_frontier().unwrap();
    let output = NativeProOutputPage {
        inventory_generation: 0,
        source: state.source.clone(),
        source_epoch: state.source_epoch,
        observed_revision: "revision".to_owned(),
        parser_revision: KIMI_OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: "materializer".to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_sink_frontier.clone(),
        observations: vec![exact_observation],
    };
    assert!(NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::KimiCodeCli.as_str(), "wire"),
        expected_frontier,
        next_frontier,
        true,
        NativePageAccounting {
            logical_units: 1,
            conservative_serialized_bytes: exact,
        },
        output,
    )
    .is_ok());

    let over_observation = output_observation(exact_content_bytes + 1);
    assert_eq!(
        kimi_output_page_owned_bytes(
            &state,
            "revision",
            "materializer",
            &checkpoint,
            &[],
            Some(&over_observation),
        )
        .unwrap(),
        NATIVE_INGESTION_PAGE_MAX_BYTES + 1
    );
}
