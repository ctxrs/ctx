#![recursion_limit = "512"]

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
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

fn inventory_initial_journal_digest(generation: u64, fingerprint: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"ctx-pro-journal-initial-v1\0");
    hash.update(generation.to_be_bytes());
    hash.update(fingerprint.as_bytes());
    hex(&hash.finalize())
}

fn checkpoint(fingerprint: &str) -> JournalCheckpoint {
    JournalCheckpoint {
        position: JournalPosition {
            generation: 1,
            sequence: 0,
        },
        contract_fingerprint: fingerprint.to_owned(),
        cumulative_digest: inventory_initial_journal_digest(1, fingerprint),
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

fn journal_request(roots: Vec<String>, fingerprint: &str) -> JournalSyncRequest {
    let checkpoint = checkpoint(fingerprint);
    JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: 47,
        canonical_schema_identity: "ctx-store-schema-47-final-v3".to_owned(),
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: fingerprint.to_owned(),
        prior_checkpoint: checkpoint.clone(),
        frozen_through: checkpoint,
        authorized_repository_roots: roots,
        records: Vec::new(),
    }
}

fn canonical_observation_payload(
    entity_kind: JournalEntityKind,
    stable_entity_id: Uuid,
    sequence: u64,
) -> Value {
    let (observation_kind, event_type, typed_event, file_touch, vcs_change) = match entity_kind {
        JournalEntityKind::Event => (
            "event",
            "fixture_event",
            Value::Null,
            Value::Null,
            Value::Null,
        ),
        JournalEntityKind::FileTouch => (
            "file_touch",
            "file_touched",
            json!("file_touched"),
            json!({
                "id": stable_entity_id,
                "history_record_id": null,
                "run_id": null,
                "event_id": null,
                "vcs_workspace_id": null,
                "path": "src/lib.rs",
                "change_kind": "modified",
                "old_path": null,
                "line_count_delta": 1,
                "confidence": "explicit",
                "source_id": null
            }),
            Value::Null,
        ),
        JournalEntityKind::VcsChange => (
            "vcs_change",
            "vcs_change",
            json!("vcs_change"),
            Value::Null,
            json!({
                "id": stable_entity_id,
                "vcs_workspace_id": Uuid::from_u128(900 + u128::from(sequence)),
                "kind": "git_commit",
                "change_id": "0123456789abcdef0123456789abcdef01234567",
                "parent_change_ids": [],
                "branch_or_bookmark": "main",
                "tree_hash": "89abcdef0123456789abcdef0123456789abcdef",
                "author_time_ms": 1_753_232_400_000_i64,
                "confidence": "explicit",
                "source_id": null
            }),
        ),
    };
    let event_id = matches!(entity_kind, JournalEntityKind::Event).then_some(stable_entity_id);
    json!({
        "observation_id": stable_entity_id,
        "observation_seq": sequence,
        "observation_kind": observation_kind,
        "event_id": event_id,
        "event_seq": event_id.map(|_| sequence),
        "occurred_at_ms": 1_753_232_400_000_i64 + sequence as i64,
        "history_record_id": null,
        "event_type": event_type,
        "role": null,
        "payload": {},
        "metadata": {},
        "result": {
            "outcome": "unknown",
            "identifiers": [],
            "content_ref": null
        },
        "actor": null,
        "run": null,
        "source": null,
        "typed_event": typed_event,
        "file_touch": file_touch,
        "vcs_change": vcs_change,
        "citation": {
            "observation_id": stable_entity_id,
            "observation_seq": sequence,
            "observation_kind": observation_kind,
            "event_id": event_id,
            "event_seq": event_id.map(|_| sequence),
            "source_path": "fixture/session.jsonl",
            "fixture_line": sequence,
            "source_record_ordinal": sequence - 1,
            "source_record_subrecord_index": 0,
            "byte_range": {
                "start": sequence * 100,
                "end_exclusive": sequence * 100 + 80
            },
            "source_sha256": null
        },
        "semantic_digest": format!("{:064x}", 10_000 + sequence)
    })
}

fn journal_record(
    generation: u64,
    sequence: u64,
    entity_kind: JournalEntityKind,
    operation: JournalOperation,
    prior_digest: &str,
) -> JournalRecord {
    let stable_entity_id = Uuid::from_u128(100 + u128::from(sequence));
    let canonical_payload = match operation {
        JournalOperation::Upsert => Some(canonical_observation_payload(
            entity_kind,
            stable_entity_id,
            sequence,
        )),
        JournalOperation::Delete => None,
    };
    let payload_bytes = canonical_payload
        .as_ref()
        .map(canonical_payload_bytes)
        .transpose()
        .unwrap_or_else(|error| panic!("canonical fixture payload: {error:?}"))
        .unwrap_or_default();
    let mut record = JournalRecord {
        generation,
        sequence,
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        entity_kind,
        stable_entity_id,
        entity_revision: if matches!(operation, JournalOperation::Delete) {
            2
        } else {
            1
        },
        operation,
        canonical_payload,
        payload_sha256: sha256_hex(&payload_bytes),
        evidence: vec![JournalEvidenceIdentity {
            event_id: Uuid::from_u128(200 + u128::from(sequence)),
            source_id: Some(Uuid::from_u128(300 + u128::from(sequence))),
            source_path: Some("fixture/session.jsonl".to_owned()),
            source_record_ordinal: Some(sequence),
            source_record_subrecord_index: Some(0),
            byte_start: Some(sequence * 10),
            byte_end_exclusive: Some(sequence * 10 + 9),
        }],
        provenance: JournalProvenanceIdentity {
            entity_kind,
            stable_entity_id,
            capture_source_id: Some(Uuid::from_u128(300 + u128::from(sequence))),
            provider: Some("codex".to_owned()),
            provider_external_id: Some(format!("fixture-{sequence}")),
        },
        cumulative_digest: "0".repeat(64),
    };
    record.cumulative_digest = journal_record_digest(prior_digest, &record)
        .unwrap_or_else(|error| panic!("journal fixture digest: {error:?}"));
    record
}

fn journal_operation_requests(fingerprint: &str) -> [JournalSyncRequest; 2] {
    let generation = 2;
    let initial = inventory_initial_journal_digest(generation, fingerprint);
    let event = journal_record(
        generation,
        1,
        JournalEntityKind::Event,
        JournalOperation::Upsert,
        &initial,
    );
    let file_touch = journal_record(
        generation,
        2,
        JournalEntityKind::FileTouch,
        JournalOperation::Upsert,
        &event.cumulative_digest,
    );
    let vcs_change = journal_record(
        generation,
        3,
        JournalEntityKind::VcsChange,
        JournalOperation::Upsert,
        &file_touch.cumulative_digest,
    );
    let full_checkpoint = JournalCheckpoint {
        position: JournalPosition {
            generation,
            sequence: 3,
        },
        contract_fingerprint: fingerprint.to_owned(),
        cumulative_digest: vcs_change.cumulative_digest.clone(),
    };
    let full = JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: 47,
        canonical_schema_identity: "ctx-store-schema-47-final-v3".to_owned(),
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: fingerprint.to_owned(),
        prior_checkpoint: JournalCheckpoint {
            position: JournalPosition {
                generation,
                sequence: 0,
            },
            contract_fingerprint: fingerprint.to_owned(),
            cumulative_digest: initial,
        },
        frozen_through: full_checkpoint.clone(),
        authorized_repository_roots: vec!["/workspace/ctx".to_owned()],
        records: vec![event, file_touch, vcs_change],
    };
    let delete = journal_record(
        generation,
        4,
        JournalEntityKind::Event,
        JournalOperation::Delete,
        &full_checkpoint.cumulative_digest,
    );
    let incremental = JournalSyncRequest {
        mode: JournalSyncMode::Incremental,
        canonical_schema_version: 47,
        canonical_schema_identity: "ctx-store-schema-47-final-v3".to_owned(),
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: fingerprint.to_owned(),
        prior_checkpoint: full_checkpoint,
        frozen_through: JournalCheckpoint {
            position: JournalPosition {
                generation,
                sequence: 4,
            },
            contract_fingerprint: fingerprint.to_owned(),
            cumulative_digest: delete.cumulative_digest.clone(),
        },
        authorized_repository_roots: vec!["/workspace/ctx".to_owned()],
        records: vec![delete],
    };
    [full, incremental]
}

fn blame_request(target: BlameTarget, cursor: Option<String>, fingerprint: &str) -> BlameRequest {
    BlameRequest {
        target,
        limit: 10,
        cursor,
        expected_snapshot: QuerySnapshotExpectation {
            checkpoint: checkpoint(fingerprint),
            projection_pending: false,
        },
    }
}

fn blame(cursor: Option<String>, fingerprint: &str) -> BlameRequest {
    blame_request(
        BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
        },
        cursor,
        fingerprint,
    )
}

fn output_source() -> OutputSourceIdentity {
    OutputSourceIdentity {
        provider: "codex".to_owned(),
        namespace_id: "codex-session-jsonl".to_owned(),
        source_id: "fixture/session.jsonl".to_owned(),
    }
}

fn output_cursor() -> OutputNativeCursor {
    OutputNativeCursor {
        version: 1,
        payload_base64: "Y3Vyc29yLTE=".to_owned(),
    }
}

fn output_page() -> ProOutputMaterializationPage {
    ProOutputMaterializationPage {
        contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
        inventory_generation: 1,
        source: output_source(),
        source_epoch: 0,
        observed_revision: "revision-1".to_owned(),
        parser_revision: "parser-1".to_owned(),
        materializer_revision: "materializer-1".to_owned(),
        disposition: OutputSourceDisposition::NewSource,
        expected_prior_source_epoch: None,
        expected_prior_cursor: None,
        next_safe_cursor: output_cursor(),
        terminal: true,
        observations: vec![ProOutputObservation {
            kind: OutputObservationKind::Command,
            coordinate: OutputNativeCoordinate {
                unit_key: "output-1".to_owned(),
                native_sequence: 1,
                native_record_id: Some("record-1".to_owned()),
                source_record_ordinal: Some(0),
                source_record_subrecord_index: Some(0),
                byte_start: Some(0),
                byte_end_exclusive: Some(15),
            },
            occurred_at_unix_ms: Some(1_753_232_400_000),
            associations: OutputAssociations {
                direct_session_id: "session-1".to_owned(),
                root_session_id: "session-1".to_owned(),
                parent_session_id: None,
                provider_session_id: Some("provider-session-1".to_owned()),
                agent_id: Some("agent-1".to_owned()),
                repository: None,
            },
            call_id: Some("call-1".to_owned()),
            command: Some(OutputCommandContext {
                tool_name: "exec_command".to_owned(),
                command: "cargo check".to_owned(),
                working_directory: Some("/workspace/ctx".to_owned()),
            }),
            outcome: OutputOutcomeMetadata {
                outcome: OutputOutcome::Success,
                exit_code: Some(0),
                duration_ms: Some(42),
            },
            locator: OutputSourceLocator {
                version: 1,
                kind: "jsonl-byte-range".to_owned(),
                payload_base64: "bG9jYXRvci0x".to_owned(),
            },
            content: TransientOutputContent::from_bytes(b"complete output")
                .unwrap_or_else(|| panic!("small output fixture")),
        }],
    }
}

fn structured_pr_output_coordinate() -> OutputNativeCoordinate {
    OutputNativeCoordinate {
        unit_key: "structured-pr-membership-42".to_owned(),
        native_sequence: 42,
        native_record_id: Some("command-result-42".to_owned()),
        source_record_ordinal: Some(41),
        source_record_subrecord_index: Some(0),
        byte_start: Some(1_024),
        byte_end_exclusive: Some(1_280),
    }
}

fn structured_pr_output_locator() -> OutputSourceLocator {
    OutputSourceLocator {
        version: 1,
        kind: "jsonl-byte-range".to_owned(),
        payload_base64: "c3RydWN0dXJlZC1wci00Mg==".to_owned(),
    }
}

fn output_observation(
    kind: OutputObservationKind,
    sequence: u64,
    outcome: OutputOutcome,
    content: &[u8],
) -> ProOutputObservation {
    ProOutputObservation {
        kind,
        coordinate: OutputNativeCoordinate {
            unit_key: format!("output-operation-{sequence}"),
            native_sequence: sequence,
            native_record_id: Some(format!("record-{sequence}")),
            source_record_ordinal: Some(sequence),
            source_record_subrecord_index: Some(0),
            byte_start: Some(sequence * 100),
            byte_end_exclusive: Some(sequence * 100 + content.len() as u64),
        },
        occurred_at_unix_ms: Some(1_753_232_400_000 + sequence as i64),
        associations: OutputAssociations {
            direct_session_id: "session-1".to_owned(),
            root_session_id: "session-1".to_owned(),
            parent_session_id: None,
            provider_session_id: Some("provider-session-1".to_owned()),
            agent_id: Some("agent-1".to_owned()),
            repository: Some(OutputRepositoryContext {
                repository_id: "repository:ctxrs/ctx".to_owned(),
                checkout_id: Some("checkout:ctxrs/ctx".to_owned()),
                worktree_id: Some("worktree:ctxrs/ctx".to_owned()),
                object_format: Some("sha1".to_owned()),
            }),
        },
        call_id: Some(format!("call-{sequence}")),
        command: None,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code: match outcome {
                OutputOutcome::Success => Some(0),
                OutputOutcome::Failure => Some(1),
                OutputOutcome::Timeout | OutputOutcome::Unknown => None,
            },
            duration_ms: Some(40 + sequence),
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: "jsonl-byte-range".to_owned(),
            payload_base64: base64::engine::general_purpose::STANDARD
                .encode(format!("locator-{sequence}")),
        },
        content: TransientOutputContent::from_bytes(content)
            .unwrap_or_else(|| panic!("small operation output fixture")),
    }
}

fn structured_pr_output_observation() -> ProOutputObservation {
    let mut observation = output_observation(
        OutputObservationKind::Command,
        42,
        OutputOutcome::Success,
        br#"{"url":"https://github.com/ctxrs/ctx/pull/42","commits":[{"oid":"0123456789abcdef0123456789abcdef01234567"}],"mergeCommit":{"oid":"89abcdef0123456789abcdef0123456789abcdef"}}"#,
    );
    observation.coordinate = structured_pr_output_coordinate();
    observation.locator = structured_pr_output_locator();
    observation.command = Some(OutputCommandContext {
        tool_name: "gh".to_owned(),
        command: "gh pr view 42 --repo ctxrs/ctx --json url,commits,mergeCommit".to_owned(),
        working_directory: Some("/workspace/ctx".to_owned()),
    });
    observation
}

fn output_operation_pages() -> [ProOutputMaterializationPage; 3] {
    let new_source = ProOutputMaterializationPage {
        contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
        inventory_generation: 2,
        source: output_source(),
        source_epoch: 0,
        observed_revision: "revision-structured-pr".to_owned(),
        parser_revision: "parser-1".to_owned(),
        materializer_revision: "materializer-1".to_owned(),
        disposition: OutputSourceDisposition::NewSource,
        expected_prior_source_epoch: None,
        expected_prior_cursor: None,
        next_safe_cursor: OutputNativeCursor {
            version: 1,
            payload_base64: "bmV3LXNvdXJjZS1jdXJzb3I=".to_owned(),
        },
        terminal: false,
        observations: vec![structured_pr_output_observation()],
    };
    let append_or_resume = ProOutputMaterializationPage {
        contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
        inventory_generation: 2,
        source: output_source(),
        source_epoch: 0,
        observed_revision: "revision-structured-pr".to_owned(),
        parser_revision: "parser-1".to_owned(),
        materializer_revision: "materializer-1".to_owned(),
        disposition: OutputSourceDisposition::AppendOrResume,
        expected_prior_source_epoch: Some(0),
        expected_prior_cursor: Some(OutputNativeCursor {
            version: 1,
            payload_base64: "bmV3LXNvdXJjZS1jdXJzb3I=".to_owned(),
        }),
        next_safe_cursor: OutputNativeCursor {
            version: 1,
            payload_base64: "cmVzdW1lZC1jdXJzb3I=".to_owned(),
        },
        terminal: false,
        observations: vec![output_observation(
            OutputObservationKind::Tool,
            43,
            OutputOutcome::Failure,
            b"structured forge query failed",
        )],
    };
    let rewrite = ProOutputMaterializationPage {
        contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
        inventory_generation: 2,
        source: output_source(),
        source_epoch: 1,
        observed_revision: "revision-rewritten".to_owned(),
        parser_revision: "parser-2".to_owned(),
        materializer_revision: "materializer-2".to_owned(),
        disposition: OutputSourceDisposition::Rewrite,
        expected_prior_source_epoch: Some(0),
        expected_prior_cursor: Some(OutputNativeCursor {
            version: 1,
            payload_base64: "cmVzdW1lZC1jdXJzb3I=".to_owned(),
        }),
        next_safe_cursor: OutputNativeCursor {
            version: 1,
            payload_base64: "cmV3cml0dGVuLWN1cnNvcg==".to_owned(),
        },
        terminal: true,
        observations: vec![
            output_observation(
                OutputObservationKind::Command,
                44,
                OutputOutcome::Timeout,
                b"structured forge query timed out",
            ),
            output_observation(
                OutputObservationKind::Tool,
                45,
                OutputOutcome::Unknown,
                b"structured forge query outcome unavailable",
            ),
        ],
    };
    [new_source, append_or_resume, rewrite]
}

fn provider_output_citation() -> EvidenceCitation {
    EvidenceCitation {
        observation_id: None,
        observation_seq: None,
        observation_kind: None,
        session_id: None,
        event_id: None,
        event_seq: None,
        source_path: None,
        fixture_line: None,
        source_record_ordinal: None,
        source_record_subrecord_index: None,
        byte_range: None,
        source_sha256: None,
        provider_output: Some(ProviderOutputEvidence {
            source_id: "fixture/session.jsonl".to_owned(),
            source_epoch: 0,
            locator: OutputSourceLocator {
                version: 1,
                kind: "jsonl-byte-range".to_owned(),
                payload_base64: "bG9jYXRvci0x".to_owned(),
            },
            coordinate: OutputNativeCoordinate {
                unit_key: "output-1".to_owned(),
                native_sequence: 1,
                native_record_id: Some("record-1".to_owned()),
                source_record_ordinal: Some(0),
                source_record_subrecord_index: Some(0),
                byte_start: Some(0),
                byte_end_exclusive: Some(15),
            },
            availability: OutputSourceAvailability::Available,
        }),
    }
}

fn structured_pr_provider_output_citation() -> EvidenceCitation {
    EvidenceCitation {
        observation_id: None,
        observation_seq: None,
        observation_kind: None,
        session_id: None,
        event_id: None,
        event_seq: None,
        source_path: None,
        fixture_line: None,
        source_record_ordinal: None,
        source_record_subrecord_index: None,
        byte_range: None,
        source_sha256: None,
        provider_output: Some(ProviderOutputEvidence {
            source_id: output_source().source_id,
            source_epoch: 0,
            locator: structured_pr_output_locator(),
            coordinate: structured_pr_output_coordinate(),
            availability: OutputSourceAvailability::Available,
        }),
    }
}
fn provider_output_blame_result() -> BlameResult {
    BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: ResourceRef {
                id: "commit:fixture-1".to_owned(),
                kind: ResourceKind::Commit,
                display: "0123456789abcdef".to_owned(),
            },
            repository: ResourceRef {
                id: "repository:fixture-1".to_owned(),
                kind: ResourceKind::Repository,
                display: "ctxrs/ctx".to_owned(),
            },
        },
        git_snapshot: None,
        matches: vec![BlameMatch::Commit(CommitBlameMatch {
            fact_id: "fact:fixture-1".to_owned(),
            fact_type: CommitFactType::Referenced,
            predicate: CommitPredicate::ReferencedBy,
            subject: ResourceRef {
                id: "commit:fixture-1".to_owned(),
                kind: ResourceKind::Commit,
                display: "0123456789abcdef".to_owned(),
            },
            object: Some(ResourceRef {
                id: "session:fixture-1".to_owned(),
                kind: ResourceKind::Session,
                display: "session-1".to_owned(),
            }),
            fact_occurred_at_ms: Some(1_753_232_400_000),
            confidence: FactConfidence::Explicit,
            state: FactState::Asserted,
            direct_actor: None,
            owning_root: None,
            evidence_numbers: vec![1],
        })],
        evidence: vec![NumberedEvidence {
            number: 1,
            citation: provider_output_citation(),
        }],
        next: None,
    }
}

fn resource(kind: ResourceKind, suffix: &str, display: &str) -> ResourceRef {
    ResourceRef {
        id: format!("{}:{suffix}", kind.wire_name()),
        kind,
        display: display.to_owned(),
    }
}

fn canonical_citation(seed: u32, observation_kind: ObservationKind) -> EvidenceCitation {
    EvidenceCitation {
        observation_id: Some(Uuid::from_u128(1_000 + u128::from(seed))),
        observation_seq: Some(u64::from(seed)),
        observation_kind: Some(observation_kind),
        session_id: Some(Uuid::from_u128(2_000 + u128::from(seed))),
        event_id: Some(Uuid::from_u128(3_000 + u128::from(seed))),
        event_seq: Some(u64::from(seed)),
        source_path: Some("fixture/session.jsonl".to_owned()),
        fixture_line: Some(u64::from(seed)),
        source_record_ordinal: Some(u64::from(seed - 1)),
        source_record_subrecord_index: Some(0),
        byte_range: Some(ByteRange {
            start: u64::from(seed) * 100,
            end_exclusive: u64::from(seed) * 100 + 80,
        }),
        source_sha256: Some(format!("{seed:064x}")),
        provider_output: None,
    }
}

fn production_attribution(
    suffix: &str,
    relationship: ProductionRelationship,
    evidence_number: u32,
) -> AgentAttribution {
    let (confidence, state) = match relationship {
        ProductionRelationship::ProducedBy => (FactConfidence::Explicit, FactState::Asserted),
        ProductionRelationship::PossiblyProducedBy => {
            (FactConfidence::Ambiguous, FactState::Ambiguous)
        }
    };
    AgentAttribution {
        id: format!("attribution:{suffix}"),
        relationship,
        producing_session: resource(ResourceKind::Session, suffix, &format!("session-{suffix}")),
        direct_actor: Some(resource(
            ResourceKind::Agent,
            suffix,
            &format!("agent-{suffix}"),
        )),
        owning_root: Some(resource(
            ResourceKind::Session,
            &format!("root-{suffix}"),
            &format!("root-session-{suffix}"),
        )),
        confidence,
        state,
        evidence_numbers: vec![evidence_number],
    }
}

fn file_blame_result(
    requested_lines: Option<LineRange>,
    matched_lines: LineRange,
    worktree_status: WorktreeStatus,
    relationship: ProductionRelationship,
    next: Option<BlameContinuation>,
) -> BlameResult {
    BlameResult {
        target: ResolvedBlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: resource(ResourceKind::Repository, "ctxrs-ctx", "ctxrs/ctx"),
            requested_lines,
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worktree_status,
        }),
        matches: vec![BlameMatch::File(FileBlameMatch {
            id: format!("file-match-{}-{}", matched_lines.start, matched_lines.end),
            lines: matched_lines,
            commit: resource(
                ResourceKind::Commit,
                "file-head",
                "0123456789abcdef0123456789abcdef01234567",
            ),
            line_evidence_numbers: vec![1],
            production: vec![production_attribution("file-head", relationship, 2)],
        })],
        evidence: vec![
            NumberedEvidence {
                number: 1,
                citation: canonical_citation(1, ObservationKind::FileTouch),
            },
            NumberedEvidence {
                number: 2,
                citation: canonical_citation(2, ObservationKind::VcsChange),
            },
        ],
        next,
    }
}

fn commit_blame_result() -> BlameResult {
    let commit = resource(
        ResourceKind::Commit,
        "commit-query",
        "0123456789abcdef0123456789abcdef01234567",
    );
    let variants = [
        (
            CommitFactType::Produced,
            CommitPredicate::ProducedBy,
            ResourceKind::Session,
            FactConfidence::Explicit,
            FactState::Asserted,
        ),
        (
            CommitFactType::Amended,
            CommitPredicate::AmendedBy,
            ResourceKind::Session,
            FactConfidence::High,
            FactState::Asserted,
        ),
        (
            CommitFactType::CherryPicked,
            CommitPredicate::CherryPickedFrom,
            ResourceKind::Commit,
            FactConfidence::Medium,
            FactState::Asserted,
        ),
        (
            CommitFactType::Reverted,
            CommitPredicate::Reverts,
            ResourceKind::Commit,
            FactConfidence::Low,
            FactState::Contradicted,
        ),
        (
            CommitFactType::Pushed,
            CommitPredicate::PushedBy,
            ResourceKind::Session,
            FactConfidence::Ambiguous,
            FactState::Ambiguous,
        ),
        (
            CommitFactType::Inspected,
            CommitPredicate::InspectedBy,
            ResourceKind::Session,
            FactConfidence::Unknown,
            FactState::Superseded,
        ),
        (
            CommitFactType::Referenced,
            CommitPredicate::ReferencedBy,
            ResourceKind::Session,
            FactConfidence::Explicit,
            FactState::Asserted,
        ),
        (
            CommitFactType::Ambiguous,
            CommitPredicate::PossiblyProducedBy,
            ResourceKind::Session,
            FactConfidence::Ambiguous,
            FactState::Ambiguous,
        ),
    ];
    let matches = variants
        .into_iter()
        .enumerate()
        .map(
            |(index, (fact_type, predicate, object_kind, confidence, state))| {
                let number = u32::try_from(index + 1)
                    .unwrap_or_else(|_| panic!("small commit fixture index"));
                BlameMatch::Commit(CommitBlameMatch {
                    fact_id: format!("commit-fact-{number}"),
                    fact_type,
                    predicate,
                    subject: commit.clone(),
                    object: Some(resource(
                        object_kind,
                        &format!("commit-object-{number}"),
                        &format!("commit-object-{number}"),
                    )),
                    fact_occurred_at_ms: Some(1_753_232_400_000 + i64::from(number)),
                    confidence,
                    state,
                    direct_actor: Some(resource(
                        ResourceKind::Agent,
                        &format!("commit-actor-{number}"),
                        &format!("agent-{number}"),
                    )),
                    owning_root: Some(resource(
                        ResourceKind::Session,
                        &format!("commit-root-{number}"),
                        &format!("root-session-{number}"),
                    )),
                    evidence_numbers: vec![number],
                })
            },
        )
        .collect();
    let evidence = (1..=8)
        .map(|number| NumberedEvidence {
            number,
            citation: canonical_citation(
                number,
                match number % 3 {
                    0 => ObservationKind::Event,
                    1 => ObservationKind::FileTouch,
                    _ => ObservationKind::VcsChange,
                },
            ),
        })
        .collect();
    BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit,
            repository: resource(ResourceKind::Repository, "ctxrs-ctx", "ctxrs/ctx"),
        },
        git_snapshot: None,
        matches,
        evidence,
        next: Some(BlameContinuation {
            cursor: "commit-next".to_owned(),
            reason: ContinuationReason::MoreMatches,
        }),
    }
}

fn pull_request_activity_result() -> BlameResult {
    let pull_request = resource(
        ResourceKind::PullRequest,
        "github-ctxrs-ctx-42",
        "https://github.com/ctxrs/ctx/pull/42",
    );
    let actions = [
        PullRequestAction::Referenced,
        PullRequestAction::Created,
        PullRequestAction::Reviewed,
        PullRequestAction::Commented,
        PullRequestAction::Merged,
        PullRequestAction::Edited,
        PullRequestAction::Closed,
        PullRequestAction::Reopened,
    ];
    let matches = actions
        .into_iter()
        .enumerate()
        .map(|(index, action)| {
            let number =
                u32::try_from(index + 1).unwrap_or_else(|_| panic!("small PR fixture index"));
            BlameMatch::PullRequest(PullRequestBlameMatch {
                pull_request: pull_request.clone(),
                relationship: PullRequestBlameRelationship::Activity(PullRequestActivity {
                    fact_id: format!("pr-activity-{number}"),
                    action,
                    session: resource(
                        ResourceKind::Session,
                        &format!("pr-activity-{number}"),
                        &format!("session-{number}"),
                    ),
                    direct_actor: Some(resource(
                        ResourceKind::Agent,
                        &format!("pr-actor-{number}"),
                        &format!("agent-{number}"),
                    )),
                    owning_root: Some(resource(
                        ResourceKind::Session,
                        &format!("pr-root-{number}"),
                        &format!("root-session-{number}"),
                    )),
                    fact_occurred_at_ms: Some(1_753_232_500_000 + i64::from(number)),
                    confidence: FactConfidence::Explicit,
                    state: FactState::Asserted,
                    evidence_numbers: vec![number],
                }),
            })
        })
        .collect();
    let evidence = (1..=8)
        .map(|number| NumberedEvidence {
            number,
            citation: canonical_citation(number, ObservationKind::Event),
        })
        .collect();
    BlameResult {
        target: ResolvedBlameTarget::PullRequest {
            selector: "42".to_owned(),
            pull_request,
            repository: resource(ResourceKind::Repository, "ctxrs-ctx", "ctxrs/ctx"),
        },
        git_snapshot: None,
        matches,
        evidence,
        next: None,
    }
}

fn pull_request_membership_result() -> BlameResult {
    let pull_request = resource(
        ResourceKind::PullRequest,
        "github-ctxrs-ctx-42",
        "https://github.com/ctxrs/ctx/pull/42",
    );
    let membership_evidence = NumberedEvidence {
        number: 1,
        citation: structured_pr_provider_output_citation(),
    };
    let contains = PullRequestCommit {
        fact_id: "pr-contains-commit".to_owned(),
        relationship: PullRequestCommitRelationship::ContainsCommit,
        commit: resource(
            ResourceKind::Commit,
            "pr-commit",
            "0123456789abcdef0123456789abcdef01234567",
        ),
        production: vec![production_attribution(
            "pr-commit",
            ProductionRelationship::ProducedBy,
            2,
        )],
        evidence_numbers: vec![1],
    };
    let merged_as = PullRequestCommit {
        fact_id: "pr-merged-as".to_owned(),
        relationship: PullRequestCommitRelationship::MergedAs,
        commit: resource(
            ResourceKind::Commit,
            "pr-merge-commit",
            "89abcdef0123456789abcdef0123456789abcdef",
        ),
        production: vec![production_attribution(
            "pr-merge-commit",
            ProductionRelationship::PossiblyProducedBy,
            3,
        )],
        evidence_numbers: vec![1],
    };
    BlameResult {
        target: ResolvedBlameTarget::PullRequest {
            selector: "https://github.com/ctxrs/ctx/pull/42".to_owned(),
            pull_request: pull_request.clone(),
            repository: resource(ResourceKind::Repository, "ctxrs-ctx", "ctxrs/ctx"),
        },
        git_snapshot: None,
        matches: vec![
            BlameMatch::PullRequest(PullRequestBlameMatch {
                pull_request: pull_request.clone(),
                relationship: PullRequestBlameRelationship::Commit(contains),
            }),
            BlameMatch::PullRequest(PullRequestBlameMatch {
                pull_request,
                relationship: PullRequestBlameRelationship::Commit(merged_as),
            }),
        ],
        evidence: vec![
            membership_evidence,
            NumberedEvidence {
                number: 2,
                citation: canonical_citation(2, ObservationKind::VcsChange),
            },
            NumberedEvidence {
                number: 3,
                citation: canonical_citation(3, ObservationKind::VcsChange),
            },
        ],
        next: None,
    }
}

fn host_messages(fingerprint: &str) -> Vec<(&'static str, HostMessage)> {
    let capabilities = BTreeSet::from([
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::JournalSync,
        Capability::OutputMaterialization,
        Capability::Query,
        Capability::GitRead,
    ]);
    vec![
        (
            "hello",
            HostMessage::Hello(HelloRequest {
                protocol_version: PROTOCOL_VERSION,
                protocol_fingerprint: fingerprint.to_owned(),
                host_version: "golden-host".to_owned(),
                capabilities,
            }),
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
            HostMessage::SyncJournal(journal_request(Vec::new(), fingerprint)),
        ),
        (
            "begin_output_inventory",
            HostMessage::BeginOutputInventory(BeginOutputInventoryRequest { generation: 1 }),
        ),
        (
            "observe_output_source",
            HostMessage::ObserveOutputSource(ObserveOutputSourceRequest {
                generation: 1,
                source: output_source(),
                availability: OutputSourceAvailability::Available,
            }),
        ),
        (
            "materialize_output_page",
            HostMessage::MaterializeOutputPage(output_page()),
        ),
        (
            "finish_output_inventory",
            HostMessage::FinishOutputInventory(FinishOutputInventoryRequest { generation: 1 }),
        ),
        (
            "get_output_progress",
            HostMessage::GetOutputProgress(OutputProgressRequest {
                sources: vec![output_source()],
            }),
        ),
        ("blame", HostMessage::Blame(blame(None, fingerprint))),
    ]
}

fn helper_messages(fingerprint: &str) -> Vec<(&'static str, HelperMessage)> {
    let capabilities = BTreeSet::from([
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::JournalSync,
        Capability::OutputMaterialization,
        Capability::Query,
        Capability::GitRead,
    ]);
    vec![
        (
            "hello",
            HelperMessage::Hello(HelloResult {
                protocol_version: PROTOCOL_VERSION,
                protocol_fingerprint: fingerprint.to_owned(),
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
                committed_through: checkpoint(fingerprint),
                accepted_records: 0,
                replayed: false,
                frozen_complete: true,
            }),
        ),
        (
            "output_inventory_began",
            HelperMessage::OutputInventoryBegan(OutputInventoryBegan {
                generation: 1,
                materializer_revision: "fixture-materializer-1".to_owned(),
            }),
        ),
        (
            "output_source_observed",
            HelperMessage::OutputSourceObserved(OutputSourceObserved {
                generation: 1,
                source: output_source(),
                availability: OutputSourceAvailability::Available,
            }),
        ),
        (
            "output_page_materialized",
            HelperMessage::OutputPageMaterialized(OutputPageMaterialized {
                inventory_generation: 1,
                source: output_source(),
                source_epoch: 0,
                committed_cursor: output_cursor(),
                accepted_outputs: 1,
                materialized_facts: 1,
                materialized_evidence: 1,
                replayed: false,
            }),
        ),
        (
            "output_inventory_finished",
            HelperMessage::OutputInventoryFinished(OutputInventoryFinished {
                generation: 1,
                observed_sources: 1,
                unavailable_sources: 0,
            }),
        ),
        (
            "output_progress",
            HelperMessage::OutputProgress(OutputProgressResult {
                inventory_generation: 1,
                inventory_complete: true,
                sources: vec![OutputSourceProgress {
                    source: output_source(),
                    source_epoch: 0,
                    observed_revision: "revision-1".to_owned(),
                    cursor: Some(output_cursor()),
                    parser_revision: "parser-1".to_owned(),
                    materializer_revision: "materializer-1".to_owned(),
                    terminal: true,
                    availability: OutputSourceAvailability::Available,
                    last_seen_inventory: Some(1),
                }],
            }),
        ),
        (
            "blame",
            HelperMessage::Blame(provider_output_blame_result()),
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
        ErrorClass::LineOutOfRange,
        ErrorClass::StaleSnapshot,
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

fn host_operation_messages(fingerprint: &str) -> Vec<(&'static str, HostMessage)> {
    let authorize = |access_kind| {
        let mut request = authorization();
        request.entitlement.grant.access_kind = access_kind;
        request.entitlement.grant.capabilities = BTreeSet::from([
            EntitlementCapability::GraphRead,
            EntitlementCapability::GraphWrite,
            EntitlementCapability::Export,
            EntitlementCapability::Migrate,
            EntitlementCapability::Update,
        ]);
        HostMessage::Authorize(request)
    };
    let [full_baseline, incremental] = journal_operation_requests(fingerprint);
    let [new_source, append_or_resume, rewrite] = output_operation_pages();
    vec![
        ("authorize_trial", authorize(EntitlementAccessKind::Trial)),
        ("authorize_active", authorize(EntitlementAccessKind::Active)),
        (
            "authorize_canceling_paid",
            authorize(EntitlementAccessKind::CancelingPaid),
        ),
        (
            "sync_journal_full_baseline_upsert",
            HostMessage::SyncJournal(full_baseline),
        ),
        (
            "sync_journal_incremental_delete",
            HostMessage::SyncJournal(incremental),
        ),
        (
            "observe_output_source_available",
            HostMessage::ObserveOutputSource(ObserveOutputSourceRequest {
                generation: 2,
                source: output_source(),
                availability: OutputSourceAvailability::Available,
            }),
        ),
        (
            "observe_output_source_unavailable",
            HostMessage::ObserveOutputSource(ObserveOutputSourceRequest {
                generation: 2,
                source: output_source(),
                availability: OutputSourceAvailability::Unavailable,
            }),
        ),
        (
            "observe_output_source_error",
            HostMessage::ObserveOutputSource(ObserveOutputSourceRequest {
                generation: 2,
                source: output_source(),
                availability: OutputSourceAvailability::Error,
            }),
        ),
        (
            "materialize_output_page_new_source_command_success",
            HostMessage::MaterializeOutputPage(new_source),
        ),
        (
            "materialize_output_page_append_or_resume_tool_failure",
            HostMessage::MaterializeOutputPage(append_or_resume),
        ),
        (
            "materialize_output_page_rewrite_command_timeout_and_tool_unknown",
            HostMessage::MaterializeOutputPage(rewrite),
        ),
        (
            "blame_file",
            HostMessage::Blame(blame_request(
                BlameTarget::File {
                    path: "src/lib.rs".to_owned(),
                    repository: Some("ctxrs/ctx".to_owned()),
                    lines: None,
                },
                None,
                fingerprint,
            )),
        ),
        (
            "blame_file_line",
            HostMessage::Blame(blame_request(
                BlameTarget::File {
                    path: "src/lib.rs".to_owned(),
                    repository: Some("ctxrs/ctx".to_owned()),
                    lines: Some(LineRange { start: 42, end: 42 }),
                },
                None,
                fingerprint,
            )),
        ),
        (
            "blame_file_range",
            HostMessage::Blame(blame_request(
                BlameTarget::File {
                    path: "src/lib.rs".to_owned(),
                    repository: Some("ctxrs/ctx".to_owned()),
                    lines: Some(LineRange { start: 42, end: 60 }),
                },
                None,
                fingerprint,
            )),
        ),
        (
            "blame_commit",
            HostMessage::Blame(blame_request(
                BlameTarget::Commit {
                    oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    repository: Some("ctxrs/ctx".to_owned()),
                },
                Some("commit-page-2".to_owned()),
                fingerprint,
            )),
        ),
        (
            "blame_pull_request_number",
            HostMessage::Blame(blame_request(
                BlameTarget::PullRequest {
                    selector: "42".to_owned(),
                    repository: Some("ctxrs/ctx".to_owned()),
                },
                None,
                fingerprint,
            )),
        ),
        (
            "blame_pull_request_url",
            HostMessage::Blame(blame_request(
                BlameTarget::PullRequest {
                    selector: "https://github.com/ctxrs/ctx/pull/42".to_owned(),
                    repository: None,
                },
                None,
                fingerprint,
            )),
        ),
    ]
}

fn helper_operation_messages(fingerprint: &str) -> Vec<(&'static str, HelperMessage)> {
    let authorized = |state| {
        HelperMessage::Authorized(AuthorizationResult {
            state,
            refresh_required: matches!(
                state,
                EntitlementAccessState::OfflineGrace | EntitlementAccessState::Locked
            ),
            expires_at_unix: 175,
            access_deadline_unix: 200,
            grace_deadline_unix: 250,
            capabilities: BTreeSet::from([
                EntitlementCapability::GraphRead,
                EntitlementCapability::GraphWrite,
                EntitlementCapability::Export,
                EntitlementCapability::Migrate,
                EntitlementCapability::Update,
            ]),
        })
    };
    let status = |state| {
        HelperMessage::Status(StatusResult {
            state,
            checkpoint: matches!(state, GraphState::Ready).then(|| checkpoint(fingerprint)),
        })
    };
    let source_observed = |availability| {
        HelperMessage::OutputSourceObserved(OutputSourceObserved {
            generation: 2,
            source: output_source(),
            availability,
        })
    };
    vec![
        (
            "authorized_trial",
            authorized(EntitlementAccessState::Trial),
        ),
        (
            "authorized_active",
            authorized(EntitlementAccessState::Active),
        ),
        (
            "authorized_canceling_paid",
            authorized(EntitlementAccessState::CancelingPaid),
        ),
        (
            "authorized_offline_grace",
            authorized(EntitlementAccessState::OfflineGrace),
        ),
        (
            "authorized_locked",
            authorized(EntitlementAccessState::Locked),
        ),
        (
            "status_not_materialized",
            status(GraphState::NotMaterialized),
        ),
        ("status_needs_rebuild", status(GraphState::NeedsRebuild)),
        ("status_partial", status(GraphState::Partial)),
        ("status_needs_resume", status(GraphState::NeedsResume)),
        ("status_ready", status(GraphState::Ready)),
        (
            "output_source_observed_available",
            source_observed(OutputSourceAvailability::Available),
        ),
        (
            "output_source_observed_unavailable",
            source_observed(OutputSourceAvailability::Unavailable),
        ),
        (
            "output_source_observed_error",
            source_observed(OutputSourceAvailability::Error),
        ),
        (
            "blame_file",
            HelperMessage::Blame(file_blame_result(
                None,
                LineRange { start: 1, end: 100 },
                WorktreeStatus::Clean,
                ProductionRelationship::ProducedBy,
                None,
            )),
        ),
        (
            "blame_file_line",
            HelperMessage::Blame(file_blame_result(
                Some(LineRange { start: 42, end: 42 }),
                LineRange { start: 42, end: 42 },
                WorktreeStatus::Differs,
                ProductionRelationship::PossiblyProducedBy,
                Some(BlameContinuation {
                    cursor: "file-line-next".to_owned(),
                    reason: ContinuationReason::MoreCommittedLines,
                }),
            )),
        ),
        (
            "blame_file_range",
            HelperMessage::Blame(file_blame_result(
                Some(LineRange { start: 42, end: 60 }),
                LineRange { start: 42, end: 60 },
                WorktreeStatus::Clean,
                ProductionRelationship::ProducedBy,
                None,
            )),
        ),
        ("blame_commit", HelperMessage::Blame(commit_blame_result())),
        (
            "blame_pull_request_activity_without_commit_membership",
            HelperMessage::Blame(pull_request_activity_result()),
        ),
        (
            "blame_pull_request_commit_membership",
            HelperMessage::Blame(pull_request_membership_result()),
        ),
    ]
}

fn operation_frames(fingerprint: &str) -> Value {
    let request_id = Uuid::from_u128(2);
    let host = host_operation_messages(fingerprint)
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
    let helper = helper_operation_messages(fingerprint)
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
    json!({
        "host_request_frames": host,
        "helper_response_frames": helper
    })
}

fn golden_vectors(fingerprint: &str) -> Value {
    let request_id = Uuid::from_u128(1);
    let host = host_messages(fingerprint)
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
    let helper = helper_messages(fingerprint)
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
        message: HostMessage::Blame(blame(Some("c".repeat(MAX_BLAME_CURSOR_BYTES)), fingerprint)),
    });
    let max_roots = HostEnvelope {
        sequence: u64::MAX,
        request_id,
        message: HostMessage::SyncJournal(journal_request(maximum_escaping_roots(), fingerprint)),
    };
    let max_roots_bytes = serde_json::to_vec(&max_roots)
        .unwrap_or_else(|error| panic!("max roots envelope: {error}"));
    json!({
        "host_frames": host,
        "helper_frames": helper,
        "operation_frames": operation_frames(fingerprint),
        "error_frames": errors,
        "cursor_frames": {"blame_cursor_max": max_cursor},
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
        Capability::OutputMaterialization,
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
            "output_observations_per_page": MAX_OUTPUT_OBSERVATIONS_PER_PAGE,
            "output_content_bytes": MAX_OUTPUT_CONTENT_BYTES,
            "output_content_bytes_per_page": MAX_OUTPUT_CONTENT_BYTES_PER_PAGE,
            "output_cursor_bytes": MAX_OUTPUT_CURSOR_BYTES,
            "output_locator_bytes": MAX_OUTPUT_LOCATOR_BYTES,
            "output_identity_bytes": MAX_OUTPUT_IDENTITY_BYTES,
            "output_command_bytes": MAX_OUTPUT_COMMAND_BYTES,
            "output_progress_sources": MAX_OUTPUT_PROGRESS_SOURCES,
            "blame_results": MAX_BLAME_RESULTS,
            "blame_cursor_bytes": MAX_BLAME_CURSOR_BYTES,
            "blame_evidence": MAX_BLAME_EVIDENCE,
            "blame_attributions_per_match": MAX_BLAME_ATTRIBUTIONS_PER_MATCH,
            "citations_per_fact": MAX_CITATIONS_PER_FACT,
            "blame_target_bytes": MAX_BLAME_TARGET_BYTES
        },
        "host_message_kinds": [
            "hello", "authorize", "prepare_graph_key_deletion",
            "confirm_graph_key_deletion", "status", "sync_journal",
            "begin_output_inventory", "observe_output_source", "materialize_output_page",
            "finish_output_inventory", "get_output_progress", "blame"
        ],
        "helper_message_kinds": [
            "hello", "authorized", "graph_key_deletion_prepared", "graph_key_deleted",
            "status", "journal_synced", "output_inventory_began", "output_source_observed",
            "output_page_materialized", "output_inventory_finished", "output_progress",
            "blame", "error"
        ],
        "capabilities": wire_names(&capabilities, Capability::wire_name),
        "enums": {
            "entitlement_access_kind": ["trial", "active", "canceling_paid"],
            "entitlement_access_state": ["trial", "active", "canceling_paid", "offline_grace", "locked"],
            "entitlement_capability": ["graph_read", "graph_write", "export", "migrate", "update"],
            "error_class": [
                "entitlement_expired", "key_store_unavailable", "key_store_locked",
                "not_materialized", "protocol_mismatch", "missing_source",
                "missing_repository", "stale_fact", "line_out_of_range",
                "stale_snapshot", "ambiguous", "corrupt",
                "invalid_request", "bounds", "sequence", "internal"
            ],
            "blame_match_kind": ["file", "commit", "pull_request"],
            "blame_target_kind": ["file", "commit", "pull_request"],
            "commit_fact_type": [
                "git.commit.produced", "git.commit.amended", "git.commit.cherry_picked",
                "git.commit.reverted", "git.commit.pushed", "git.commit.inspected",
                "git.commit.referenced", "git.commit.ambiguous"
            ],
            "commit_predicate": [
                "produced_by", "possibly_produced_by", "amended_by", "cherry_picked_from",
                "reverts", "pushed_by", "inspected_by", "referenced_by"
            ],
            "continuation_reason": ["more_matches", "more_committed_lines"],
            "fact_confidence": ["explicit", "high", "medium", "low", "ambiguous", "unknown"],
            "fact_state": ["asserted", "ambiguous", "contradicted", "superseded"],
            "graph_state": ["not_materialized", "needs_rebuild", "partial", "needs_resume", "ready"],
            "journal_entity_kind": ["event", "file_touch", "vcs_change"],
            "journal_operation": ["upsert", "delete"],
            "journal_sync_mode": ["full_baseline", "incremental"],
            "observation_kind": ["event", "file_touch", "vcs_change"],
            "output_observation_kind": ["command", "tool"],
            "output_outcome": ["success", "failure", "timeout", "unknown"],
            "output_source_availability": ["available", "unavailable", "error"],
            "output_source_disposition": ["append_or_resume", "new_source", "rewrite"],
            "production_relationship": ["produced_by", "possibly_produced_by"],
            "pull_request_action": [
                "referenced", "created", "reviewed", "commented",
                "merged", "edited", "closed", "reopened"
            ],
            "pull_request_commit_relationship": ["contains_commit", "merged_as"],
            "pull_request_relationship_kind": ["activity", "commit"],
            "resource_kind": ResourceKind::ALL.map(ResourceKind::wire_name),
            "worktree_status": ["clean", "differs"]
        },
        "dto_fields": {
            "AgentAttribution": fields(
                &["id", "relationship", "producing_session", "confidence", "state", "evidence_numbers"],
                &["direct_actor", "owning_root"]),
            "AuthorizationRequest": fields(
                &["entitlement", "installation_public_key_base64url", "challenge_base64url", "proof_signature_base64url"], &[]),
            "AuthorizationResult": fields(&[
                "state", "refresh_required", "expires_at_unix", "access_deadline_unix",
                "grace_deadline_unix", "capabilities"
            ], &[]),
            "ByteRange": fields(&["start", "end_exclusive"], &[]),
            "BeginOutputInventoryRequest": fields(&["generation"], &[]),
            "BlameContinuation": fields(&["cursor", "reason"], &[]),
            "BlameRequest": fields(&["target", "limit", "expected_snapshot"], &["cursor"]),
            "BlameResult": fields(&["target", "matches", "evidence"], &["git_snapshot", "next"]),
            "BlameTarget.commit": fields(&["kind", "oid"], &["repository"]),
            "BlameTarget.file": fields(&["kind", "path"], &["repository", "lines"]),
            "BlameTarget.pull_request": fields(&["kind", "selector"], &["repository"]),
            "ConfirmGraphKeyDeletionRequest": fields(&["authorization"], &[]),
            "CommitBlameMatch": fields(&[
                "fact_id", "fact_type", "predicate", "subject", "confidence", "state",
                "evidence_numbers"
            ], &["object", "fact_occurred_at_ms", "direct_actor", "owning_root"]),
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
                "source_record_subrecord_index", "byte_range", "source_sha256", "provider_output"
            ]),
            "FileBlameMatch": fields(
                &["id", "lines", "commit", "line_evidence_numbers", "production"], &[]),
            "GitSnapshot": fields(&["head_oid", "worktree_status"], &[]),
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
                "frozen_through", "authorized_repository_roots", "records"
            ], &[]),
            "JournalSyncResult": fields(&[
                "committed_through", "accepted_records", "replayed", "frozen_complete"
            ], &[]),
            "LineRange": fields(&["start", "end"], &[]),
            "NumberedEvidence": fields(&["number", "citation"], &[]),
            "FinishOutputInventoryRequest": fields(&["generation"], &[]),
            "ObserveOutputSourceRequest": fields(
                &["generation", "source", "availability"], &[]),
            "OutputAssociations": fields(
                &["direct_session_id", "root_session_id"],
                &["parent_session_id", "provider_session_id", "agent_id", "repository"]),
            "OutputCommandContext": fields(
                &["tool_name", "command"], &["working_directory"]),
            "OutputInventoryBegan": fields(
                &["generation", "materializer_revision"], &[]),
            "OutputInventoryFinished": fields(
                &["generation", "observed_sources", "unavailable_sources"], &[]),
            "OutputNativeCoordinate": fields(
                &["unit_key", "native_sequence"],
                &["native_record_id", "source_record_ordinal",
                  "source_record_subrecord_index", "byte_start", "byte_end_exclusive"]),
            "OutputNativeCursor": fields(&["version", "payload_base64"], &[]),
            "OutputOutcomeMetadata": fields(
                &["outcome"], &["exit_code", "duration_ms"]),
            "OutputPageMaterialized": fields(&[
                "inventory_generation", "source", "source_epoch", "committed_cursor",
                "accepted_outputs", "materialized_facts", "materialized_evidence", "replayed"
            ], &[]),
            "OutputProgressRequest": fields(&["sources"], &[]),
            "OutputProgressResult": fields(
                &["inventory_generation", "inventory_complete", "sources"], &[]),
            "OutputRepositoryContext": fields(
                &["repository_id"], &["checkout_id", "worktree_id", "object_format"]),
            "OutputSourceIdentity": fields(
                &["provider", "namespace_id", "source_id"], &[]),
            "OutputSourceLocator": fields(
                &["version", "kind", "payload_base64"], &[]),
            "OutputSourceObserved": fields(
                &["generation", "source", "availability"], &[]),
            "OutputSourceProgress": fields(&[
                "source", "source_epoch", "observed_revision", "parser_revision",
                "materializer_revision", "terminal", "availability"
            ], &["cursor", "last_seen_inventory"]),
            "PrepareGraphKeyDeletionRequest": fields(&["installation_key_thumbprint"], &[]),
            "ProOutputMaterializationPage": fields(&[
                "contract_version", "inventory_generation", "source", "source_epoch",
                "observed_revision", "parser_revision", "materializer_revision", "disposition",
                "next_safe_cursor", "terminal", "observations"
            ], &["expected_prior_source_epoch", "expected_prior_cursor"]),
            "ProOutputObservation": fields(&[
                "kind", "coordinate", "associations", "outcome", "locator", "content"
            ], &["occurred_at_unix_ms", "call_id", "command"]),
            "ProviderOutputEvidence": fields(&[
                "source_id", "source_epoch", "locator", "coordinate", "availability"
            ], &[]),
            "ProtocolError": fields(&["class", "message", "retryable"], &[]),
            "PullRequestActivity": fields(&[
                "fact_id", "action", "session", "confidence", "state", "evidence_numbers"
            ], &["direct_actor", "owning_root", "fact_occurred_at_ms"]),
            "PullRequestBlameMatch": fields(&["pull_request", "relationship"], &[]),
            "PullRequestCommit": fields(
                &["fact_id", "relationship", "commit", "production", "evidence_numbers"], &[]),
            "QuerySnapshotExpectation": fields(&["checkpoint", "projection_pending"], &[]),
            "ResourceRef": fields(&["id", "kind", "display"], &[]),
            "ResolvedBlameTarget.commit": fields(&["kind", "commit", "repository"], &[]),
            "ResolvedBlameTarget.file": fields(
                &["kind", "path", "repository"], &["requested_lines"]),
            "ResolvedBlameTarget.pull_request": fields(
                &["kind", "selector", "pull_request", "repository"], &[]),
            "SignedEntitlement": fields(&["grant", "signature_base64url"], &[]),
            "StatusRequest": fields(&[], &[]),
            "StatusResult": fields(&["state"], &["checkpoint"]),
            "TransientOutputContent": {
                "wire_type": "canonical_base64_string",
                "debug": "redacted"
            }
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
        "output_materialization": {
            "contract_version": OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
            "durability": "complete_output_content_is_request_only_and_never_part_of_journal_sync",
            "ordering": "strict_native_sequence_then_unit_key",
            "commit": "page_sent_only_after_core_safe_group_commit_and_source_revalidation",
            "progress": "independent_per_source_epoch_and_native_cursor_compare_and_swap"
        },
        "evidence_citation": {
            "branches": {
                "canonical_or_source": [
                    "observation_id", "observation_seq", "observation_kind", "session_id",
                    "event_id", "event_seq", "source_path", "fixture_line",
                    "source_record_ordinal", "source_record_subrecord_index", "byte_range",
                    "source_sha256"
                ],
                "provider_output": ["provider_output"]
            },
            "selection": "exactly_one_usable_branch",
            "provider_output_coordinates": "typed_source_epoch_locator_and_native_coordinate",
            "provider_output_availability": "available_unavailable_or_error_is_preserved"
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
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("--fingerprint-rust") => {
            assert!(
                arguments.next().is_none(),
                "--fingerprint-rust takes no value"
            );
            println!("\"{digest}\"");
            return;
        }
        None => {}
        Some(argument) => panic!("unsupported inventory argument: {argument}"),
    }
    let output = json!({
        "canonical_inventory": serde_json::from_slice::<Value>(&bytes)
            .unwrap_or_else(|error| panic!("canonical inventory: {error}")),
        "canonical_sha256": digest,
        "golden_vectors": golden_vectors(&digest)
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output)
            .unwrap_or_else(|error| panic!("format inventory: {error}"))
    );
}
