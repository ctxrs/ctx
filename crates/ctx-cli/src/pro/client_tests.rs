use chrono::{DateTime, Utc};
use ctx_history_capture::{import_codex_session_jsonl, CodexSessionImportOptions};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    EntityTimestamps, Event, EventRole, EventType, Fidelity, Session, SessionStatus, SyncMetadata,
};
use serde_json::json;
use std::{cell::Cell, fs, path::PathBuf};
use tempfile::tempdir;

use super::*;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn now() -> DateTime<Utc> {
    "2026-07-22T00:00:00Z".parse().expect("valid fixture time")
}

fn event(id: Uuid, session_id: Uuid, source_id: Uuid) -> Event {
    Event {
        id,
        seq: 1,
        history_record_id: None,
        session_id: Some(session_id),
        run_id: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: now(),
        capture_source_id: Some(source_id),
        payload: json!({"body": "journal parity"}),
        payload_blob_id: None,
        dedupe_key: None,
        sync: SyncMetadata {
            fidelity: Fidelity::Imported,
            metadata: json!({"fixture_line": 1}),
            ..SyncMetadata::default()
        },
    }
}

fn source(id: Uuid) -> CaptureSource {
    CaptureSource {
        id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: "fixture-machine".to_owned(),
            process_id: None,
            cwd: Some("/fixture/repository".to_owned()),
            raw_source_path: Some("/fixture/provider/session.jsonl".to_owned()),
            source_format: Some("codex_session_jsonl_tree".to_owned()),
            source_root: Some("/fixture/provider".to_owned()),
            source_identity: Some("fixture-source".to_owned()),
            external_session_id: Some("provider-session-1".to_owned()),
        },
        started_at: now(),
        ended_at: None,
        sync: SyncMetadata::default(),
    }
}

fn session(id: Uuid, source_id: Uuid) -> Session {
    Session {
        id,
        history_record_id: None,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Codex,
        external_session_id: Some("provider-session-1".to_owned()),
        external_agent_id: Some("agent-1".to_owned()),
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: now(),
        ended_at: None,
        timestamps: EntityTimestamps {
            created_at: now(),
            updated_at: now(),
        },
        sync: SyncMetadata::default(),
    }
}

#[test]
fn store_pages_validate_as_exact_protocol_v1_journal_requests() {
    let temp = tempdir().expect("temp dir");
    let store = Store::open(temp.path().join("ctx.db")).expect("open Store");
    let source_id = Uuid::from_u128(2);
    let session_id = Uuid::from_u128(3);
    store
        .upsert_capture_source(&source(source_id))
        .expect("source");
    store
        .upsert_session(&session(session_id, source_id))
        .expect("session");
    let genesis = store
        .activate_projection_journal(PROTOCOL_FINGERPRINT)
        .expect("activate journal");
    assert_eq!(
        genesis.cumulative_digest,
        initial_journal_digest(genesis.position.generation)
    );

    store
        .upsert_event(&event(Uuid::from_u128(1), session_id, source_id))
        .expect("append event");
    let snapshot = store
        .projection_journal_snapshot(None)
        .expect("read journal");
    let prior = JournalCheckpoint {
        position: JournalPosition {
            generation: genesis.position.generation,
            sequence: 0,
        },
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        cumulative_digest: genesis.cumulative_digest,
    };
    let request = JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: snapshot.canonical_schema_version,
        canonical_schema_identity: snapshot.canonical_schema_identity,
        projection_contract_version: snapshot.projection_contract_version,
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        prior_checkpoint: prior,
        frozen_through: protocol_checkpoint(snapshot.frozen_through),
        authorized_repository_roots: snapshot.authorized_repository_roots,
        records: snapshot
            .records
            .into_iter()
            .map(protocol_journal_record)
            .collect(),
        result_contents: Vec::new(),
    };
    request.validate().expect("Store page matches protocol");
    assert_eq!(request.committed_checkpoint(), request.frozen_through);
    let golden: serde_json::Value = serde_json::from_str(include_str!(
        "../../../ctx-pro-host-protocol/testdata/v1/public-journal-page.json"
    ))
    .expect("public journal golden JSON");
    assert_eq!(serde_json::to_value(&request).unwrap(), golden);
}

#[test]
fn codex_result_hydration_is_source_verified_transient_and_fail_open() {
    let temp = tempdir().expect("temp dir");
    let source_path = temp.path().join("rollout.jsonl");
    let secret = "RESULT-BODY-SECRET-8d23a0";
    let second_secret = "RESULT-BODY-SECRET-4c71be";
    let oversized_result = format!(
        "OVERSIZED-RESULT-{}",
        "x".repeat(MAX_RESULT_CONTENT_BYTES_PER_ITEM)
    );
    let transcript = [
        json!({
            "timestamp": "2026-07-22T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "source-backed-session",
                "timestamp": "2026-07-22T00:00:00Z",
                "cwd": "/workspace/project",
                "originator": "codex-cli"
            }
        }),
        json!({
            "timestamp": "2026-07-22T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": "call-source-backed",
                "arguments": "{\"cmd\":\"printf secret\"}"
            }
        }),
        json!({
            "timestamp": "2026-07-22T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call-source-backed",
                "output": format!("{secret}\nProcess exited with code 0")
            }
        }),
        json!({
            "timestamp": "2026-07-22T00:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": "call-source-backed-2",
                "arguments": "{\"cmd\":\"printf second-secret\"}"
            }
        }),
        json!({
            "timestamp": "2026-07-22T00:00:04Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call-source-backed-2",
                "output": format!("{second_secret}\nProcess exited with code 0")
            }
        }),
        json!({
            "timestamp": "2026-07-22T00:00:05Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": "call-source-backed-oversized",
                "arguments": "{\"cmd\":\"emit oversized output\"}"
            }
        }),
        json!({
            "timestamp": "2026-07-22T00:00:06Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call-source-backed-oversized",
                "output": oversized_result
            }
        }),
    ]
    .into_iter()
    .map(|value| format!("{}\n", serde_json::to_string(&value).unwrap()))
    .collect::<String>();
    fs::write(&source_path, &transcript).expect("write Codex source");
    let database = temp.path().join("ctx.db");
    let mut store = Store::open(&database).expect("open Store");
    let imported = import_codex_session_jsonl(
        &source_path,
        &mut store,
        CodexSessionImportOptions::default(),
    )
    .expect("import source");
    assert_eq!(imported.failed, 0, "{:?}", imported.failures);
    let session = store
        .session_by_external_session(CaptureProvider::Codex, "source-backed-session")
        .expect("session query")
        .expect("imported session");
    let outputs = store
        .events_for_session(session.id)
        .expect("events")
        .into_iter()
        .filter(|event| event.event_type == EventType::CommandOutput)
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 3);
    for output in outputs {
        let stored_payload = serde_json::to_string(&output.payload).unwrap();
        assert!(!stored_payload.contains(secret));
        assert!(!stored_payload.contains(second_secret));
        assert!(!stored_payload.contains("OVERSIZED-RESULT"));
        assert!(!stored_payload.contains("output_preview"));
    }
    assert!(store.search_event_hits(secret, 10).unwrap().is_empty());
    assert!(store
        .search_event_hits(second_secret, 10)
        .unwrap()
        .is_empty());

    let genesis = store
        .activate_projection_journal(PROTOCOL_FINGERPRINT)
        .expect("activate journal");
    let snapshot = store
        .projection_journal_snapshot(None)
        .expect("journal snapshot");
    let mut request = JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: snapshot.canonical_schema_version,
        canonical_schema_identity: snapshot.canonical_schema_identity,
        projection_contract_version: snapshot.projection_contract_version,
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        prior_checkpoint: JournalCheckpoint {
            position: JournalPosition {
                generation: genesis.position.generation,
                sequence: 0,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: initial_journal_digest(genesis.position.generation),
        },
        frozen_through: protocol_checkpoint(snapshot.frozen_through),
        authorized_repository_roots: snapshot.authorized_repository_roots,
        records: snapshot
            .records
            .into_iter()
            .map(protocol_journal_record)
            .collect(),
        result_contents: Vec::new(),
    };
    let durable_json = serde_json::to_string(&request.records).unwrap();
    assert!(!durable_json.contains(secret));
    assert!(!durable_json.contains(second_secret));
    assert!(!durable_json.contains("OVERSIZED-RESULT"));
    assert!(!durable_json.contains("output_preview"));
    let counts = hydrate_result_contents(&store, &mut request);
    assert_eq!(
        counts,
        ResultHydrationCounts {
            hydrated: 2,
            omitted: 1,
            resolver_batches: 1,
        }
    );
    assert_eq!(request.result_contents.len(), 2);
    assert!(request
        .result_contents
        .iter()
        .any(|sidecar| sidecar.content.contains(secret)));
    assert!(request
        .result_contents
        .iter()
        .any(|sidecar| sidecar.content.contains(second_secret)));
    request.validate().expect("hydrated request");

    fs::write(
        &source_path,
        transcript.replace(secret, "ALTERD-BODY-SECRET-8d23a0"),
    )
    .expect("change source");
    let changed = hydrate_result_contents(&store, &mut request);
    assert_eq!(
        changed,
        ResultHydrationCounts {
            hydrated: 1,
            omitted: 2,
            resolver_batches: 1,
        }
    );
    assert_eq!(request.result_contents.len(), 1);
    assert!(request.result_contents[0].content.contains(second_secret));
    assert!(!request.result_contents[0]
        .content
        .contains("ALTERD-BODY-SECRET-8d23a0"));
    request
        .validate()
        .expect("changed source omission is valid");

    fs::remove_file(&source_path).expect("remove source");
    let missing = hydrate_result_contents(&store, &mut request);
    assert_eq!(
        missing,
        ResultHydrationCounts {
            hydrated: 0,
            omitted: 3,
            resolver_batches: 1,
        }
    );
    assert!(request.result_contents.is_empty());
    request
        .validate()
        .expect("missing source omission is valid");
}

#[test]
fn result_hydration_reserves_the_aggregate_budget_before_multi_source_reads() {
    let temp = tempdir().expect("temp dir");
    let database = temp.path().join("ctx.db");
    let mut store = Store::open(&database).expect("open Store");
    let mut source_paths = Vec::new();
    for index in 0..5 {
        let source_path = temp.path().join(format!("rollout-{index}.jsonl"));
        let prefix = format!("source-{index}:");
        let output = format!(
            "{prefix}{}",
            "x".repeat(MAX_RESULT_CONTENT_BYTES_PER_ITEM - prefix.len())
        );
        let transcript = [
            json!({
                "timestamp": "2026-07-22T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": format!("aggregate-budget-session-{index}"),
                    "timestamp": "2026-07-22T00:00:00Z",
                    "cwd": "/workspace/project",
                    "originator": "codex-cli"
                }
            }),
            json!({
                "timestamp": "2026-07-22T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "exec_command",
                    "call_id": format!("aggregate-budget-call-{index}"),
                    "arguments": "{\"cmd\":\"emit bounded result\"}"
                }
            }),
            json!({
                "timestamp": "2026-07-22T00:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": format!("aggregate-budget-call-{index}"),
                    "output": output
                }
            }),
        ]
        .into_iter()
        .map(|value| format!("{}\n", serde_json::to_string(&value).unwrap()))
        .collect::<String>();
        fs::write(&source_path, transcript).expect("write Codex source");
        let imported = import_codex_session_jsonl(
            &source_path,
            &mut store,
            CodexSessionImportOptions::default(),
        )
        .expect("import source");
        assert_eq!(imported.failed, 0, "{:?}", imported.failures);
        source_paths.push(source_path);
    }

    let genesis = store
        .activate_projection_journal(PROTOCOL_FINGERPRINT)
        .expect("activate journal");
    let snapshot = store
        .projection_journal_snapshot(None)
        .expect("journal snapshot");
    let missing_source_path = snapshot
        .records
        .iter()
        .find_map(|record| {
            record
                .canonical_payload
                .as_ref()?
                .pointer("/result/content_ref")?;
            let event = store.get_event(record.stable_entity_id).ok()?;
            let source = store.get_capture_source(event.capture_source_id?).ok()?;
            source.descriptor.raw_source_path.map(PathBuf::from)
        })
        .expect("first budget-reserved result source");
    let missing_source_index = source_paths
        .iter()
        .position(|path| path == &missing_source_path)
        .expect("reserved source belongs to the fixture");
    let missing_prefix = format!("source-{missing_source_index}:");
    let mut request = JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: snapshot.canonical_schema_version,
        canonical_schema_identity: snapshot.canonical_schema_identity,
        projection_contract_version: snapshot.projection_contract_version,
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        prior_checkpoint: JournalCheckpoint {
            position: JournalPosition {
                generation: genesis.position.generation,
                sequence: 0,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: initial_journal_digest(genesis.position.generation),
        },
        frozen_through: protocol_checkpoint(snapshot.frozen_through),
        authorized_repository_roots: snapshot.authorized_repository_roots,
        records: snapshot
            .records
            .into_iter()
            .map(protocol_journal_record)
            .collect(),
        result_contents: Vec::new(),
    };
    fs::remove_file(&missing_source_path).expect("remove budget-reserved source");

    let counts = hydrate_result_contents(&store, &mut request);
    assert_eq!(
        counts,
        ResultHydrationCounts {
            hydrated: 3,
            omitted: 2,
            resolver_batches: 4,
        }
    );
    assert_eq!(
        request
            .result_contents
            .iter()
            .map(|content| content.content.len())
            .sum::<usize>(),
        3 * MAX_RESULT_CONTENT_BYTES_PER_ITEM
    );
    assert!(!request
        .result_contents
        .iter()
        .any(|content| content.content.contains(&missing_prefix)));
    request.validate().expect("bounded request");
}

#[test]
fn frozen_store_pages_coalesce_and_resume_without_skips_or_duplicates() {
    let temp = tempdir().expect("temp dir");
    let store = Store::open(temp.path().join("ctx.db")).expect("open Store");
    let source_id = Uuid::from_u128(2);
    let session_id = Uuid::from_u128(3);
    store
        .upsert_capture_source(&source(source_id))
        .expect("source");
    store
        .upsert_session(&session(session_id, source_id))
        .expect("session");
    let genesis = store
        .activate_projection_journal(PROTOCOL_FINGERPRINT)
        .expect("activate journal");
    for index in 1..=600_u64 {
        let mut fixture = event(
            Uuid::from_u128(u128::from(index).saturating_add(100)),
            session_id,
            source_id,
        );
        fixture.seq = index;
        fixture.payload =
            json!({"body": format!("variable-{index}-{}", "\\\"".repeat(index as usize % 31))});
        store.upsert_event(&fixture).expect("append event");
    }

    let first = coalesced_journal_snapshot(
        &store,
        StoreJournalPosition {
            generation: genesis.position.generation,
            sequence: 0,
        },
    )
    .expect("first coalesced page");
    assert_eq!(first.records.len(), MAX_JOURNAL_RECORDS_PER_BATCH);
    assert!(first.has_more);
    let frozen = first.frozen_through.clone();
    let first_sequences = first
        .records
        .iter()
        .map(|record| record.sequence)
        .collect::<Vec<_>>();

    let second =
        coalesced_journal_snapshot(&store, first.next_position).expect("resumed coalesced page");
    assert_eq!(second.frozen_through, frozen);
    assert!(!second.has_more);
    let all_sequences = first_sequences
        .into_iter()
        .chain(second.records.iter().map(|record| record.sequence))
        .collect::<Vec<_>>();
    assert_eq!(all_sequences.len(), 600);
    assert_eq!(all_sequences, (1..=600_u64).collect::<Vec<_>>());
}

#[test]
fn journal_page_is_trimmed_against_the_complete_maximum_envelope() {
    fn record(sequence: u64, prior: &str, payload_bytes: usize) -> JournalRecord {
        let payload = json!({"body": "x".repeat(payload_bytes)});
        let payload_sha256 = ctx_pro_host_protocol::sha256_hex(
            &ctx_pro_host_protocol::canonical_payload_bytes(&payload).unwrap(),
        );
        let id = Uuid::from_u128(u128::from(sequence));
        let provenance = JournalProvenanceIdentity {
            entity_kind: JournalEntityKind::Event,
            stable_entity_id: id,
            capture_source_id: None,
            provider: None,
            provider_external_id: None,
        };
        let mut record = JournalRecord {
            generation: 1,
            sequence,
            projection_contract_version: ctx_pro_host_protocol::PROJECTION_CONTRACT_VERSION,
            entity_kind: JournalEntityKind::Event,
            stable_entity_id: id,
            entity_revision: 1,
            operation: JournalOperation::Upsert,
            canonical_payload: Some(payload),
            payload_sha256,
            evidence: Vec::new(),
            provenance,
            cumulative_digest: "0".repeat(64),
        };
        record.cumulative_digest =
            ctx_pro_host_protocol::journal_record_digest(prior, &record).unwrap();
        record
    }

    let roots = (0..ctx_pro_host_protocol::MAX_AUTHORIZED_REPOSITORY_ROOTS)
        .map(|index| format!("/{index:03}/{}", "\\\"".repeat(1_020)))
        .collect::<Vec<_>>();
    let initial = initial_journal_digest(1);
    let first = record(1, &initial, 3 * 1024 * 1024);
    let second = record(2, &first.cumulative_digest, 1024 * 1024);
    let frozen = JournalCheckpoint {
        position: JournalPosition {
            generation: 1,
            sequence: 2,
        },
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        cumulative_digest: second.cumulative_digest.clone(),
    };
    let request = JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: 47,
        canonical_schema_identity: "ctx-store-schema-47-final-v3".to_owned(),
        projection_contract_version: ctx_pro_host_protocol::PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        prior_checkpoint: JournalCheckpoint {
            position: JournalPosition {
                generation: 1,
                sequence: 0,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: initial,
        },
        frozen_through: frozen,
        authorized_repository_roots: roots,
        records: vec![first, second],
        result_contents: Vec::new(),
    };
    assert!(journal_sync_envelope_bytes(&request).unwrap() > MAX_JOURNAL_SYNC_ENVELOPE_BYTES);

    let fitted = fit_journal_sync_request(request).expect("fit request");
    assert_eq!(fitted.records.len(), 1);
    assert!(journal_sync_envelope_bytes(&fitted).unwrap() <= MAX_JOURNAL_SYNC_ENVELOPE_BYTES);
    fitted.validate().expect("trimmed page remains valid");
    assert_eq!(fitted.committed_checkpoint().position.sequence, 1);
    assert_eq!(fitted.frozen_through.position.sequence, 2);
}

#[test]
fn query_capabilities_are_exact_and_blame_also_requires_git() {
    assert_eq!(
        required_query_capabilities(QueryKind::Show),
        BTreeSet::from([Capability::Query])
    );
    assert_eq!(
        required_query_capabilities(QueryKind::Blame),
        BTreeSet::from([Capability::Query, Capability::GitRead])
    );
}

#[test]
fn journal_ack_requires_the_exact_checkpoint_and_counts() {
    let checkpoint = JournalCheckpoint {
        position: JournalPosition {
            generation: 1,
            sequence: 7,
        },
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        cumulative_digest: "a".repeat(64),
    };
    let accepted = JournalSyncResult {
        committed_through: checkpoint.clone(),
        accepted_records: 3,
        replayed: false,
        frozen_complete: true,
    };
    assert!(validate_journal_ack(&accepted, &checkpoint, &checkpoint, 3).is_ok());

    let mut invalid = accepted;
    invalid.accepted_records = 2;
    assert!(validate_journal_ack(&invalid, &checkpoint, &checkpoint, 3).is_err());
}

#[test]
fn repository_semantic_rebuild_retry_is_bounded_to_one_retry() {
    let calls = Cell::new(0_u32);
    let result = retry_materialization_once(|| {
        calls.set(calls.get() + 1);
        Err::<(), _>(anyhow!("not_materialized"))
    });
    assert!(result.is_err());
    assert_eq!(calls.get(), 2);

    let calls = Cell::new(0_u32);
    let result = retry_materialization_once(|| {
        calls.set(calls.get() + 1);
        (calls.get() == 2)
            .then_some(())
            .ok_or_else(|| anyhow!("not_materialized"))
    });
    assert!(result.is_ok());
    assert_eq!(calls.get(), 2);
}

#[test]
fn materialization_mode_uses_only_public_host_graph_state_and_checkpoint_shape() {
    assert_eq!(
        ProMaterializationModeV1::from_graph_state(GraphState::NotMaterialized, false, 0),
        ProMaterializationModeV1::Full
    );
    assert_eq!(
        ProMaterializationModeV1::from_graph_state(GraphState::Ready, true, 9),
        ProMaterializationModeV1::Incremental
    );
    assert_eq!(
        ProMaterializationModeV1::from_graph_state(GraphState::NeedsResume, true, 9),
        ProMaterializationModeV1::Resume
    );
    assert_eq!(
        ProMaterializationModeV1::from_graph_state(GraphState::NeedsRebuild, false, 9),
        ProMaterializationModeV1::Rebuild
    );
    assert_eq!(
        ProMaterializationModeV1::from_graph_state(GraphState::Ready, false, 9),
        ProMaterializationModeV1::Rebuild
    );
}

#[test]
fn generic_protocol_failures_map_to_stable_public_codes() {
    for (class, expected) in [
        (
            ctx_pro_host_protocol::ErrorClass::ProtocolMismatch,
            "protocol_mismatch",
        ),
        (
            ctx_pro_host_protocol::ErrorClass::MissingSource,
            "source_unavailable",
        ),
        (
            ctx_pro_host_protocol::ErrorClass::MissingRepository,
            "repository_unavailable",
        ),
        (ctx_pro_host_protocol::ErrorClass::StaleFact, "stale_fact"),
    ] {
        let mapped = protocol_error(ctx_pro_host_protocol::ProtocolError::new(
            class,
            "untrusted helper detail",
        ));
        assert_eq!(mapped.to_string(), expected);
        assert_eq!(stable_error_code(&mapped), Some(expected));
    }
}

#[test]
fn status_binds_installation_identity_and_preserves_locked_state() {
    let status = BTreeSet::from([Capability::Status]);
    assert!(authorization_required(&status, true));
    assert!(!authorization_required(&status, false));
    assert_eq!(
        status_outcome(GraphState::Ready, Some(EntitlementAccessState::Locked)),
        (false, true, Some("entitlement_expired"))
    );
    assert_eq!(
        status_outcome(
            GraphState::Ready,
            Some(EntitlementAccessState::OfflineGrace)
        ),
        (true, true, None)
    );
}

#[cfg(unix)]
struct RecordingAuthorization {
    calls: Cell<u32>,
}

#[cfg(unix)]
impl AuthorizationProvider for RecordingAuthorization {
    fn authorization_for_challenge(
        &self,
        challenge: &[u8; ctx_pro_host_protocol::AUTHORIZATION_CHALLENGE_BYTES],
    ) -> Result<ctx_pro_host_protocol::AuthorizationRequest> {
        use ctx_pro_host_protocol::{
            AuthorizationRequest, EntitlementAccessKind, EntitlementCapability, EntitlementGrant,
            SignedEntitlement,
        };
        self.calls.set(self.calls.get() + 1);
        Ok(AuthorizationRequest {
            entitlement: SignedEntitlement {
                grant: EntitlementGrant {
                    schema_version: 1,
                    issuer: "test".to_owned(),
                    key_id: "test".to_owned(),
                    grant_id: "test".to_owned(),
                    subject: "test".to_owned(),
                    account_id: "test".to_owned(),
                    product: "ctx-pro".to_owned(),
                    access_kind: EntitlementAccessKind::Active,
                    installation_key_thumbprint: "test".to_owned(),
                    issued_at_unix: 1,
                    not_before_unix: 1,
                    refresh_after_unix: 2,
                    access_deadline_unix: 3,
                    grace_deadline_unix: 4,
                    expires_at_unix: 5,
                    minimum_helper_protocol: PROTOCOL_VERSION,
                    revocation_epoch: 0,
                    capabilities: BTreeSet::from([EntitlementCapability::GraphRead]),
                },
                signature_base64url: "test".to_owned(),
            },
            installation_public_key_base64url: "test".to_owned(),
            challenge_base64url: ctx_pro_host_protocol::base64url(challenge),
            proof_signature_base64url: "test".to_owned(),
        })
    }
}

#[cfg(unix)]
fn write_smoke_helper(path: &Path, capabilities: &str) {
    let script = format!(
        r#"#!/usr/bin/python3
import json, struct, sys

def receive():
    header = sys.stdin.buffer.read(12)
    if len(header) != 12 or header[:8] != b'CTXPRO\x00\x01':
        sys.exit(20)
    size = struct.unpack('>I', header[8:12])[0]
    return json.loads(sys.stdin.buffer.read(size))

def send(request, kind, body):
    value = {{'sequence':request['sequence'],'request_id':request['request_id'],
             'message':{{'kind':kind,'body':body}}}}
    payload = json.dumps(value, separators=(',', ':')).encode()
    sys.stdout.buffer.write(b'CTXPRO\x00\x01' + struct.pack('>I', len(payload)) + payload)
    sys.stdout.buffer.flush()

hello = receive()
send(hello, 'hello', {{
    'protocol_version':1,
    'protocol_fingerprint':'{PROTOCOL_FINGERPRINT}',
    'helper_version':'staged-smoke-test',
    'capabilities':{capabilities},
    'authorization_challenge_base64url':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'
}})
if 'entitlement_authorization' not in {capabilities}:
    sys.exit(0)
authorization = receive()
if authorization['message']['kind'] != 'authorize':
    sys.exit(21)
send(authorization, 'authorized', {{
    'state':'active','refresh_required':False,'expires_at_unix':5,
    'access_deadline_unix':3,'grace_deadline_unix':4,'capabilities':['graph_read']
}})
status = receive()
if status['message']['kind'] != 'status':
    sys.exit(22)
send(status, 'status', {{'state':'ready','checkpoint':None}})
"#
    );
    fs::write(path, script).expect("write helper");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("make helper executable");
}

#[cfg(unix)]
#[test]
fn staged_smoke_proves_authorization_and_status_before_success() {
    let temp = tempdir().expect("temp dir");
    crate::identity::installation_id(temp.path()).expect("installation identity");
    let helper = temp.path().join("ctx-pro-smoke");
    write_smoke_helper(&helper, "['entitlement_authorization','status']");
    let authorization = RecordingAuthorization {
        calls: Cell::new(0),
    };
    let (smoke, status) = super::client_status::smoke_helper_at_path_with_authorization_and_status(
        temp.path(),
        &helper,
        Some(&authorization),
    )
    .expect("full staged smoke");
    assert_eq!(authorization.calls.get(), 1);
    assert_eq!(status.state, GraphState::Ready);
    assert!(smoke
        .capabilities
        .contains(&Capability::EntitlementAuthorization));
}

#[cfg(unix)]
#[test]
fn staged_smoke_rejects_a_helper_without_entitlement_authorization() {
    let temp = tempdir().expect("temp dir");
    crate::identity::installation_id(temp.path()).expect("installation identity");
    let helper = temp.path().join("ctx-pro-smoke");
    write_smoke_helper(&helper, "['status']");
    let authorization = RecordingAuthorization {
        calls: Cell::new(0),
    };
    let error = smoke_helper_at_path_with_authorization(temp.path(), &helper, Some(&authorization))
        .expect_err("missing entitlement capability must fail");
    assert!(error.to_string().starts_with("protocol_mismatch:"));
    assert_eq!(authorization.calls.get(), 0);
}
