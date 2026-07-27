#![recursion_limit = "512"]

use std::collections::{BTreeMap, BTreeSet};

use ctx_pro_host_protocol::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const FINGERPRINT_PLACEHOLDER: &str = "<sha256-of-this-canonical-inventory>";

fn fields(required: &[&str], optional: &[&str]) -> Value {
    json!({"required": required, "optional": optional})
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

fn frame_hex<T: serde::Serialize>(value: &T) -> String {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, value).unwrap_or_else(|error| panic!("encode golden frame: {error}"));
    hex(&bytes)
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
                capabilities: BTreeSet::from([
                    EntitlementCapability::GraphRead,
                    EntitlementCapability::GraphWrite,
                ]),
            },
            signature_base64url: base64url(&[2; ED25519_SIGNATURE_BYTES]),
        },
        installation_public_key_base64url: base64url(&[3; INSTALLATION_PUBLIC_KEY_BYTES]),
        challenge_base64url: base64url(&[4; AUTHORIZATION_CHALLENGE_BYTES]),
        proof_signature_base64url: base64url(&[5; ED25519_SIGNATURE_BYTES]),
    }
}

fn journal_request(roots: Vec<String>) -> JournalSyncRequest {
    let checkpoint = checkpoint();
    JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: 47,
        canonical_schema_identity: "ctx-store-schema-47-final-v3".to_owned(),
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        prior_checkpoint: checkpoint.clone(),
        frozen_through: checkpoint,
        authorized_repository_roots: roots,
        records: Vec::new(),
        result_contents: Vec::new(),
    }
}

fn query(cursor: Option<String>) -> QueryRequest {
    QueryRequest {
        kind: QueryKind::Facts,
        target: ResourceSelector {
            kind: ResourceKind::Commit,
            value: "0123456789abcdef".to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
            line: None,
        },
        limit: 10,
        cursor,
        expected_snapshot: QuerySnapshotExpectation {
            checkpoint: checkpoint(),
            projection_pending: false,
        },
    }
}

fn host_messages() -> Vec<(&'static str, HostMessage)> {
    let capabilities = BTreeSet::from([
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::JournalSync,
        Capability::Query,
        Capability::GitRead,
    ]);
    vec![
        (
            "hello",
            HostMessage::Hello(HelloRequest::current("golden-host", capabilities)),
        ),
        ("authorize", HostMessage::Authorize(authorization())),
        (
            "prepare_graph_key_deletion",
            HostMessage::PrepareGraphKeyDeletion(PrepareGraphKeyDeletionRequest {
                installation_key_thumbprint: base64url(&[1; 32]),
            }),
        ),
        (
            "confirm_graph_key_deletion",
            HostMessage::ConfirmGraphKeyDeletion(ConfirmGraphKeyDeletionRequest {
                authorization: authorization(),
            }),
        ),
        ("status", HostMessage::Status(StatusRequest {})),
        (
            "sync_journal",
            HostMessage::SyncJournal(journal_request(Vec::new())),
        ),
        ("query", HostMessage::Query(query(None))),
    ]
}

fn helper_messages() -> Vec<(&'static str, HelperMessage)> {
    let capabilities = BTreeSet::from([
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::JournalSync,
        Capability::Query,
        Capability::GitRead,
    ]);
    vec![
        (
            "hello",
            HelperMessage::Hello(HelloResult {
                protocol_version: PROTOCOL_VERSION,
                protocol_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
                helper_version: "golden-helper".to_owned(),
                capabilities,
                authorization_challenge_base64url: base64url(&[4; AUTHORIZATION_CHALLENGE_BYTES]),
            }),
        ),
        (
            "authorized",
            HelperMessage::Authorized(AuthorizationResult {
                state: EntitlementAccessState::Trial,
                refresh_required: false,
                expires_at_unix: 175,
                access_deadline_unix: 200,
                grace_deadline_unix: 250,
                capabilities: BTreeSet::from([EntitlementCapability::GraphRead]),
            }),
        ),
        (
            "graph_key_deletion_prepared",
            HelperMessage::GraphKeyDeletionPrepared(GraphKeyDeletionPrepared {
                challenge_base64url: base64url(&[6; GRAPH_KEY_DELETION_CHALLENGE_BYTES]),
                expires_at_unix: 200,
                key_present: true,
            }),
        ),
        (
            "graph_key_deleted",
            HelperMessage::GraphKeyDeleted(GraphKeyDeleted { deleted: true }),
        ),
        (
            "status",
            HelperMessage::Status(StatusResult {
                state: GraphState::NotMaterialized,
                checkpoint: None,
            }),
        ),
        (
            "journal_synced",
            HelperMessage::JournalSynced(JournalSyncResult {
                committed_through: checkpoint(),
                accepted_records: 0,
                replayed: false,
                frozen_complete: true,
            }),
        ),
        (
            "query",
            HelperMessage::Query(QueryResult {
                records: Vec::new(),
                next_cursor: None,
                truncated: false,
                stale: false,
            }),
        ),
        (
            "error",
            HelperMessage::Error(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "exact Protocol V1 mismatch",
            )),
        ),
    ]
}

fn error_classes() -> Vec<ErrorClass> {
    vec![
        ErrorClass::EntitlementExpired,
        ErrorClass::KeyStoreUnavailable,
        ErrorClass::KeyStoreLocked,
        ErrorClass::NotMaterialized,
        ErrorClass::ProtocolMismatch,
        ErrorClass::MissingSource,
        ErrorClass::MissingRepository,
        ErrorClass::StaleFact,
        ErrorClass::Ambiguous,
        ErrorClass::Corrupt,
        ErrorClass::InvalidRequest,
        ErrorClass::Bounds,
        ErrorClass::Sequence,
        ErrorClass::Internal,
    ]
}

fn error_name(error: ErrorClass) -> String {
    serde_json::to_value(error)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "invalid".to_owned())
}

fn maximum_escaping_roots() -> Vec<String> {
    (0..MAX_AUTHORIZED_REPOSITORY_ROOTS)
        .map(|index| {
            let prefix = format!("/{index:03}/");
            format!(
                "{prefix}{}",
                "\\".repeat(2048_usize.saturating_sub(prefix.len()))
            )
        })
        .collect()
}

fn golden_vectors() -> Value {
    let request_id = Uuid::from_u128(1);
    let host = host_messages()
        .into_iter()
        .enumerate()
        .map(|(sequence, (name, message))| {
            (
                name,
                frame_hex(&HostEnvelope {
                    sequence: sequence as u64,
                    request_id,
                    message,
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let helper = helper_messages()
        .into_iter()
        .enumerate()
        .map(|(sequence, (name, message))| {
            (
                name,
                frame_hex(&HelperEnvelope {
                    sequence: sequence as u64,
                    request_id,
                    message,
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let errors = error_classes()
        .into_iter()
        .map(|class| {
            let name = error_name(class);
            let frame = frame_hex(&HelperEnvelope {
                sequence: 0,
                request_id,
                message: HelperMessage::Error(ProtocolError::new(class, "golden error")),
            });
            (name, frame)
        })
        .collect::<BTreeMap<_, _>>();
    let max_cursor = frame_hex(&HostEnvelope {
        sequence: u64::MAX,
        request_id,
        message: HostMessage::Query(query(Some("c".repeat(MAX_QUERY_CURSOR_BYTES)))),
    });
    let max_roots = HostEnvelope {
        sequence: u64::MAX,
        request_id,
        message: HostMessage::SyncJournal(journal_request(maximum_escaping_roots())),
    };
    let max_roots_bytes = serde_json::to_vec(&max_roots)
        .unwrap_or_else(|error| panic!("max roots envelope: {error}"));
    json!({
        "host_frames": host,
        "helper_frames": helper,
        "error_frames": errors,
        "cursor_frames": {"query_cursor_max": max_cursor},
        "boundary_frames": {
            "maximum_escaping_roots": {
                "payload_bytes": max_roots_bytes.len(),
                "sha256": hex(&Sha256::digest(&max_roots_bytes)),
                "root_count": MAX_AUTHORIZED_REPOSITORY_ROOTS,
                "root_total_unescaped_bytes": MAX_AUTHORIZED_REPOSITORY_ROOTS_TOTAL_BYTES
            }
        }
    })
}

fn wire_names<T: Copy>(values: &[T], name: impl Fn(T) -> &'static str) -> Vec<&'static str> {
    values.iter().copied().map(name).collect()
}

fn inventory() -> Value {
    let capabilities = [
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::JournalSync,
        Capability::Query,
        Capability::GitRead,
    ];
    let request_id = Uuid::from_u128(1);
    let status = HostEnvelope {
        sequence: 0,
        request_id,
        message: HostMessage::Status(StatusRequest {}),
    };
    let error = HelperEnvelope {
        sequence: 0,
        request_id,
        message: HelperMessage::Error(ProtocolError::new(
            ErrorClass::ProtocolMismatch,
            "exact Protocol V1 mismatch",
        )),
    };
    json!({
        "inventory_schema": 1,
        "protocol_version": PROTOCOL_VERSION,
        "projection_contract_version": PROJECTION_CONTRACT_VERSION,
        "fingerprint": {
            "algorithm": "sha256",
            "encoding": "lowercase_hex",
            "scope": "exact_compact_utf8_bytes_of_this_inventory_without_a_fingerprint_value",
            "runtime_value": FINGERPRINT_PLACEHOLDER
        },
        "framing": {
            "magic_ascii": std::str::from_utf8(FRAME_MAGIC).unwrap_or("CTXPRO"),
            "header_bytes": FRAME_HEADER_BYTES,
            "version_bytes": 2,
            "length_bytes": 4,
            "byte_order": "big_endian",
            "payload": "strict_json",
            "maximum_payload_bytes": MAX_FRAME_PAYLOAD_BYTES
        },
        "bounds": {
            "journal_records_per_batch": MAX_JOURNAL_RECORDS_PER_BATCH,
            "journal_payload_bytes": MAX_JOURNAL_PAYLOAD_BYTES,
            "journal_evidence_per_record": MAX_JOURNAL_EVIDENCE_PER_RECORD,
            "journal_identity_bytes": MAX_JOURNAL_IDENTITY_BYTES,
            "authorized_repository_roots": MAX_AUTHORIZED_REPOSITORY_ROOTS,
            "authorized_repository_root_bytes": MAX_AUTHORIZED_REPOSITORY_ROOT_BYTES,
            "authorized_repository_roots_total_bytes": MAX_AUTHORIZED_REPOSITORY_ROOTS_TOTAL_BYTES,
            "journal_sync_envelope_bytes": MAX_JOURNAL_SYNC_ENVELOPE_BYTES,
            "result_content_items_per_request": MAX_RESULT_CONTENT_ITEMS_PER_REQUEST,
            "result_content_bytes_per_item": MAX_RESULT_CONTENT_BYTES_PER_ITEM,
            "result_content_total_bytes": MAX_RESULT_CONTENT_TOTAL_BYTES,
            "query_results": MAX_QUERY_RESULTS,
            "query_cursor_bytes": MAX_QUERY_CURSOR_BYTES,
            "citations_per_fact": MAX_CITATIONS_PER_FACT,
            "facts_per_query_record": MAX_FACTS_PER_QUERY_RECORD,
            "resource_selector_bytes": MAX_RESOURCE_SELECTOR_BYTES
        },
        "host_message_kinds": [
            "hello", "authorize", "prepare_graph_key_deletion",
            "confirm_graph_key_deletion", "status", "sync_journal", "query"
        ],
        "helper_message_kinds": [
            "hello", "authorized", "graph_key_deletion_prepared", "graph_key_deleted",
            "status", "journal_synced", "query", "error"
        ],
        "capabilities": wire_names(&capabilities, Capability::wire_name),
        "enums": {
            "entitlement_access_kind": ["trial", "active", "canceling_paid"],
            "entitlement_access_state": ["trial", "active", "canceling_paid", "offline_grace", "locked"],
            "entitlement_capability": ["graph_read", "graph_write", "export", "migrate", "update"],
            "error_class": [
                "entitlement_expired", "key_store_unavailable", "key_store_locked",
                "not_materialized", "protocol_mismatch", "missing_source",
                "missing_repository", "stale_fact", "ambiguous", "corrupt",
                "invalid_request", "bounds", "sequence", "internal"
            ],
            "fact_confidence": ["explicit", "high", "medium", "low", "unknown"],
            "fact_state": ["asserted", "ambiguous", "contradicted", "superseded"],
            "fact_value_kind": ["resource", "text", "integer", "boolean", "json"],
            "graph_state": ["not_materialized", "needs_rebuild", "partial", "needs_resume", "ready"],
            "journal_entity_kind": ["event", "file_touch", "vcs_change"],
            "journal_operation": ["upsert", "delete"],
            "journal_sync_mode": ["full_baseline", "incremental"],
            "observation_kind": ["event", "file_touch", "vcs_change"],
            "query_kind": ["show", "locate", "blame", "timeline", "related", "facts"],
            "resource_kind": ResourceKind::ALL.map(ResourceKind::wire_name)
        },
        "dto_fields": {
            "AuthorizationRequest": fields(
                &["entitlement", "installation_public_key_base64url", "challenge_base64url", "proof_signature_base64url"], &[]),
            "AuthorizationResult": fields(&[
                "state", "refresh_required", "expires_at_unix", "access_deadline_unix",
                "grace_deadline_unix", "capabilities"
            ], &[]),
            "ByteRange": fields(&["start", "end_exclusive"], &[]),
            "ConfirmGraphKeyDeletionRequest": fields(&["authorization"], &[]),
            "ContentRef": fields(&["sha256", "byte_len"], &[]),
            "EntitlementGrant": fields(&[
                "schema_version", "issuer", "key_id", "grant_id", "subject", "account_id",
                "product", "access_kind", "installation_key_thumbprint", "issued_at_unix",
                "not_before_unix", "refresh_after_unix", "access_deadline_unix",
                "grace_deadline_unix", "expires_at_unix", "minimum_helper_protocol",
                "revocation_epoch", "capabilities"
            ], &[]),
            "EvidenceCitation": fields(&[], &[
                "observation_id", "observation_seq", "observation_kind", "session_id", "event_id",
                "event_seq", "source_path", "fixture_line", "source_record_ordinal",
                "source_record_subrecord_index", "byte_range", "source_sha256"
            ]),
            "FactRecord": fields(&[
                "id", "fact_type", "subject", "predicate", "object", "confidence", "state",
                "detector_version", "citations"
            ], &["owning_root_session_id", "direct_actor_session_id"]),
            "GraphKeyDeleted": fields(&["deleted"], &[]),
            "GraphKeyDeletionPrepared": fields(&["challenge_base64url", "expires_at_unix", "key_present"], &[]),
            "HelloRequest": fields(&["protocol_version", "protocol_fingerprint", "host_version", "capabilities"], &[]),
            "HelloResult": fields(&[
                "protocol_version", "protocol_fingerprint", "helper_version", "capabilities",
                "authorization_challenge_base64url"
            ], &[]),
            "HelperEnvelope": fields(&["sequence", "request_id", "message"], &[]),
            "HostEnvelope": fields(&["sequence", "request_id", "message"], &[]),
            "JournalCheckpoint": fields(&["position", "contract_fingerprint", "cumulative_digest"], &[]),
            "JournalEvidenceIdentity": fields(&["event_id"], &[
                "source_id", "source_path", "source_record_ordinal", "source_record_subrecord_index",
                "byte_start", "byte_end_exclusive"
            ]),
            "JournalPosition": fields(&["generation", "sequence"], &[]),
            "JournalProvenanceIdentity": fields(&["entity_kind", "stable_entity_id"], &[
                "capture_source_id", "provider", "provider_external_id"
            ]),
            "JournalRecord": fields(&[
                "generation", "sequence", "projection_contract_version", "entity_kind",
                "stable_entity_id", "entity_revision", "operation", "payload_sha256",
                "evidence", "provenance", "cumulative_digest"
            ], &["canonical_payload"]),
            "JournalSyncRequest": fields(&[
                "mode", "canonical_schema_version", "canonical_schema_identity",
                "projection_contract_version", "contract_fingerprint", "prior_checkpoint",
                "frozen_through", "authorized_repository_roots", "records", "result_contents"
            ], &[]),
            "JournalSyncResult": fields(&[
                "committed_through", "accepted_records", "replayed", "frozen_complete"
            ], &[]),
            "PrepareGraphKeyDeletionRequest": fields(&["installation_key_thumbprint"], &[]),
            "ResultContentSidecar": fields(&[
                "journal_sequence", "stable_entity_id", "content_ref", "content"
            ], &[]),
            "ProtocolError": fields(&["class", "message", "retryable"], &[]),
            "QueryRecord": fields(&["resource", "facts", "citations"], &["summary", "occurred_at_ms"]),
            "QueryRequest": fields(&["kind", "target", "limit", "expected_snapshot"], &["cursor"]),
            "QueryResult": fields(&["records", "truncated", "stale"], &["next_cursor"]),
            "QuerySnapshotExpectation": fields(&["checkpoint", "projection_pending"], &[]),
            "ResourceRef": fields(&["id", "kind", "display"], &[]),
            "ResourceSelector": fields(&["kind", "value"], &["repository", "line"]),
            "SignedEntitlement": fields(&["grant", "signature_base64url"], &[]),
            "StatusRequest": fields(&[], &[]),
            "StatusResult": fields(&["state"], &["checkpoint"])
        },
        "canonical_payload": {
            "wire_type": "optional_json_value",
            "encoding": "compact_json_with_recursively_sorted_object_keys_and_integer_only_numbers",
            "digest": "sha256_of_canonical_compact_utf8_bytes_or_empty_bytes_for_delete",
            "tombstone": "delete_requires_absent_payload_and_positive_entity_revision"
        },
        "canonical_payload_schema": {
            "root": "CanonicalObservation",
            "dto_fields": {
                "CanonicalObservation": fields(&[
                    "observation_id", "observation_seq", "observation_kind", "event_id",
                    "event_seq", "occurred_at_ms", "history_record_id", "event_type", "role",
                    "payload", "metadata", "result", "actor", "run", "source", "typed_event",
                    "file_touch", "vcs_change", "citation", "semantic_digest"
                ], &[]),
                "CanonicalActor": fields(&[
                    "direct_session_id", "root_session_id", "parent_session_id",
                    "external_session_id", "external_agent_id", "agent_type", "role_hint",
                    "is_primary"
                ], &[]),
                "CanonicalRun": fields(&[
                    "id", "run_type", "status", "started_at_ms", "ended_at_ms", "exit_code",
                    "cwd", "command_preview"
                ], &[]),
                "CanonicalSource": fields(&[
                    "id", "provider", "path", "format", "root", "identity", "cwd",
                    "imported_observation", "permitted_bytes"
                ], &[]),
                "CanonicalSourceObservation": fields(
                    &["byte_size", "modified_at_ms", "sha256"], &[]),
                "CanonicalFileTouch": fields(&[
                    "id", "history_record_id", "run_id", "event_id", "vcs_workspace_id", "path",
                    "change_kind", "old_path", "line_count_delta", "confidence", "source_id"
                ], &[]),
                "CanonicalVcsChange": fields(&[
                    "id", "vcs_workspace_id", "kind", "change_id", "parent_change_ids",
                    "branch_or_bookmark", "tree_hash", "author_time_ms", "confidence", "source_id"
                ], &[]),
                "CanonicalCitation": fields(&[
                    "observation_id", "observation_seq", "observation_kind", "event_id", "event_seq",
                    "source_path", "fixture_line", "source_record_ordinal",
                    "source_record_subrecord_index", "byte_range", "source_sha256"
                ], &[]),
                "CanonicalResultEvidence": fields(&["outcome", "identifiers", "content_ref"], &[]),
                "CanonicalResultIdentifier": fields(&["kind", "value"], &[])
            },
            "enums": {
                "typed_event_kind": ["file_touched", "vcs_change"],
                "file_change_kind": ["read", "created", "modified", "deleted", "renamed", "unknown"],
                "confidence": ["explicit", "high", "medium", "low", "unknown"],
                "vcs_change_kind": ["git_commit", "git_branch", "git_worktree", "jj_change", "jj_bookmark", "patch", "working_copy"],
                "result_outcome": ["success", "failure", "unknown"],
                "result_evidence_kind": ["call_id", "git_commit_summary_id", "git_oid", "git_abbrev_oid", "forge_url"]
            },
            "identity_rules": {
                "uuid": "non_nil_uuid",
                "optional_identity_bytes": MAX_JOURNAL_IDENTITY_BYTES,
                "source_reads": "forbidden; source root/cwd/imported observation/permitted bytes are absent or null",
                "subrecord": "source_record_subrecord_index_requires_source_record_ordinal"
            }
        },
        "journal_chain": {
            "initial_domain_hex": hex(b"ctx-pro-journal-initial-v1\0"),
            "record_domain_hex": hex(b"ctx-pro-journal-record-v1\0"),
            "new_generation": "full_baseline_starts_at_sequence_1_after_sequence_0_checkpoint",
            "ordering": "strictly_contiguous",
            "frozen_terminal": "position_and_cumulative_digest_captured_with_records",
            "checkpoint_commit": "only_after_private_graph_transaction_commits"
        },
        "transient_result_content": {
            "durability": "request_only_excluded_from_canonical_payload_record_and_cumulative_digests",
            "binding": "unique_record_binding_with_stable_entity_id_and_exact_canonical_content_ref",
            "integrity": "complete_utf8_bytes_must_match_content_ref_sha256_and_byte_len",
            "oversize_policy": "omit_entire_item_without_prefix_or_tail"
        },
        "representative_frames": {
            "host_status": frame_hex(&status),
            "helper_protocol_mismatch": frame_hex(&error)
        }
    })
}

fn main() {
    let bytes =
        serde_json::to_vec(&inventory()).unwrap_or_else(|error| panic!("inventory: {error}"));
    let digest = hex(&Sha256::digest(&bytes));
    let output = json!({
        "canonical_inventory": serde_json::from_slice::<Value>(&bytes)
            .unwrap_or_else(|error| panic!("canonical inventory: {error}")),
        "canonical_sha256": digest,
        "golden_vectors": golden_vectors()
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output)
            .unwrap_or_else(|error| panic!("format inventory: {error}"))
    );
}
