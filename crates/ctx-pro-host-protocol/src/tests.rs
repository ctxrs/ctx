use std::{collections::BTreeSet, io::Cursor};

use proptest::prelude::*;

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
                Capability::SourceMaterialization,
                Capability::Query,
            ]),
        )),
    )
}

fn source_receipt() -> SourceManifestReceipt {
    SourceManifestReceipt {
        core_generation_id: "a".repeat(64),
        manifest_aggregate_sha256: "b".repeat(64),
        materializer_revision: "materializer-v1".to_owned(),
        progress: Vec::new(),
    }
}

fn source_snapshot() -> QuerySnapshotExpectation {
    QuerySnapshotExpectation::Source {
        receipt: SourceManifestReceiptIdentity::from_receipt(&source_receipt()).unwrap(),
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
fn query_snapshot_is_source_authoritative_and_exact() {
    let receipt = source_receipt();
    let identity = SourceManifestReceiptIdentity::from_receipt(&receipt).unwrap();
    assert_eq!(
        identity.receipt_sha256,
        source_manifest_receipt_sha256(&receipt).unwrap()
    );
    let snapshot = QuerySnapshotExpectation::Source { receipt: identity };
    snapshot.validate().unwrap();
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap()["kind"],
        serde_json::json!("source")
    );
}

#[test]
fn status_receipt_exists_only_for_completed_source_authority() {
    StatusResult {
        state: GraphState::Ready,
        authority: MaterializationAuthority::Source,
        source_receipt: Some(source_receipt()),
    }
    .validate()
    .unwrap();

    StatusResult {
        state: GraphState::NeedsResume,
        authority: MaterializationAuthority::Source,
        source_receipt: None,
    }
    .validate()
    .unwrap();

    for state in [
        GraphState::NotMaterialized,
        GraphState::NeedsRebuild,
        GraphState::Partial,
        GraphState::NeedsResume,
    ] {
        let invalid = StatusResult {
            state,
            authority: MaterializationAuthority::Source,
            source_receipt: Some(source_receipt()),
        };
        assert_eq!(invalid.validate().unwrap_err().class, ErrorClass::Sequence);
    }
}

#[test]
fn source_citations_are_typed_bounded_and_strict() {
    let value = citation();
    assert!(value.is_usable());

    let mut incomplete_coordinate = value.clone();
    incomplete_coordinate.observation_seq = None;
    assert!(!incomplete_coordinate.is_usable());

    let mut invalid_hash = value.clone();
    invalid_hash.source_sha256 = Some("A".repeat(64));
    assert!(!invalid_hash.is_usable());

    let mut unknown = serde_json::to_value(value).unwrap();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("provider_legacy".to_owned(), serde_json::json!({}));
    assert!(serde_json::from_value::<EvidenceCitation>(unknown).is_err());
}

#[test]
fn exact_handshake_rejects_wrong_fingerprint_and_negotiates_current_capabilities() {
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
fn fake_query_accepts_source_snapshot_and_returns_bound_target() {
    let mut fake = FakeHelper::default();
    assert!(matches!(
        fake.handle(hello(0)).message,
        HelperMessage::Hello(_)
    ));
    let query = BlameRequest {
        target: BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
        },
        limit: 10,
        cursor: None,
        expected_snapshot: source_snapshot(),
    };
    assert!(matches!(
        fake.handle(host(1, HostMessage::Blame(query))).message,
        HelperMessage::Blame(BlameResult {
            target: ResolvedBlameTarget::Commit { .. },
            ..
        })
    ));
}

#[test]
fn blame_targets_and_limits_are_bounded() {
    let request = BlameRequest {
        target: BlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
            lines: Some(LineRange { start: 42, end: 60 }),
        },
        limit: MAX_BLAME_RESULTS,
        cursor: Some("c".repeat(MAX_BLAME_CURSOR_BYTES)),
        expected_snapshot: source_snapshot(),
    };
    request.validate().unwrap();

    let mut invalid = request;
    invalid.limit = 0;
    assert_eq!(invalid.validate().unwrap_err().class, ErrorClass::Bounds);
    invalid.limit = 1;
    invalid.target = BlameTarget::File {
        path: "src/lib.rs".to_owned(),
        repository: None,
        lines: Some(LineRange { start: 0, end: 1 }),
    };
    assert_eq!(
        invalid.validate().unwrap_err().class,
        ErrorClass::InvalidRequest
    );
}

#[test]
fn frame_payload_limit_is_exact_and_prevents_partial_writes() {
    let exact = "x".repeat(MAX_FRAME_PAYLOAD_BYTES - 2);
    let mut encoded = Vec::new();
    write_frame(&mut encoded, &exact).unwrap();
    assert_eq!(encoded.len(), FRAME_HEADER_BYTES + MAX_FRAME_PAYLOAD_BYTES);
    assert_eq!(
        read_frame::<_, String>(&mut Cursor::new(encoded)).unwrap(),
        exact
    );

    let too_large = "x".repeat(MAX_FRAME_PAYLOAD_BYTES - 1);
    let mut output = Vec::new();
    assert!(matches!(
        write_frame(&mut output, &too_large),
        Err(FrameError::Oversized { .. })
    ));
    assert!(output.is_empty());
}

#[test]
fn unknown_duplicate_and_nil_envelope_fields_fail_closed() {
    let unknown = serde_json::json!({
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
    let nil = serde_json::json!({
        "sequence": 0,
        "request_id": Uuid::nil(),
        "message": {"kind": "status", "body": {}}
    });
    assert!(serde_json::from_value::<HostEnvelope>(nil).is_err());
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
        prop_assert!(rejected);
    }
}
