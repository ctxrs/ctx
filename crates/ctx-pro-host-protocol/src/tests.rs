use std::{collections::BTreeSet, io::Cursor};

use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
    SessionIdentityInput, SourceAnchor, TypedKey,
};

use super::*;

fn source() -> SourceKey {
    SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage([3; 32]),
    )
    .unwrap()
}

fn receipt(generation: char) -> CoreMaterializationReceipt {
    CoreMaterializationReceipt {
        core_generation_id: generation.to_string().repeat(64),
        core_record_contract_fingerprint: "b".repeat(64),
        source_snapshot_sha256: "c".repeat(64),
        materializer_revision: "materializer-v1".to_owned(),
        source_count: 1,
        event_count: 2,
    }
}

fn access(state: ProAccessState) -> ProAccessStatus {
    ProAccessStatus {
        entitlement: state,
        graph_key: state,
        local_repository: state,
    }
}

fn operations() -> BTreeSet<ProOperation> {
    BTreeSet::from([
        ProOperation::FileBlame,
        ProOperation::CommitBlame,
        ProOperation::PullRequestBlame,
    ])
}

fn repository_coverage(event_count: u64) -> RepositoryCoverage {
    RepositoryCoverage {
        repository_candidate_events: event_count,
        logical_binding_events: event_count,
        certified_live_root_access_events: event_count,
        file_evidence_events: event_count,
        exact_commit_evidence_events: event_count,
        exact_pull_request_evidence_events: event_count,
    }
}

#[test]
fn status_axes_preserve_terminal_empty_without_advertising_blame() {
    let generation = "a".repeat(64);
    let mut empty_receipt = receipt('a');
    empty_receipt.event_count = 0;
    let quiet = StatusResult {
        currentness: CoreProjectionCurrentness::Current,
        requested_core_generation_id: Some(generation.clone()),
        core_receipt: Some(empty_receipt),
        coverage: MaterializedCoverage::Empty,
        repository_coverage: RepositoryCoverage::default(),
        core_preparation_peak_workers: 0,
        access: access(ProAccessState::Available),
        supported_operations: operations(),
        available_operations: BTreeSet::new(),
        finalization_progress: None,
        storage_evidence: None,
    };
    quiet.validate().unwrap();

    let mut invalid = quiet.clone();
    invalid.available_operations.insert(ProOperation::FileBlame);
    assert_eq!(invalid.validate().unwrap_err().class, ErrorClass::Sequence);

    let mut abstained = quiet.clone();
    abstained.core_receipt = Some(receipt('a'));
    abstained.coverage = MaterializedCoverage::Abstained;
    abstained.validate().unwrap();

    let ready = StatusResult {
        core_receipt: Some(receipt('a')),
        coverage: MaterializedCoverage::Complete,
        repository_coverage: repository_coverage(2),
        available_operations: operations(),
        ..quiet
    };
    ready.validate().unwrap();
}

#[test]
fn status_currentness_is_bound_to_requested_and_receipt_generations() {
    let stale = StatusResult {
        currentness: CoreProjectionCurrentness::Stale,
        requested_core_generation_id: Some("d".repeat(64)),
        core_receipt: Some(receipt('a')),
        coverage: MaterializedCoverage::Partial,
        repository_coverage: RepositoryCoverage::default(),
        core_preparation_peak_workers: 0,
        access: access(ProAccessState::Available),
        supported_operations: operations(),
        available_operations: BTreeSet::new(),
        finalization_progress: None,
        storage_evidence: None,
    };
    stale.validate().unwrap();

    let mut false_current = stale;
    false_current.currentness = CoreProjectionCurrentness::Current;
    false_current.coverage = MaterializedCoverage::Complete;
    assert_eq!(
        false_current.validate().unwrap_err().class,
        ErrorClass::Sequence
    );
}

#[test]
fn finalizing_status_binds_durable_progress_without_displacing_prior_receipt() {
    let requested = "d".repeat(64);
    let progress = CoreMaterializationFinalizationProgress {
        materialization_id: "e".repeat(64),
        core_generation_id: requested.clone(),
        finish_request_digest: "1".repeat(64),
        materializer_revision: "materializer-v1".to_owned(),
        phase: CoreMaterializationFinalizationPhase::EmitFlat,
        cursor_sha256: "f".repeat(64),
    };
    let mut status = StatusResult {
        currentness: CoreProjectionCurrentness::Finalizing,
        requested_core_generation_id: Some(requested),
        core_receipt: Some(receipt('a')),
        coverage: MaterializedCoverage::Complete,
        repository_coverage: repository_coverage(2),
        core_preparation_peak_workers: 4,
        access: access(ProAccessState::Available),
        supported_operations: operations(),
        available_operations: operations(),
        finalization_progress: Some(progress),
        storage_evidence: None,
    };
    status.validate().unwrap();

    status.requested_core_generation_id = Some("c".repeat(64));
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);
    status.requested_core_generation_id = Some("d".repeat(64));
    status.finalization_progress = None;
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);

    status.currentness = CoreProjectionCurrentness::Partial;
    status.finalization_progress = Some(CoreMaterializationFinalizationProgress {
        materialization_id: "e".repeat(64),
        core_generation_id: "d".repeat(64),
        finish_request_digest: "1".repeat(64),
        materializer_revision: "materializer-v1".to_owned(),
        phase: CoreMaterializationFinalizationPhase::EmitFlat,
        cursor_sha256: "f".repeat(64),
    });
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);
}

#[test]
fn available_operations_are_a_supported_ready_subset() {
    let mut status = StatusResult {
        currentness: CoreProjectionCurrentness::Current,
        requested_core_generation_id: Some("a".repeat(64)),
        core_receipt: Some(receipt('a')),
        coverage: MaterializedCoverage::Complete,
        repository_coverage: repository_coverage(2),
        core_preparation_peak_workers: 4,
        access: access(ProAccessState::Available),
        supported_operations: BTreeSet::from([ProOperation::CommitBlame]),
        available_operations: BTreeSet::from([ProOperation::FileBlame]),
        finalization_progress: None,
        storage_evidence: None,
    };
    assert_eq!(
        status.validate().unwrap_err().class,
        ErrorClass::InvalidRequest
    );
    status.available_operations = BTreeSet::from([ProOperation::CommitBlame]);
    status.access.local_repository = ProAccessState::Unavailable;
    status.validate().unwrap();

    status.repository_coverage.exact_commit_evidence_events = 0;
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);

    status.repository_coverage = repository_coverage(2);
    status.supported_operations = operations();
    status.available_operations = BTreeSet::from([ProOperation::FileBlame]);
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);
    status.access.local_repository = ProAccessState::Available;
    status.repository_coverage.certified_live_root_access_events = 0;
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);
    status.repository_coverage.certified_live_root_access_events = 1;
    status.repository_coverage.file_evidence_events = 0;
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);
    status.repository_coverage.file_evidence_events = 1;
    status.repository_coverage.exact_commit_evidence_events = 0;
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);
    status.repository_coverage.exact_commit_evidence_events = 1;
    status.validate().unwrap();

    status.available_operations = BTreeSet::from([ProOperation::PullRequestBlame]);
    status.access.local_repository = ProAccessState::Unavailable;
    status.validate().unwrap();
    status
        .repository_coverage
        .exact_pull_request_evidence_events = 0;
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);

    status.available_operations = BTreeSet::from([ProOperation::CommitBlame]);
    status.repository_coverage = repository_coverage(2);
    status.access.entitlement = ProAccessState::Locked;
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);
    status.access.entitlement = ProAccessState::Available;
    status.access.graph_key = ProAccessState::Unavailable;
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);

    status.access = access(ProAccessState::Available);
    status.available_operations.clear();
    status.validate().unwrap();
}

#[test]
fn repository_coverage_is_zero_without_and_bounded_by_a_receipt() {
    let mut status = StatusResult {
        currentness: CoreProjectionCurrentness::Partial,
        requested_core_generation_id: Some("a".repeat(64)),
        core_receipt: None,
        coverage: MaterializedCoverage::Partial,
        repository_coverage: RepositoryCoverage::default(),
        core_preparation_peak_workers: 0,
        access: access(ProAccessState::Available),
        supported_operations: operations(),
        available_operations: BTreeSet::new(),
        finalization_progress: None,
        storage_evidence: None,
    };
    status.validate().unwrap();

    status.repository_coverage.repository_candidate_events = 1;
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);

    status.core_receipt = Some(receipt('a'));
    for coverage in [
        RepositoryCoverage {
            repository_candidate_events: 3,
            ..RepositoryCoverage::default()
        },
        RepositoryCoverage {
            logical_binding_events: 3,
            ..RepositoryCoverage::default()
        },
        RepositoryCoverage {
            certified_live_root_access_events: 3,
            ..RepositoryCoverage::default()
        },
        RepositoryCoverage {
            file_evidence_events: 3,
            ..RepositoryCoverage::default()
        },
        RepositoryCoverage {
            exact_commit_evidence_events: 3,
            ..RepositoryCoverage::default()
        },
        RepositoryCoverage {
            exact_pull_request_evidence_events: 3,
            ..RepositoryCoverage::default()
        },
    ] {
        status.repository_coverage = coverage;
        assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);
    }

    status.repository_coverage = repository_coverage(2);
    status.validate().unwrap();
}

#[test]
fn impossible_terminal_status_and_coverage_lattice_vectors_fail_closed() {
    let base = StatusResult {
        currentness: CoreProjectionCurrentness::Current,
        requested_core_generation_id: Some("a".repeat(64)),
        core_receipt: Some(receipt('a')),
        coverage: MaterializedCoverage::Complete,
        repository_coverage: repository_coverage(2),
        core_preparation_peak_workers: 4,
        access: access(ProAccessState::Available),
        supported_operations: operations(),
        available_operations: BTreeSet::new(),
        finalization_progress: None,
        storage_evidence: None,
    };

    let impossible_coverages = [
        RepositoryCoverage {
            repository_candidate_events: 1,
            logical_binding_events: 2,
            ..RepositoryCoverage::default()
        },
        RepositoryCoverage {
            repository_candidate_events: 2,
            logical_binding_events: 1,
            certified_live_root_access_events: 2,
            ..RepositoryCoverage::default()
        },
        RepositoryCoverage {
            repository_candidate_events: 2,
            logical_binding_events: 1,
            file_evidence_events: 2,
            ..RepositoryCoverage::default()
        },
        RepositoryCoverage {
            repository_candidate_events: 2,
            logical_binding_events: 1,
            exact_commit_evidence_events: 2,
            ..RepositoryCoverage::default()
        },
        RepositoryCoverage {
            repository_candidate_events: 2,
            logical_binding_events: 1,
            exact_pull_request_evidence_events: 2,
            ..RepositoryCoverage::default()
        },
    ];
    for repository_coverage in impossible_coverages {
        let status = StatusResult {
            repository_coverage,
            ..base.clone()
        };
        assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);
    }

    for coverage in [MaterializedCoverage::Empty, MaterializedCoverage::Abstained] {
        let status = StatusResult {
            coverage,
            ..base.clone()
        };
        assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);
    }
    let status = StatusResult {
        coverage: MaterializedCoverage::Complete,
        repository_coverage: RepositoryCoverage::default(),
        ..base
    };
    assert_eq!(status.validate().unwrap_err().class, ErrorClass::Sequence);
}

#[test]
fn core_query_snapshot_and_citation_are_generation_stable() {
    let source = source();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &NativeSessionKey::native_id("session", TypedKey::U64(1)).unwrap(),
    })
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(2)).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    let citation = EvidenceCitation {
        core_generation_id: "a".repeat(64),
        source,
        session_id,
        event_id,
        event_sequence: 7,
        byte_range: Some(ByteRange {
            start: 10,
            end_exclusive: 20,
        }),
        evidence_sha256: Some("e".repeat(64)),
    };
    assert!(citation.is_usable());
    let encoded = serde_json::to_value(&citation).unwrap();
    assert!(encoded.get("source_locator").is_none());

    QuerySnapshotExpectation::Core {
        receipt: CoreMaterializationReceiptIdentity::from_receipt(&receipt('a')).unwrap(),
    }
    .validate()
    .unwrap();
}

#[test]
fn core_capability_and_strict_status_frame_round_trip() {
    let envelope = HostEnvelope {
        sequence: 4,
        request_id: uuid::Uuid::from_u128(1),
        message: HostMessage::Hello(HelloRequest::current(
            "test-host",
            BTreeSet::from([Capability::Status, Capability::CoreMaterialization]),
        )),
    };
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &envelope).unwrap();
    assert_eq!(
        read_frame::<_, HostEnvelope>(&mut Cursor::new(bytes)).unwrap(),
        envelope
    );

    let value = serde_json::json!({"requested_core_generation_id": null, "legacy": true});
    assert!(serde_json::from_value::<StatusRequest>(value).is_err());
}

#[test]
fn protocol_v2_frame_is_rejected_before_v3_payload_decoding() {
    let envelope = HostEnvelope {
        sequence: 4,
        request_id: uuid::Uuid::from_u128(1),
        message: HostMessage::Status(StatusRequest {
            requested_core_generation_id: None,
        }),
    };
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &envelope).unwrap();
    let version_offset = FRAME_MAGIC.len();
    bytes[version_offset..version_offset + 2].copy_from_slice(&2_u16.to_be_bytes());

    assert!(matches!(
        read_frame::<_, HostEnvelope>(&mut Cursor::new(bytes)),
        Err(FrameError::UnsupportedVersion {
            received: 2,
            supported: 3
        })
    ));
}

#[test]
fn storage_evidence_is_strict_and_required_nullable() {
    let evidence = ProStorageEvidence {
        graph_manifest_schema: 3,
        flat_format_version: 2,
        materializer_checkpoint_version: 5,
        journal_pack_format_version: 3,
        legacy_journals_written: 0,
        journal_pages_written: 2,
        journal_packs_written: 1,
        journal_finish_activity: JournalFinishActivity {
            worker_limit: 1,
            peak_workers: 1,
            started_after_preparation: true,
        },
    };
    assert_eq!(
        serde_json::from_value::<ProStorageEvidence>(serde_json::to_value(&evidence).unwrap())
            .unwrap(),
        evidence
    );

    let status = StatusResult {
        currentness: CoreProjectionCurrentness::NotMaterialized,
        requested_core_generation_id: None,
        core_receipt: None,
        coverage: MaterializedCoverage::NotMaterialized,
        repository_coverage: RepositoryCoverage::default(),
        core_preparation_peak_workers: 0,
        access: access(ProAccessState::Unavailable),
        supported_operations: BTreeSet::new(),
        available_operations: BTreeSet::new(),
        finalization_progress: None,
        storage_evidence: None,
    };
    let mut value = serde_json::to_value(status).unwrap();
    assert_eq!(value["core_preparation_peak_workers"], 0);
    let mut missing_peak = value.clone();
    missing_peak
        .as_object_mut()
        .unwrap()
        .remove("core_preparation_peak_workers");
    assert!(serde_json::from_value::<StatusResult>(missing_peak).is_err());
    assert_eq!(value["storage_evidence"], serde_json::Value::Null);
    assert_eq!(value["finalization_progress"], serde_json::Value::Null);
    let mut missing_progress = value.clone();
    missing_progress
        .as_object_mut()
        .unwrap()
        .remove("finalization_progress");
    assert!(serde_json::from_value::<StatusResult>(missing_progress).is_err());
    value.as_object_mut().unwrap().remove("storage_evidence");
    assert!(serde_json::from_value::<StatusResult>(value).is_err());

    let mut excessive_peak = serde_json::to_value(StatusResult {
        currentness: CoreProjectionCurrentness::NotMaterialized,
        requested_core_generation_id: None,
        core_receipt: None,
        coverage: MaterializedCoverage::NotMaterialized,
        repository_coverage: RepositoryCoverage::default(),
        core_preparation_peak_workers: 17,
        access: access(ProAccessState::Unavailable),
        supported_operations: BTreeSet::new(),
        available_operations: BTreeSet::new(),
        finalization_progress: None,
        storage_evidence: None,
    })
    .unwrap();
    let excessive_peak = serde_json::from_value::<StatusResult>(excessive_peak.take()).unwrap();
    assert_eq!(
        excessive_peak.validate().unwrap_err().class,
        ErrorClass::Bounds
    );

    let mut unknown = serde_json::to_value(&evidence).unwrap();
    unknown["legacy"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ProStorageEvidence>(unknown).is_err());

    let mut missing_activity = serde_json::to_value(&evidence).unwrap();
    missing_activity
        .as_object_mut()
        .unwrap()
        .remove("journal_finish_activity");
    assert!(serde_json::from_value::<ProStorageEvidence>(missing_activity).is_err());

    for activity in [
        JournalFinishActivity {
            worker_limit: 9,
            peak_workers: 1,
            started_after_preparation: true,
        },
        JournalFinishActivity {
            worker_limit: 0,
            peak_workers: 1,
            started_after_preparation: true,
        },
        JournalFinishActivity {
            worker_limit: 1,
            peak_workers: 0,
            started_after_preparation: true,
        },
        JournalFinishActivity {
            worker_limit: 1,
            peak_workers: 2,
            started_after_preparation: true,
        },
    ] {
        assert!(activity.validate(1).is_err(), "activity={activity:?}");
    }
}

#[test]
fn storage_evidence_accepts_v4_through_v8_only() {
    let evidence = ProStorageEvidence {
        graph_manifest_schema: 3,
        flat_format_version: 2,
        materializer_checkpoint_version: 8,
        journal_pack_format_version: 3,
        legacy_journals_written: 0,
        journal_pages_written: 2,
        journal_packs_written: 1,
        journal_finish_activity: JournalFinishActivity {
            worker_limit: 1,
            peak_workers: 1,
            started_after_preparation: true,
        },
    };
    evidence.validate().unwrap();

    let mut invalid_v8 = evidence.clone();
    invalid_v8.legacy_journals_written = 1;
    assert_eq!(
        invalid_v8.validate().unwrap_err().class,
        ErrorClass::Sequence
    );

    for checkpoint_version in [4, 5, 6, 7] {
        let mut accepted = evidence.clone();
        accepted.materializer_checkpoint_version = checkpoint_version;
        accepted.validate().unwrap();
    }
    for checkpoint_version in [0, 3, 9, u16::MAX] {
        let mut rejected = evidence.clone();
        rejected.materializer_checkpoint_version = checkpoint_version;
        assert_eq!(rejected.validate().unwrap_err().class, ErrorClass::Sequence);
    }
}
