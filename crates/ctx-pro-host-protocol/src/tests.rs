use std::{collections::BTreeSet, io::Cursor};

use proptest::prelude::*;
use serde_json::json;

use super::*;

fn host(sequence: u64, message: HostMessage) -> HostEnvelope {
    HostEnvelope {
        sequence,
        request_id: Uuid::from_u128(u128::from(sequence) + 1),
        message,
    }
}

fn hello(sequence: u64) -> HostEnvelope {
    host(
        sequence,
        HostMessage::Hello(HelloRequest::current(
            "test-host",
            BTreeSet::from([
                Capability::Status,
                Capability::JournalSync,
                Capability::Query,
            ]),
        )),
    )
}

fn provenance(kind: JournalEntityKind, id: Uuid) -> JournalProvenanceIdentity {
    JournalProvenanceIdentity {
        entity_kind: kind,
        stable_entity_id: id,
        capture_source_id: Some(Uuid::from_u128(99)),
        provider: Some("codex".to_owned()),
        provider_external_id: Some("session-1".to_owned()),
    }
}

fn record(
    generation: u64,
    sequence: u64,
    revision: u64,
    operation: JournalOperation,
    prior_digest: &str,
) -> JournalRecord {
    let id = Uuid::from_u128(u128::from(sequence) + 100);
    let canonical_payload = match operation {
        JournalOperation::Upsert => Some(json!({"sequence": sequence})),
        JournalOperation::Delete => None,
    };
    let mut record = JournalRecord {
        generation,
        sequence,
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        entity_kind: JournalEntityKind::Event,
        stable_entity_id: id,
        entity_revision: revision,
        operation,
        payload_sha256: sha256_hex(
            &canonical_payload
                .as_ref()
                .map(canonical_payload_bytes)
                .transpose()
                .expect("canonical payload")
                .unwrap_or_default(),
        ),
        canonical_payload,
        evidence: vec![JournalEvidenceIdentity {
            event_id: id,
            source_id: Some(Uuid::from_u128(99)),
            source_path: Some("fixture/session.jsonl".to_owned()),
            source_record_ordinal: Some(sequence),
            source_record_subrecord_index: Some(0),
            byte_start: Some(10),
            byte_end_exclusive: Some(20),
        }],
        provenance: provenance(JournalEntityKind::Event, id),
        cumulative_digest: "0".repeat(64),
    };
    record.cumulative_digest = journal_record_digest(prior_digest, &record).expect("record digest");
    record
}

fn full_request(records: Vec<JournalRecord>) -> JournalSyncRequest {
    let generation = records.first().map_or(1, |record| record.generation);
    let initial = initial_journal_digest(generation);
    let frozen = records.last().map_or_else(
        || JournalCheckpoint {
            position: JournalPosition {
                generation,
                sequence: 0,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: initial.clone(),
        },
        |record| JournalCheckpoint {
            position: JournalPosition {
                generation,
                sequence: record.sequence,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: record.cumulative_digest.clone(),
        },
    );
    JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: 47,
        canonical_schema_identity: "a".repeat(64),
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        prior_checkpoint: JournalCheckpoint {
            position: JournalPosition {
                generation,
                sequence: 0,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: initial,
        },
        frozen_through: frozen,
        authorized_repository_roots: Vec::new(),
        records,
        result_contents: Vec::new(),
    }
}

fn citation() -> EvidenceCitation {
    EvidenceCitation {
        observation_id: Some(Uuid::from_u128(1)),
        observation_seq: Some(1),
        observation_kind: Some(ObservationKind::Event),
        session_id: Some(Uuid::from_u128(2)),
        event_id: Some(Uuid::from_u128(1)),
        event_seq: Some(1),
        source_path: Some("fixture/session.jsonl".to_owned()),
        fixture_line: Some(1),
        source_record_ordinal: Some(0),
        source_record_subrecord_index: Some(0),
        byte_range: Some(ByteRange {
            start: 0,
            end_exclusive: 10,
        }),
        source_sha256: Some("b".repeat(64)),
    }
}

#[test]
fn exact_v1_frame_round_trips_and_rejects_other_versions() {
    let request = hello(0);
    let mut frame = Vec::new();
    write_frame(&mut frame, &request).unwrap();
    assert_eq!(&frame[..6], FRAME_MAGIC);
    assert_eq!(&frame[6..8], &PROTOCOL_VERSION.to_be_bytes());
    assert_eq!(
        read_frame::<_, HostEnvelope>(&mut Cursor::new(&frame)).unwrap(),
        request
    );
    for draft in [0_u16, 2, 3, 4, u16::MAX] {
        let mut incompatible = frame.clone();
        incompatible[6..8].copy_from_slice(&draft.to_be_bytes());
        assert!(matches!(
            read_frame::<_, HostEnvelope>(&mut Cursor::new(incompatible)),
            Err(FrameError::UnsupportedVersion { received, .. }) if received == draft
        ));
    }
}

#[test]
fn frame_payload_limit_is_exact_and_prevents_partial_output() {
    let exact = "x".repeat(MAX_FRAME_PAYLOAD_BYTES - 2);
    let mut output = Vec::new();
    write_frame(&mut output, &exact).unwrap();
    assert_eq!(output.len(), FRAME_HEADER_BYTES + MAX_FRAME_PAYLOAD_BYTES);

    let oversized = "x".repeat(MAX_FRAME_PAYLOAD_BYTES - 1);
    let mut rejected = Vec::new();
    assert!(matches!(
        write_frame(&mut rejected, &oversized),
        Err(FrameError::Oversized { .. })
    ));
    assert!(rejected.is_empty());
}

#[test]
fn exact_hello_binds_version_fingerprint_and_capabilities() {
    let mut fake = FakeHelper::default();
    let mut wrong = hello(0);
    let HostMessage::Hello(body) = &mut wrong.message else {
        unreachable!()
    };
    body.protocol_fingerprint = "0".repeat(64);
    assert!(matches!(
        fake.handle(wrong).message,
        HelperMessage::Error(ProtocolError {
            class: ErrorClass::ProtocolMismatch,
            ..
        })
    ));

    let response = fake.handle(hello(1)).message;
    assert!(matches!(
        response,
        HelperMessage::Hello(HelloResult {
            protocol_version: PROTOCOL_VERSION,
            ref protocol_fingerprint,
            ..
        }) if protocol_fingerprint == PROTOCOL_FINGERPRINT
    ));
}

#[test]
fn journal_full_baseline_is_contiguous_digest_bound_and_frozen() {
    let generation = 7;
    let initial = initial_journal_digest(generation);
    let first = record(generation, 1, 1, JournalOperation::Upsert, &initial);
    let second = record(
        generation,
        2,
        2,
        JournalOperation::Delete,
        &first.cumulative_digest,
    );
    let request = full_request(vec![first, second]);
    request.validate().unwrap();
    assert_eq!(request.committed_checkpoint(), request.frozen_through);
}

#[test]
fn journal_rejects_gaps_payload_mutation_and_tombstone_payloads() {
    let generation = 8;
    let initial = initial_journal_digest(generation);
    let mut skipped = record(generation, 2, 1, JournalOperation::Upsert, &initial);
    let request = full_request(vec![skipped.clone()]);
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Sequence);

    skipped.sequence = 1;
    skipped.canonical_payload = Some(json!({"sequence": 9}));
    let request = full_request(vec![skipped]);
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Corrupt);

    let mut tombstone = record(generation, 1, 2, JournalOperation::Delete, &initial);
    tombstone.canonical_payload = Some(json!({}));
    let request = full_request(vec![tombstone]);
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Corrupt);
}

fn request_with_result_content(content: &str) -> JournalSyncRequest {
    let generation = 10;
    let initial = initial_journal_digest(generation);
    let content_ref = ContentRef::from_bytes(content.as_bytes()).unwrap();
    let mut record = record(generation, 1, 1, JournalOperation::Upsert, &initial);
    record.canonical_payload = Some(json!({
        "result": {
            "outcome": "success",
            "identifiers": [],
            "content_ref": content_ref
        }
    }));
    record.payload_sha256 =
        sha256_hex(&canonical_payload_bytes(record.canonical_payload.as_ref().unwrap()).unwrap());
    record.cumulative_digest = journal_record_digest(&initial, &record).unwrap();
    let sidecar = ResultContentSidecar {
        journal_sequence: record.sequence,
        stable_entity_id: record.stable_entity_id,
        content_ref,
        content: content.to_owned(),
    };
    let mut request = full_request(vec![record]);
    request.result_contents.push(sidecar);
    request
}

#[test]
fn transient_result_content_is_exact_bounded_and_not_in_the_journal_digest() {
    let request = request_with_result_content("complete normalized output");
    request.validate().unwrap();
    let sidecar = &request.result_contents[0];
    let record = &request.records[0];
    assert!(sidecar.content_ref.verifies(sidecar.content.as_bytes()));
    assert_eq!(
        journal_record_digest(&request.prior_checkpoint.cumulative_digest, record).unwrap(),
        record.cumulative_digest
    );

    let mut changed = request.clone();
    changed.result_contents[0].content.push('!');
    assert_eq!(changed.validate().unwrap_err().class, ErrorClass::Corrupt);

    let mut bad_binding = request.clone();
    bad_binding.result_contents[0].stable_entity_id = Uuid::from_u128(999);
    assert_eq!(
        bad_binding.validate().unwrap_err().class,
        ErrorClass::InvalidRequest
    );

    let mut bad_reference = request.clone();
    bad_reference.result_contents[0].content_ref = ContentRef::from_bytes(b"other").unwrap();
    bad_reference.result_contents[0].content = "other".to_owned();
    assert_eq!(
        bad_reference.validate().unwrap_err().class,
        ErrorClass::Corrupt
    );

    let mut duplicate = request;
    duplicate
        .result_contents
        .push(duplicate.result_contents[0].clone());
    assert_eq!(
        duplicate.validate().unwrap_err().class,
        ErrorClass::InvalidRequest
    );
}

#[test]
fn transient_result_content_allows_distinct_revisions_of_one_event() {
    let generation = 10;
    let content = "same complete output";
    let content_ref = ContentRef::from_bytes(content.as_bytes()).unwrap();
    let mut initial_request = request_with_result_content(content);
    let first = initial_request.records.remove(0);
    let mut second = record(
        generation,
        2,
        2,
        JournalOperation::Upsert,
        &first.cumulative_digest,
    );
    second.stable_entity_id = first.stable_entity_id;
    second.provenance.stable_entity_id = first.stable_entity_id;
    second.evidence[0].event_id = first.stable_entity_id;
    second.canonical_payload = first.canonical_payload.clone();
    second.payload_sha256 = first.payload_sha256.clone();
    second.cumulative_digest = journal_record_digest(&first.cumulative_digest, &second).unwrap();

    let mut request = full_request(vec![first.clone(), second.clone()]);
    request.result_contents = vec![
        ResultContentSidecar {
            journal_sequence: first.sequence,
            stable_entity_id: first.stable_entity_id,
            content_ref: content_ref.clone(),
            content: content.to_owned(),
        },
        ResultContentSidecar {
            journal_sequence: second.sequence,
            stable_entity_id: second.stable_entity_id,
            content_ref,
            content: content.to_owned(),
        },
    ];
    request.validate().unwrap();
}

#[test]
fn transient_result_content_rejects_partial_or_overbound_text() {
    let full = "x".repeat(MAX_RESULT_CONTENT_BYTES_PER_ITEM + 1);
    let request = request_with_result_content(&full);
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Bounds);

    let mut partial = request_with_result_content("full output");
    partial.result_contents[0].content = "full".to_owned();
    assert_eq!(partial.validate().unwrap_err().class, ErrorClass::Corrupt);

    let generation = 12;
    let content = "x".repeat(MAX_RESULT_CONTENT_BYTES_PER_ITEM);
    let content_ref = ContentRef::from_bytes(content.as_bytes()).unwrap();
    let mut prior = initial_journal_digest(generation);
    let mut records = Vec::new();
    let mut sidecars = Vec::new();
    for sequence in 1..=5 {
        let mut record = record(generation, sequence, 1, JournalOperation::Upsert, &prior);
        record.canonical_payload = Some(json!({
            "result": {
                "outcome": "success",
                "identifiers": [],
                "content_ref": content_ref.clone()
            }
        }));
        record.payload_sha256 = sha256_hex(
            &canonical_payload_bytes(record.canonical_payload.as_ref().unwrap()).unwrap(),
        );
        record.cumulative_digest = journal_record_digest(&prior, &record).unwrap();
        prior.clone_from(&record.cumulative_digest);
        sidecars.push(ResultContentSidecar {
            journal_sequence: sequence,
            stable_entity_id: record.stable_entity_id,
            content_ref: content_ref.clone(),
            content: content.clone(),
        });
        records.push(record);
    }
    let mut over_total = full_request(records);
    over_total.result_contents = sidecars;
    assert_eq!(over_total.validate().unwrap_err().class, ErrorClass::Bounds);
}

#[test]
fn fake_journal_ack_is_idempotent_and_queries_require_exact_checkpoint() {
    let generation = 9;
    let initial = initial_journal_digest(generation);
    let request = full_request(vec![record(
        generation,
        1,
        1,
        JournalOperation::Upsert,
        &initial,
    )]);
    let checkpoint = request.frozen_through.clone();
    let mut fake = FakeHelper::default();
    assert!(matches!(
        fake.handle(hello(0)).message,
        HelperMessage::Hello(_)
    ));
    let first = fake.handle(host(1, HostMessage::SyncJournal(request.clone())));
    assert!(matches!(
        first.message,
        HelperMessage::JournalSynced(JournalSyncResult {
            replayed: false,
            frozen_complete: true,
            ..
        })
    ));
    let replay = fake.handle(host(2, HostMessage::SyncJournal(request)));
    assert!(matches!(
        replay.message,
        HelperMessage::JournalSynced(JournalSyncResult {
            replayed: true,
            accepted_records: 0,
            ..
        })
    ));

    let query = QueryRequest {
        kind: QueryKind::Facts,
        target: ResourceSelector {
            kind: ResourceKind::Commit,
            value: "0123456789abcdef".to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
            line: None,
        },
        limit: 10,
        cursor: None,
        expected_snapshot: QuerySnapshotExpectation {
            checkpoint,
            projection_pending: false,
        },
    };
    assert!(matches!(
        fake.handle(host(3, HostMessage::Query(query))).message,
        HelperMessage::Query(_)
    ));
}

#[test]
fn resource_selector_line_is_scoped_to_files() {
    let file = ResourceSelector {
        kind: ResourceKind::File,
        value: "src/lib.rs".to_owned(),
        repository: Some("forge:github.com/ctxrs/ctx".to_owned()),
        line: Some(42),
    };
    file.validate().unwrap();

    for kind in [
        ResourceKind::Commit,
        ResourceKind::PullRequest,
        ResourceKind::Issue,
        ResourceKind::Session,
        ResourceKind::Run,
    ] {
        let mut invalid = file.clone();
        invalid.kind = kind;
        let error = invalid.validate().unwrap_err();
        assert_eq!(error.class, ErrorClass::InvalidRequest);
        assert_eq!(
            error.message,
            "resource selector line is valid only for file targets"
        );
    }
}

#[test]
fn resource_kind_inventory_round_trips_and_rejects_unknown_values() {
    for kind in ResourceKind::ALL {
        assert_eq!(ResourceKind::from_wire_name(kind.wire_name()), Some(kind));
        assert!(matches!(
            serde_json::from_value::<ResourceKind>(serde_json::json!(kind.wire_name())),
            Ok(decoded) if decoded == kind
        ));
    }
    assert_eq!(ResourceKind::from_wire_name("deployment"), None);
    assert!(serde_json::from_str::<ResourceKind>(r#""deployment""#).is_err());
}

#[test]
fn typed_query_records_require_resolvable_citations_and_bounds() {
    let resource = ResourceRef {
        id: "commit:1".to_owned(),
        kind: ResourceKind::Commit,
        display: "0123456789ab".to_owned(),
    };
    let fact = FactRecord {
        id: "fact:1".to_owned(),
        fact_type: "commit.produced".to_owned(),
        subject: resource.clone(),
        predicate: "produced".to_owned(),
        object: FactValue::Boolean(true),
        confidence: FactConfidence::Explicit,
        state: FactState::Asserted,
        detector_version: "v1".to_owned(),
        owning_root_session_id: None,
        direct_actor_session_id: None,
        citations: vec![citation()],
    };
    fact.validate().unwrap();
    let mut uncited = fact;
    uncited.citations.clear();
    assert_eq!(uncited.validate().unwrap_err().class, ErrorClass::Corrupt);
}

#[test]
fn unknown_and_duplicate_envelope_fields_fail_closed() {
    let unknown = json!({
        "sequence": 0,
        "request_id": Uuid::from_u128(1),
        "message": {"kind": "status", "body": {}},
        "extra": true
    });
    assert!(serde_json::from_value::<HostEnvelope>(unknown).is_err());
    let duplicate = format!(
        "{{\"sequence\":0,\"sequence\":1,\"request_id\":\"{}\",\"message\":{{\"kind\":\"status\",\"body\":{{}}}}}}",
        Uuid::from_u128(1)
    );
    assert!(serde_json::from_str::<HostEnvelope>(&duplicate).is_err());
    let nil = json!({
        "sequence": 0,
        "request_id": Uuid::nil(),
        "message": {"kind": "status", "body": {}}
    });
    assert!(serde_json::from_value::<HostEnvelope>(nil.clone()).is_err());
    assert!(serde_json::from_value::<HelperEnvelope>(json!({
        "sequence": 0,
        "request_id": Uuid::nil(),
        "message": {"kind": "error", "body": {
            "class": "invalid_request", "message": "nil", "retryable": false
        }}
    }))
    .is_err());
}

#[test]
fn journal_roots_are_bounded_sorted_unique_and_control_free() {
    let mut request = full_request(Vec::new());
    request.authorized_repository_roots = vec!["/repo/a".to_owned(), "/repo/b".to_owned()];
    request.validate().unwrap();
    request.authorized_repository_roots.reverse();
    assert_eq!(
        request.validate().unwrap_err().class,
        ErrorClass::InvalidRequest
    );
    request.authorized_repository_roots = vec!["/repo\ncontrol".to_owned()];
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Bounds);
    request.authorized_repository_roots = (0..=MAX_AUTHORIZED_REPOSITORY_ROOTS)
        .map(|index| format!("/{index:03}"))
        .collect();
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Bounds);
}

proptest! {
    #[test]
    fn arbitrary_split_payloads_never_decode_as_complete_frames(split in 0usize..128) {
        let mut frame = Vec::new();
        write_frame(&mut frame, &hello(0)).unwrap();
        let split = split.min(frame.len().saturating_sub(1));
        prop_assert!(read_frame::<_, HostEnvelope>(&mut Cursor::new(&frame[..split])).is_err());
    }

    #[test]
    fn arbitrary_non_v1_headers_fail_before_json(version in any::<u16>().prop_filter("not V1", |v| *v != PROTOCOL_VERSION)) {
        let mut frame = Vec::new();
        frame.extend_from_slice(FRAME_MAGIC);
        frame.extend_from_slice(&version.to_be_bytes());
        frame.extend_from_slice(&2_u32.to_be_bytes());
        frame.extend_from_slice(b"{}");
        let rejected = matches!(
            read_frame::<_, HostEnvelope>(&mut Cursor::new(frame)),
            Err(FrameError::UnsupportedVersion { received, .. }) if received == version
        );
        prop_assert!(rejected, "non-V1 frame was not rejected exactly");
    }
}
