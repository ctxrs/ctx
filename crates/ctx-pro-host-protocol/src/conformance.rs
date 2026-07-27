use std::{collections::BTreeSet, io::Cursor};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::*;

const INVENTORY: &str = include_str!("../testdata/v1/inventory.json");

fn inventory() -> Value {
    serde_json::from_str(INVENTORY).expect("Protocol V1 inventory must be valid JSON")
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let nibble = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => panic!("invalid golden hex"),
            };
            (nibble(pair[0]) << 4) | nibble(pair[1])
        })
        .collect()
}

fn host_kind(message: &HostMessage) -> &'static str {
    match message {
        HostMessage::Hello(_) => "hello",
        HostMessage::Authorize(_) => "authorize",
        HostMessage::PrepareGraphKeyDeletion(_) => "prepare_graph_key_deletion",
        HostMessage::ConfirmGraphKeyDeletion(_) => "confirm_graph_key_deletion",
        HostMessage::Status(_) => "status",
        HostMessage::SyncJournal(_) => "sync_journal",
        HostMessage::Query(_) => "query",
    }
}

fn helper_kind(message: &HelperMessage) -> &'static str {
    match message {
        HelperMessage::Hello(_) => "hello",
        HelperMessage::Authorized(_) => "authorized",
        HelperMessage::GraphKeyDeletionPrepared(_) => "graph_key_deletion_prepared",
        HelperMessage::GraphKeyDeleted(_) => "graph_key_deleted",
        HelperMessage::Status(_) => "status",
        HelperMessage::JournalSynced(_) => "journal_synced",
        HelperMessage::Query(_) => "query",
        HelperMessage::Error(_) => "error",
    }
}

fn checkpoint() -> JournalCheckpoint {
    JournalCheckpoint {
        position: JournalPosition {
            generation: 1,
            sequence: 0,
        },
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        cumulative_digest: initial_journal_digest(1),
    }
}

fn authorization() -> AuthorizationRequest {
    let capabilities = BTreeSet::from([
        EntitlementCapability::GraphRead,
        EntitlementCapability::GraphWrite,
    ]);
    AuthorizationRequest {
        entitlement: SignedEntitlement {
            grant: EntitlementGrant {
                schema_version: ENTITLEMENT_SCHEMA_VERSION,
                issuer: "https://commercial.ctx.rs".to_owned(),
                key_id: "fixture-v1".to_owned(),
                grant_id: "grant-1".to_owned(),
                subject: "user-1".to_owned(),
                account_id: "account-1".to_owned(),
                product: "ctx-local-pro".to_owned(),
                access_kind: EntitlementAccessKind::Trial,
                installation_key_thumbprint: base64url(&[1; 32]),
                issued_at_unix: 100,
                not_before_unix: 90,
                refresh_after_unix: 150,
                access_deadline_unix: 200,
                grace_deadline_unix: 250,
                expires_at_unix: 175,
                minimum_helper_protocol: PROTOCOL_VERSION,
                revocation_epoch: 0,
                capabilities,
            },
            signature_base64url: base64url(&[2; ED25519_SIGNATURE_BYTES]),
        },
        installation_public_key_base64url: base64url(&[3; INSTALLATION_PUBLIC_KEY_BYTES]),
        challenge_base64url: base64url(&[4; AUTHORIZATION_CHALLENGE_BYTES]),
        proof_signature_base64url: base64url(&[5; ED25519_SIGNATURE_BYTES]),
    }
}

fn query() -> QueryRequest {
    QueryRequest {
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
            checkpoint: checkpoint(),
            projection_pending: false,
        },
    }
}

fn journal_request() -> JournalSyncRequest {
    let checkpoint = checkpoint();
    JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: 47,
        canonical_schema_identity: "ctx-store-schema-47-final-v3".to_owned(),
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        prior_checkpoint: checkpoint.clone(),
        frozen_through: checkpoint,
        authorized_repository_roots: Vec::new(),
        records: Vec::new(),
        result_contents: Vec::new(),
    }
}

fn host_messages() -> Vec<HostMessage> {
    vec![
        HostMessage::Hello(HelloRequest::current(
            "conformance-host",
            BTreeSet::from([
                Capability::EntitlementAuthorization,
                Capability::GraphKeyDeletion,
                Capability::Status,
                Capability::JournalSync,
                Capability::Query,
                Capability::GitRead,
            ]),
        )),
        HostMessage::Authorize(authorization()),
        HostMessage::PrepareGraphKeyDeletion(PrepareGraphKeyDeletionRequest {
            installation_key_thumbprint: base64url(&[1; 32]),
        }),
        HostMessage::ConfirmGraphKeyDeletion(ConfirmGraphKeyDeletionRequest {
            authorization: authorization(),
        }),
        HostMessage::Status(StatusRequest {}),
        HostMessage::SyncJournal(journal_request()),
        HostMessage::Query(query()),
    ]
}

fn helper_messages() -> Vec<HelperMessage> {
    let capabilities = BTreeSet::from([
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::JournalSync,
        Capability::Query,
        Capability::GitRead,
    ]);
    vec![
        HelperMessage::Hello(HelloResult {
            protocol_version: PROTOCOL_VERSION,
            protocol_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            helper_version: "conformance-helper".to_owned(),
            capabilities,
            authorization_challenge_base64url: base64url(&[4; AUTHORIZATION_CHALLENGE_BYTES]),
        }),
        HelperMessage::Authorized(AuthorizationResult {
            state: EntitlementAccessState::Trial,
            refresh_required: false,
            expires_at_unix: 175,
            access_deadline_unix: 200,
            grace_deadline_unix: 250,
            capabilities: BTreeSet::from([EntitlementCapability::GraphRead]),
        }),
        HelperMessage::GraphKeyDeletionPrepared(GraphKeyDeletionPrepared {
            challenge_base64url: base64url(&[6; GRAPH_KEY_DELETION_CHALLENGE_BYTES]),
            expires_at_unix: 200,
            key_present: true,
        }),
        HelperMessage::GraphKeyDeleted(GraphKeyDeleted { deleted: true }),
        HelperMessage::Status(StatusResult {
            state: GraphState::NotMaterialized,
            checkpoint: None,
        }),
        HelperMessage::JournalSynced(JournalSyncResult {
            committed_through: checkpoint(),
            accepted_records: 0,
            replayed: false,
            frozen_complete: true,
        }),
        HelperMessage::Query(QueryResult {
            records: Vec::new(),
            next_cursor: None,
            truncated: false,
            stale: false,
        }),
        HelperMessage::Error(ProtocolError::new(
            ErrorClass::ProtocolMismatch,
            "exact Protocol V1 mismatch",
        )),
    ]
}

#[test]
fn canonical_inventory_and_exported_fingerprint_are_exact() {
    let value = inventory();
    let canonical = serde_json::to_vec(&value["canonical_inventory"])
        .expect("canonical inventory serialization");
    let digest = hex(&Sha256::digest(&canonical));
    assert_eq!(digest, value["canonical_sha256"]);
    assert_eq!(digest, PROTOCOL_FINGERPRINT);
    assert_eq!(value["canonical_inventory"]["protocol_version"], 1);
    assert_eq!(
        value["canonical_inventory"]["projection_contract_version"],
        1
    );
}

#[test]
fn inventory_freezes_every_message_kind_capability_and_error() {
    let value = inventory();
    let contract = &value["canonical_inventory"];
    let host: Vec<_> = host_messages().iter().map(host_kind).collect();
    let helper: Vec<_> = helper_messages().iter().map(helper_kind).collect();
    assert_eq!(
        serde_json::to_value(host).unwrap(),
        contract["host_message_kinds"]
    );
    assert_eq!(
        serde_json::to_value(helper).unwrap(),
        contract["helper_message_kinds"]
    );

    let capabilities = [
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::JournalSync,
        Capability::Query,
        Capability::GitRead,
    ];
    assert_eq!(
        serde_json::to_value(
            capabilities
                .iter()
                .map(|capability| capability.wire_name())
                .collect::<Vec<_>>()
        )
        .unwrap(),
        contract["capabilities"]
    );
    for error in contract["enums"]["error_class"]
        .as_array()
        .expect("error inventory")
    {
        serde_json::from_value::<ErrorClass>(error.clone()).expect("typed error class");
    }
}

#[test]
fn all_typed_messages_have_strict_canonical_v1_frames() {
    for (sequence, message) in host_messages().into_iter().enumerate() {
        let envelope = HostEnvelope {
            sequence: sequence as u64,
            request_id: Uuid::from_u128(sequence as u128 + 1),
            message,
        };
        let mut encoded = Vec::new();
        write_frame(&mut encoded, &envelope).unwrap();
        assert_eq!(&encoded[6..8], &PROTOCOL_VERSION.to_be_bytes());
        assert_eq!(
            read_frame::<_, HostEnvelope>(&mut Cursor::new(encoded)).unwrap(),
            envelope
        );
    }
    for (sequence, message) in helper_messages().into_iter().enumerate() {
        let envelope = HelperEnvelope {
            sequence: sequence as u64,
            request_id: Uuid::from_u128(sequence as u128 + 1),
            message,
        };
        let mut encoded = Vec::new();
        write_frame(&mut encoded, &envelope).unwrap();
        assert_eq!(&encoded[6..8], &PROTOCOL_VERSION.to_be_bytes());
        assert_eq!(
            read_frame::<_, HelperEnvelope>(&mut Cursor::new(encoded)).unwrap(),
            envelope
        );
    }
}

#[test]
fn representative_inventory_frames_decode_byte_for_byte() {
    let value = inventory();
    let frames = &value["canonical_inventory"]["representative_frames"];
    for (name, expected) in [
        ("host_status", {
            let mut bytes = Vec::new();
            write_frame(
                &mut bytes,
                &HostEnvelope {
                    sequence: 0,
                    request_id: Uuid::from_u128(1),
                    message: HostMessage::Status(StatusRequest {}),
                },
            )
            .unwrap();
            hex(&bytes)
        }),
        ("helper_protocol_mismatch", {
            let mut bytes = Vec::new();
            write_frame(
                &mut bytes,
                &HelperEnvelope {
                    sequence: 0,
                    request_id: Uuid::from_u128(1),
                    message: HelperMessage::Error(ProtocolError::new(
                        ErrorClass::ProtocolMismatch,
                        "exact Protocol V1 mismatch",
                    )),
                },
            )
            .unwrap();
            hex(&bytes)
        }),
    ] {
        assert_eq!(frames[name], expected);
    }
}

#[test]
fn every_generated_golden_frame_round_trips_byte_for_byte() {
    let value = inventory();
    for group in ["host_frames", "cursor_frames"] {
        for encoded in value["golden_vectors"][group]
            .as_object()
            .expect("host golden map")
            .values()
        {
            let bytes = unhex(encoded.as_str().expect("host golden hex"));
            let decoded = read_frame::<_, HostEnvelope>(&mut Cursor::new(&bytes)).unwrap();
            let mut round_trip = Vec::new();
            write_frame(&mut round_trip, &decoded).unwrap();
            assert_eq!(round_trip, bytes);
        }
    }
    for group in ["helper_frames", "error_frames"] {
        for encoded in value["golden_vectors"][group]
            .as_object()
            .expect("helper golden map")
            .values()
        {
            let bytes = unhex(encoded.as_str().expect("helper golden hex"));
            let decoded = read_frame::<_, HelperEnvelope>(&mut Cursor::new(&bytes)).unwrap();
            let mut round_trip = Vec::new();
            write_frame(&mut round_trip, &decoded).unwrap();
            assert_eq!(round_trip, bytes);
        }
    }
}

#[test]
fn maximum_escaping_roots_boundary_is_exact_and_under_four_mib() {
    let roots = (0..MAX_AUTHORIZED_REPOSITORY_ROOTS)
        .map(|index| {
            let prefix = format!("/{index:03}/");
            format!(
                "{prefix}{}",
                "\\".repeat(2048_usize.saturating_sub(prefix.len()))
            )
        })
        .collect::<Vec<_>>();
    let request = JournalSyncRequest {
        authorized_repository_roots: roots,
        ..journal_request()
    };
    request.validate().unwrap();
    let envelope = HostEnvelope {
        sequence: u64::MAX,
        request_id: Uuid::from_u128(1),
        message: HostMessage::SyncJournal(request),
    };
    let bytes = serde_json::to_vec(&envelope).unwrap();
    let boundary = &inventory()["golden_vectors"]["boundary_frames"]["maximum_escaping_roots"];
    assert_eq!(boundary["payload_bytes"], bytes.len());
    assert_eq!(boundary["sha256"], hex(&Sha256::digest(&bytes)));
    assert!(bytes.len() <= MAX_JOURNAL_SYNC_ENVELOPE_BYTES);
}
