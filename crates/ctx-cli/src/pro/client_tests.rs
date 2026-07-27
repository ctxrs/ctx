use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    EntityTimestamps, Event, EventRole, EventType, Fidelity, Session, SessionStatus, SyncMetadata,
};
use serde_json::json;
use std::{cell::Cell, fs};
use tempfile::tempdir;

use super::*;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "linux")]
use std::{
    os::fd::AsRawFd,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
#[cfg(target_os = "linux")]
use {
    super::super::{
        credential_vault::{
            BoundedSignedEntitlement, CredentialRecord, CredentialVaultNamespace,
            InstallationSigningKeySeed, PlatformCredentialVault,
        },
        lifecycle::ProDeletionService,
        local_deletion::LocalDeletionService,
    },
    ed25519_dalek::SigningKey,
};

#[cfg(target_os = "linux")]
const DUAL_NAMESPACE_DELETION_MODE_ENV: &str = "CTX_TEST_DUAL_NAMESPACE_DELETION_MODE";
#[cfg(target_os = "linux")]
const DUAL_NAMESPACE_DELETION_ROOT_ENV: &str = "CTX_TEST_DUAL_NAMESPACE_DELETION_ROOT";
#[cfg(target_os = "linux")]
const DUAL_NAMESPACE_DELETION_RUNNER: &str =
    "pro::client::tests::dual_namespace_graph_key_deletion_subprocess_runner";

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

fn exact_journal_sync_response(request: &JournalSyncRequest) -> HelperMessage {
    let committed_through = request.committed_checkpoint();
    HelperMessage::JournalSynced(JournalSyncResult {
        frozen_complete: committed_through == request.frozen_through,
        committed_through,
        accepted_records: u32::try_from(request.records.len()).expect("bounded records"),
        replayed: false,
    })
}

#[test]
fn nativepath_session_requires_the_complete_existing_capability_set() {
    assert_eq!(
        nativepath_pro_capabilities(),
        BTreeSet::from([
            Capability::Status,
            Capability::JournalSync,
            Capability::OutputMaterialization,
        ])
    );
}

#[test]
fn target_bounded_pages_exclude_a_concurrent_later_core_commit() {
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
    store
        .upsert_event(&event(Uuid::from_u128(4), session_id, source_id))
        .expect("target event");
    let target = store
        .projection_journal_snapshot(None)
        .expect("target snapshot")
        .frozen_through;
    let mut later = event(Uuid::from_u128(5), session_id, source_id);
    later.seq = 2;
    store.upsert_event(&later).expect("later Core event");

    let snapshot = coalesced_journal_snapshot_through(
        &store,
        StoreJournalPosition {
            generation: genesis.position.generation,
            sequence: 0,
        },
        &target,
    )
    .expect("bounded snapshot");

    assert_eq!(snapshot.frozen_through, target);
    assert_eq!(snapshot.next_position, target.position);
    assert!(!snapshot.has_more);
    assert!(snapshot
        .records
        .iter()
        .all(|record| record.sequence <= target.position.sequence));
    assert!(
        store
            .projection_journal_snapshot(Some(target.position))
            .expect("retained later suffix")
            .records
            .iter()
            .any(|record| record.sequence > target.position.sequence),
        "later Core record was not retained beyond the bounded target"
    );
}

#[test]
fn forged_store_target_coordinates_and_digest_are_rejected_before_helper_io() {
    let temp = tempdir().expect("temp dir");
    let store = Store::open(temp.path().join("ctx.db")).expect("open Store");
    let target = store
        .activate_projection_journal(PROTOCOL_FINGERPRINT)
        .expect("activate journal");
    let forged = [
        StoreJournalCheckpoint {
            cumulative_digest: "a".repeat(64),
            ..target.clone()
        },
        StoreJournalCheckpoint {
            position: StoreJournalPosition {
                generation: target.position.generation.saturating_add(1),
                sequence: target.position.sequence,
            },
            ..target.clone()
        },
        StoreJournalCheckpoint {
            position: StoreJournalPosition {
                generation: target.position.generation,
                sequence: target.position.sequence.saturating_add(1),
            },
            ..target.clone()
        },
        StoreJournalCheckpoint {
            contract_fingerprint: "f".repeat(64),
            ..target
        },
    ];
    for forged in forged {
        let helper_called = Cell::new(false);
        sync_nativepath_group_through(&store, &forged, &mut |_, _| {
            helper_called.set(true);
            bail!("helper must not be called")
        })
        .expect_err("forged target must fail closed");
        assert!(!helper_called.get());
    }
}

#[test]
fn nativepath_sync_stops_and_prunes_at_the_receipt_then_retries_idempotently() {
    let temp = tempdir().expect("temp dir");
    let store = Store::open(temp.path().join("ctx.db")).expect("open Store");
    let source_id = Uuid::from_u128(12);
    let session_id = Uuid::from_u128(13);
    store
        .upsert_capture_source(&source(source_id))
        .expect("source");
    store
        .upsert_session(&session(session_id, source_id))
        .expect("session");
    let active = store
        .activate_projection_journal(PROTOCOL_FINGERPRINT)
        .expect("activate journal");
    store
        .upsert_event(&event(Uuid::from_u128(14), session_id, source_id))
        .expect("target event");
    let target = store
        .projection_journal_snapshot(None)
        .expect("target snapshot")
        .frozen_through;
    let mut later = event(Uuid::from_u128(15), session_id, source_id);
    later.seq = 2;
    store.upsert_event(&later).expect("later event");
    let later_checkpoint = store
        .projection_journal_snapshot(None)
        .expect("later checkpoint")
        .frozen_through;
    let helper_prior = JournalCheckpoint {
        position: JournalPosition {
            generation: active.position.generation,
            sequence: 0,
        },
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        cumulative_digest: initial_journal_digest(active.position.generation),
    };
    let sent_pages = Cell::new(0_u32);
    let disposition =
        sync_nativepath_group_through(&store, &target, &mut |message, _| match message {
            HostMessage::Status(_) => Ok(HelperMessage::Status(StatusResult {
                state: GraphState::Ready,
                checkpoint: Some(helper_prior.clone()),
            })),
            HostMessage::SyncJournal(request) => {
                sent_pages.set(sent_pages.get() + 1);
                assert_eq!(
                    store_checkpoint(&request.frozen_through),
                    target,
                    "request escaped the Core receipt target"
                );
                assert!(request
                    .records
                    .iter()
                    .all(|record| record.sequence <= target.position.sequence));
                Ok(exact_journal_sync_response(&request))
            }
            _ => bail!("unexpected helper request"),
        })
        .expect("bounded canonical sync");
    assert_eq!(disposition, NativeProAdvanceDisposition::Advanced);
    assert!(sent_pages.get() > 0);

    let retained = store
        .projection_journal_snapshot(None)
        .expect("retained suffix");
    assert_eq!(retained.frozen_through, later_checkpoint);
    assert!(retained
        .records
        .iter()
        .all(|record| record.sequence > target.position.sequence));

    let retry_sync_called = Cell::new(false);
    let retry = sync_nativepath_group_through(&store, &target, &mut |message, _| match message {
        HostMessage::Status(_) => Ok(HelperMessage::Status(StatusResult {
            state: GraphState::Ready,
            checkpoint: Some(protocol_checkpoint(target.clone())),
        })),
        HostMessage::SyncJournal(_) => {
            retry_sync_called.set(true);
            bail!("AlreadyCommitted Core retry must not replay canonical pages")
        }
        _ => bail!("unexpected helper request"),
    })
    .expect("idempotent Core retry");
    assert_eq!(retry, NativeProAdvanceDisposition::AlreadyAdvanced);
    assert!(!retry_sync_called.get());

    let beyond = sync_nativepath_group_through(&store, &target, &mut |message, _| match message {
        HostMessage::Status(_) => Ok(HelperMessage::Status(StatusResult {
            state: GraphState::Ready,
            checkpoint: Some(protocol_checkpoint(later_checkpoint.clone())),
        })),
        HostMessage::SyncJournal(_) => bail!("verified later helper checkpoint must not replay"),
        _ => bail!("unexpected helper request"),
    })
    .expect("verified later helper checkpoint");
    assert_eq!(beyond, NativeProAdvanceDisposition::AlreadyAdvanced);
}

#[test]
fn core_only_can_remain_inactive_and_later_activation_builds_the_exact_baseline() {
    let inactive_temp = tempdir().expect("inactive temp dir");
    let inactive = Store::open(inactive_temp.path().join("ctx.db")).expect("inactive Store");
    assert!(inactive.projection_journal_snapshot(None).is_err());

    let temp = tempdir().expect("activation temp dir");
    let store = Store::open(temp.path().join("ctx.db")).expect("activation Store");
    let source_id = Uuid::from_u128(22);
    let session_id = Uuid::from_u128(23);
    store
        .upsert_capture_source(&source(source_id))
        .expect("source");
    store
        .upsert_session(&session(session_id, source_id))
        .expect("session");
    store
        .upsert_event(&event(Uuid::from_u128(24), session_id, source_id))
        .expect("canonical event");
    let pages = Cell::new(0_u32);
    let checkpoint =
        prepare_nativepath_projection_journal(&store, &mut |message, _| match message {
            HostMessage::Status(_) => Ok(HelperMessage::Status(StatusResult {
                state: GraphState::NotMaterialized,
                checkpoint: None,
            })),
            HostMessage::SyncJournal(request) => {
                pages.set(pages.get() + 1);
                Ok(exact_journal_sync_response(&request))
            }
            _ => bail!("unexpected helper request"),
        })
        .expect("later activation");

    assert!(checkpoint.position.sequence > 0);
    assert!(pages.get() > 0);
    assert_eq!(
        protocol_checkpoint(
            store
                .projection_journal_snapshot(None)
                .expect("active checkpoint")
                .frozen_through
        ),
        checkpoint
    );
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
        prior_checkpoint: prior.clone(),
        context: JournalContextWindow {
            base_checkpoint: protocol_checkpoint(snapshot.context.base_checkpoint),
            records: snapshot
                .context
                .records
                .into_iter()
                .map(protocol_journal_record)
                .collect(),
        },
        frozen_through: protocol_checkpoint(snapshot.frozen_through),
        authorized_repository_roots: snapshot.authorized_repository_roots,
        records: snapshot
            .records
            .into_iter()
            .map(protocol_journal_record)
            .collect(),
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
fn acknowledged_pages_supply_exact_transient_context_to_the_next_request() {
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
    for index in 1..=600_u64 {
        let mut fixture = event(
            Uuid::from_u128(u128::from(index).saturating_add(100)),
            session_id,
            source_id,
        );
        fixture.seq = index;
        fixture.payload = json!({"body": format!("context-{index}")});
        store.upsert_event(&fixture).expect("append event");
    }

    let mut requests = Vec::new();
    let checkpoint =
        prepare_nativepath_projection_journal(&store, &mut |message, _| match message {
            HostMessage::Status(_) => Ok(HelperMessage::Status(StatusResult {
                state: GraphState::NotMaterialized,
                checkpoint: None,
            })),
            HostMessage::SyncJournal(request) => {
                request.validate().expect("valid context request");
                requests.push(request.clone());
                Ok(exact_journal_sync_response(&request))
            }
            _ => bail!("unexpected helper request"),
        })
        .expect("materialize journal pages");

    assert_eq!(checkpoint.position.sequence, 600);
    assert_eq!(requests.len(), 2);
    assert!(requests[0].context.records.is_empty());
    assert_eq!(requests[0].records.len(), MAX_JOURNAL_RECORDS_PER_BATCH);
    assert_eq!(requests[1].prior_checkpoint.position.sequence, 512);
    assert_eq!(requests[1].context.base_checkpoint.position.sequence, 448);
    assert_eq!(requests[1].context.records.len(), 64);
    assert_eq!(requests[1].context.records[0].sequence, 449);
    assert_eq!(requests[1].context.records[63].sequence, 512);
    assert_eq!(requests[1].records.first().unwrap().sequence, 513);
    assert_eq!(requests[1].records.last().unwrap().sequence, 600);
}

#[test]
fn many_near_maximum_records_never_coalesce_beyond_one_bounded_store_page() {
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
    let large_body = "x".repeat(2_900_000);
    for index in 1..=12_u64 {
        let mut fixture = event(
            Uuid::from_u128(u128::from(index).saturating_add(100)),
            session_id,
            source_id,
        );
        fixture.seq = index;
        fixture.payload = json!({"body": &large_body, "ordinal": index});
        store.upsert_event(&fixture).expect("append large event");
    }

    let mut request_shapes = Vec::new();
    let checkpoint =
        prepare_nativepath_projection_journal(&store, &mut |message, _| match message {
            HostMessage::Status(_) => Ok(HelperMessage::Status(StatusResult {
                state: GraphState::NotMaterialized,
                checkpoint: None,
            })),
            HostMessage::SyncJournal(request) => {
                request.validate().expect("valid bounded request");
                request_shapes.push((
                    request.records.len(),
                    serde_json::to_vec(&request.records)
                        .expect("encode current records")
                        .len(),
                    journal_sync_envelope_bytes(&request).expect("encode exact envelope"),
                ));
                Ok(exact_journal_sync_response(&request))
            }
            _ => bail!("unexpected helper request"),
        })
        .expect("materialize bounded large journal pages");

    assert_eq!(checkpoint.position.sequence, 12);
    assert_eq!(
        request_shapes
            .iter()
            .map(|(records, _, _)| records)
            .sum::<usize>(),
        12
    );
    assert!(
        request_shapes.len() > 1,
        "large legal records must require multiple bounded requests"
    );
    assert!(request_shapes
        .iter()
        .all(|(records, page_bytes, envelope_bytes)| {
            *records <= 2
                && *page_bytes <= ctx_history_store::PROJECTION_JOURNAL_MAX_PAGE_BYTES
                && *envelope_bytes <= MAX_JOURNAL_SYNC_ENVELOPE_BYTES
        }));
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
    let context_record = record(1, &initial, 1);
    let mut records = Vec::new();
    let mut prior_digest = context_record.cumulative_digest.clone();
    for sequence in 2..=7 {
        let next = record(sequence, &prior_digest, 3 * 1024 * 1024);
        prior_digest.clone_from(&next.cumulative_digest);
        records.push(next);
    }
    let original_records = records.clone();
    let frozen = JournalCheckpoint {
        position: JournalPosition {
            generation: 1,
            sequence: 7,
        },
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        cumulative_digest: records.last().unwrap().cumulative_digest.clone(),
    };
    let request = JournalSyncRequest {
        mode: JournalSyncMode::Incremental,
        canonical_schema_version: 47,
        canonical_schema_identity: "ctx-store-schema-47-final-v3".to_owned(),
        projection_contract_version: ctx_pro_host_protocol::PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        prior_checkpoint: JournalCheckpoint {
            position: JournalPosition {
                generation: 1,
                sequence: 1,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: context_record.cumulative_digest.clone(),
        },
        context: JournalContextWindow {
            base_checkpoint: JournalCheckpoint {
                position: JournalPosition {
                    generation: 1,
                    sequence: 0,
                },
                contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
                cumulative_digest: initial,
            },
            records: vec![context_record],
        },
        frozen_through: frozen,
        authorized_repository_roots: roots,
        records,
    };
    assert!(journal_sync_envelope_bytes(&request).unwrap() > MAX_JOURNAL_SYNC_ENVELOPE_BYTES);

    let fitted = fit_journal_sync_request(request).expect("fit request");
    assert!(!fitted.records.is_empty());
    assert!(fitted.records.len() < 6);
    assert_eq!(fitted.context.records.len(), 1);
    assert_eq!(fitted.context.records[0].sequence, 1);
    assert!(journal_sync_envelope_bytes(&fitted).unwrap() <= MAX_JOURNAL_SYNC_ENVELOPE_BYTES);
    if fitted.records.len() < original_records.len() {
        let mut one_more = fitted.clone();
        one_more
            .records
            .push(original_records[fitted.records.len()].clone());
        assert!(
            journal_sync_envelope_bytes(&one_more).unwrap() > MAX_JOURNAL_SYNC_ENVELOPE_BYTES,
            "fitting must retain the largest exact prefix"
        );
    }
    fitted.validate().expect("trimmed page remains valid");
    assert_eq!(
        fitted.committed_checkpoint().position.sequence,
        1 + fitted.records.len() as u64
    );
    assert_eq!(fitted.frozen_through.position.sequence, 7);
}

#[test]
fn blame_capabilities_require_git_only_for_file_targets() {
    assert_eq!(
        required_blame_capabilities(&BlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: None,
            lines: None,
        }),
        BTreeSet::from([Capability::Query, Capability::GitRead])
    );
    assert_eq!(
        required_blame_capabilities(&BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: None,
        }),
        BTreeSet::from([Capability::Query])
    );
    assert_eq!(
        required_blame_capabilities(&BlameTarget::PullRequest {
            selector: "https://github.com/ctxrs/ctx/pull/42".to_owned(),
            repository: None,
        }),
        BTreeSet::from([Capability::Query])
    );
}

#[test]
fn blame_client_binds_responses_to_the_original_request_context() {
    let request = ctx_pro_host_protocol::BlameRequest {
        target: BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
        },
        limit: 10,
        cursor: None,
        expected_snapshot: ctx_pro_host_protocol::QuerySnapshotExpectation {
            checkpoint: JournalCheckpoint {
                position: JournalPosition {
                    generation: 1,
                    sequence: 0,
                },
                contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
                cumulative_digest: initial_journal_digest(1),
            },
            projection_pending: false,
        },
    };
    let repository = ctx_pro_host_protocol::ResourceRef {
        id: "repository:1".to_owned(),
        kind: ctx_pro_host_protocol::ResourceKind::Repository,
        display: "ctxrs/ctx".to_owned(),
    };
    let explicit_absence = BlameResult {
        target: ctx_pro_host_protocol::ResolvedBlameTarget::Commit {
            commit: ctx_pro_host_protocol::ResourceRef {
                id: "commit:1".to_owned(),
                kind: ctx_pro_host_protocol::ResourceKind::Commit,
                display: "0123456789abcdef".to_owned(),
            },
            repository: repository.clone(),
        },
        git_snapshot: None,
        matches: Vec::new(),
        evidence: Vec::new(),
        next: None,
    };
    validate_blame_response(&request, &explicit_absence).unwrap();

    let wrong_variant = BlameResult {
        target: ctx_pro_host_protocol::ResolvedBlameTarget::PullRequest {
            selector: "42".to_owned(),
            pull_request: ctx_pro_host_protocol::ResourceRef {
                id: "pull_request:1".to_owned(),
                kind: ctx_pro_host_protocol::ResourceKind::PullRequest,
                display: "https://github.com/ctxrs/ctx/pull/42".to_owned(),
            },
            repository,
        },
        git_snapshot: None,
        matches: Vec::new(),
        evidence: Vec::new(),
        next: None,
    };
    let error = validate_blame_response(&request, &wrong_variant)
        .expect_err("cross-target response must fail closed");
    assert_eq!(stable_error_code(&error), Some("invalid_response"));
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
fn retryable_rebuild_contention_is_bounded_to_one_retry() {
    let contention = || {
        let mut error = ctx_pro_host_protocol::ProtocolError::new(
            ctx_pro_host_protocol::ErrorClass::NotMaterialized,
            "untrusted rebuild-owner detail",
        );
        error.retryable = true;
        protocol_error(error)
    };
    let calls = Cell::new(0_u32);
    let result = retry_materialization_once(|| {
        calls.set(calls.get() + 1);
        Err::<(), _>(contention())
    });
    assert!(result.is_err());
    assert_eq!(calls.get(), 2);

    let calls = Cell::new(0_u32);
    let result = retry_materialization_once(|| {
        calls.set(calls.get() + 1);
        (calls.get() == 2).then_some(()).ok_or_else(contention)
    });
    assert!(result.is_ok());
    assert_eq!(calls.get(), 2);
}

#[test]
fn unchanged_ready_graph_skips_a_second_journal_sync() {
    let checkpoint = JournalCheckpoint {
        position: JournalPosition {
            generation: 1,
            sequence: 7,
        },
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        cumulative_digest: "a".repeat(64),
    };

    assert!(!journal_sync_required(
        GraphState::Ready,
        true,
        &checkpoint,
        &checkpoint,
        0,
    ));
    assert!(journal_sync_required(
        GraphState::Ready,
        false,
        &checkpoint,
        &checkpoint,
        0,
    ));
}

#[test]
fn empty_unmaterialized_graph_still_sends_its_initial_baseline() {
    let checkpoint = JournalCheckpoint {
        position: JournalPosition {
            generation: 1,
            sequence: 0,
        },
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        cumulative_digest: initial_journal_digest(1),
    };

    assert!(journal_sync_required(
        GraphState::NotMaterialized,
        false,
        &checkpoint,
        &checkpoint,
        0,
    ));
}

#[test]
fn publication_requires_the_canonical_frontier_to_remain_frozen() {
    let temp = tempdir().expect("temp dir");
    let db_path = database_path(temp.path().to_path_buf());
    let store = Store::open(&db_path).expect("open Store");
    store
        .activate_projection_journal(PROTOCOL_FINGERPRINT)
        .expect("activate journal");
    let expected = protocol_checkpoint(
        store
            .projection_journal_snapshot(None)
            .expect("read initial frontier")
            .frozen_through,
    );
    verify_canonical_frontier(temp.path(), &expected).expect("unchanged frontier");

    let source_id = Uuid::from_u128(99);
    let session_id = Uuid::from_u128(100);
    store
        .upsert_capture_source(&source(source_id))
        .expect("append source");
    store
        .upsert_session(&session(session_id, source_id))
        .expect("append session");
    store
        .upsert_event(&event(Uuid::from_u128(101), session_id, source_id))
        .expect("advance canonical journal");
    let error = verify_canonical_frontier(temp.path(), &expected)
        .expect_err("advanced canonical history must prevent publication");
    assert_eq!(stable_error_code(&error), Some("not_materialized"));
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
fn write_graph_key_deletion_helper(
    path: &Path,
    key_present_before: bool,
    key_present_after: bool,
    challenge_base64url: &str,
) {
    let before = if key_present_before { "True" } else { "False" };
    let after = if key_present_after { "True" } else { "False" };
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
    'helper_version':'graph-key-deletion-test',
    'capabilities':['graph_key_deletion'],
    'authorization_challenge_base64url':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'
}})
prepare = receive()
if prepare['message']['kind'] != 'prepare_graph_key_deletion':
    sys.exit(21)
challenge = '{challenge_base64url}'
send(prepare, 'graph_key_deletion_prepared', {{
    'challenge_base64url':challenge,
    'expires_at_unix':2000000000,
    'key_present':{before}
}})
if not {before}:
    sys.exit(0)
confirm = receive()
if confirm['message']['kind'] != 'confirm_graph_key_deletion':
    sys.exit(22)
if confirm['message']['body']['authorization']['challenge_base64url'] != challenge:
    sys.exit(23)
send(confirm, 'graph_key_deleted', {{'deleted':True}})
verify = receive()
if verify['message']['kind'] != 'prepare_graph_key_deletion':
    sys.exit(24)
send(verify, 'graph_key_deletion_prepared', {{
    'challenge_base64url':challenge,
    'expires_at_unix':2000000000,
    'key_present':{after}
}})
"#
    );
    fs::write(path, script).expect("write graph-key deletion helper");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("make graph-key deletion helper executable");
}

#[cfg(target_os = "linux")]
fn write_namespace_graph_key_deletion_helper(
    path: &Path,
    production: (&str, &str, &str),
    staging: (&str, &str, &str),
) {
    let (production_thumbprint, production_public_key, production_issuer) = production;
    let (staging_thumbprint, staging_public_key, staging_issuer) = staging;
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
    'helper_version':'namespace-graph-key-deletion-test',
    'capabilities':['graph_key_deletion'],
    'authorization_challenge_base64url':'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'
}})
prepare = receive()
if prepare['message']['kind'] != 'prepare_graph_key_deletion':
    sys.exit(21)
expected = {{
    '{production_thumbprint}': ('{production_public_key}', '{production_issuer}'),
    '{staging_thumbprint}': ('{staging_public_key}', '{staging_issuer}'),
}}
expected_thumbprint = prepare['message']['body']['installation_key_thumbprint']
if expected_thumbprint not in expected:
    sys.exit(22)
expected_public_key, expected_issuer = expected[expected_thumbprint]
challenge = 'BAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ'
send(prepare, 'graph_key_deletion_prepared', {{
    'challenge_base64url':challenge,
    'expires_at_unix':2000000000,
    'key_present':True
}})
confirm = receive()
if confirm['message']['kind'] != 'confirm_graph_key_deletion':
    sys.exit(23)
authorization = confirm['message']['body']['authorization']
if authorization['challenge_base64url'] != challenge:
    sys.exit(24)
if authorization['installation_public_key_base64url'] != expected_public_key:
    sys.exit(25)
grant = authorization['entitlement']['grant']
if grant['installation_key_thumbprint'] != expected_thumbprint:
    sys.exit(26)
if grant['issuer'] != expected_issuer:
    sys.exit(27)
send(confirm, 'graph_key_deleted', {{'deleted':True}})
verify = receive()
if verify['message']['kind'] != 'prepare_graph_key_deletion':
    sys.exit(28)
if verify['message']['body']['installation_key_thumbprint'] != expected_thumbprint:
    sys.exit(29)
send(verify, 'graph_key_deletion_prepared', {{
    'challenge_base64url':challenge,
    'expires_at_unix':2000000000,
    'key_present':False
}})
"#
    );
    fs::write(path, script).expect("write namespace graph-key deletion helper");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("make namespace graph-key deletion helper executable");
}

#[cfg(unix)]
#[test]
fn graph_key_deletion_uses_challenge_and_verifies_selected_record_absent() {
    let temp = tempdir().expect("temp dir");
    crate::identity::installation_id(temp.path()).expect("installation identity");
    let helper = temp.path().join("ctx-pro-delete-key");
    let challenge = ctx_pro_host_protocol::base64url(&[4; GRAPH_KEY_DELETION_CHALLENGE_BYTES]);
    write_graph_key_deletion_helper(&helper, true, false, &challenge);
    let required = BTreeSet::from([Capability::GraphKeyDeletion]);
    let mut client = ProClient::connect_to_path_with_authorization_mode(
        temp.path(),
        &helper,
        None,
        &required,
        None,
        false,
    )
    .expect("helper handshake");
    let authorization = RecordingAuthorization {
        calls: Cell::new(0),
    };
    delete_graph_key_with_client(
        &mut client,
        &ctx_pro_host_protocol::base64url(&[8; 32]),
        |value| authorization.authorization_for_challenge(value),
    )
    .expect("delete and verify graph key");
    assert_eq!(authorization.calls.get(), 1);
}

#[cfg(unix)]
#[test]
fn graph_key_deletion_is_idempotent_without_loading_authorization() {
    let temp = tempdir().expect("temp dir");
    crate::identity::installation_id(temp.path()).expect("installation identity");
    let helper = temp.path().join("ctx-pro-delete-missing-key");
    let challenge = ctx_pro_host_protocol::base64url(&[5; GRAPH_KEY_DELETION_CHALLENGE_BYTES]);
    write_graph_key_deletion_helper(&helper, false, false, &challenge);
    let required = BTreeSet::from([Capability::GraphKeyDeletion]);
    let mut client = ProClient::connect_to_path_with_authorization_mode(
        temp.path(),
        &helper,
        None,
        &required,
        None,
        false,
    )
    .expect("helper handshake");
    let authorization = RecordingAuthorization {
        calls: Cell::new(0),
    };
    delete_graph_key_with_client(
        &mut client,
        &ctx_pro_host_protocol::base64url(&[9; 32]),
        |value| authorization.authorization_for_challenge(value),
    )
    .expect("accept missing graph key");
    assert_eq!(authorization.calls.get(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn dual_namespace_graph_key_deletion_uses_each_exact_vault_authorization() {
    use ctx_pro_host_protocol::{
        base64url, installation_key_thumbprint, EntitlementAccessKind, EntitlementCapability,
        EntitlementGrant, SignedEntitlement, ED25519_SIGNATURE_BYTES, ENTITLEMENT_SCHEMA_VERSION,
        INSTALLATION_PUBLIC_KEY_BYTES,
    };

    fn store_namespace(
        data_root: &Path,
        namespace: CredentialVaultNamespace,
        seed: u8,
        issuer: &str,
        key_id: &str,
    ) -> (String, String) {
        let signing_key = SigningKey::from_bytes(&[seed; INSTALLATION_PUBLIC_KEY_BYTES]);
        let public_key = signing_key.verifying_key().to_bytes();
        let thumbprint = installation_key_thumbprint(&public_key);
        let vault =
            PlatformCredentialVault::production(data_root, namespace).expect("open exact vault");
        vault
            .store(&CredentialRecord::InstallationSigningKey(
                InstallationSigningKeySeed::from_bytes([seed; INSTALLATION_PUBLIC_KEY_BYTES]),
            ))
            .expect("store exact installation key");
        vault
            .store(&CredentialRecord::SignedEntitlement(
                BoundedSignedEntitlement::new(SignedEntitlement {
                    grant: EntitlementGrant {
                        schema_version: ENTITLEMENT_SCHEMA_VERSION,
                        issuer: issuer.to_owned(),
                        key_id: key_id.to_owned(),
                        grant_id: format!("grant-{seed}"),
                        subject: "subject".to_owned(),
                        account_id: "account".to_owned(),
                        product: "ctx-local-pro".to_owned(),
                        access_kind: EntitlementAccessKind::Active,
                        installation_key_thumbprint: thumbprint.clone(),
                        issued_at_unix: 1_800_000_000,
                        not_before_unix: 1_799_999_700,
                        refresh_after_unix: 1_800_345_600,
                        access_deadline_unix: 1_802_592_000,
                        grace_deadline_unix: 1_803_196_800,
                        expires_at_unix: 1_800_604_800,
                        minimum_helper_protocol: PROTOCOL_VERSION,
                        revocation_epoch: 0,
                        capabilities: BTreeSet::from([EntitlementCapability::GraphRead]),
                    },
                    signature_base64url: base64url(&[seed; ED25519_SIGNATURE_BYTES]),
                })
                .expect("bound entitlement"),
            ))
            .expect("store exact entitlement");
        (thumbprint, base64url(&public_key))
    }

    let root = tempdir().expect("temp dir");
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).expect("protect data root");
    crate::identity::installation_id(root.path()).expect("installation identity");
    let pro = ctx_pro_host_protocol::ProFilesystemLayout::new(root.path()).pro_root();
    fs::create_dir(&pro).expect("create Pro root");
    fs::set_permissions(&pro, fs::Permissions::from_mode(0o700)).expect("protect Pro root");
    let backend_marker = pro.join(".ctx-pro.credential-backend-v1");
    fs::write(&backend_marker, b"ctx-pro-credential-backend-v1:file\n")
        .expect("select file credential vault");
    fs::set_permissions(&backend_marker, fs::Permissions::from_mode(0o600))
        .expect("protect backend marker");

    let production_issuer = "https://pro.ctx.rs";
    let staging_issuer = "https://pro-staging.ctx.rs";
    let (production_thumbprint, production_public_key) = store_namespace(
        root.path(),
        CredentialVaultNamespace::Production,
        31,
        production_issuer,
        "production-2026-07-v1",
    );
    let (staging_thumbprint, staging_public_key) = store_namespace(
        root.path(),
        CredentialVaultNamespace::Staging,
        32,
        staging_issuer,
        "staging-2026-07-v2",
    );

    let mismatch = StoredAuthorizationProvider::load_for_graph_key_deletion(
        root.path(),
        CredentialVaultNamespace::Staging,
        &production_thumbprint,
    )
    .err()
    .expect("cross-namespace thumbprint must fail closed");
    assert!(
        mismatch.to_string().starts_with("entitlement_invalid:"),
        "{mismatch:#}"
    );

    let graph = pro.join(ctx_pro_host_protocol::PRO_GRAPH_FILE_NAME);
    fs::write(&graph, b"encrypted graph fixture").expect("write encrypted graph fixture");
    let helper = root.path().join("ctx-pro-delete-dual-namespace");
    write_namespace_graph_key_deletion_helper(
        &helper,
        (
            &production_thumbprint,
            &production_public_key,
            production_issuer,
        ),
        (&staging_thumbprint, &staging_public_key, staging_issuer),
    );
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg(DUAL_NAMESPACE_DELETION_RUNNER)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(DUAL_NAMESPACE_DELETION_MODE_ENV, "run")
        .env(DUAL_NAMESPACE_DELETION_ROOT_ENV, root.path())
        .env("CTX_PRO_HELPER", &helper)
        .status()
        .expect("run production dual-namespace deletion");
    assert!(status.success(), "deletion subprocess exited with {status}");
    assert!(!graph.exists(), "encrypted graph was not deleted");

    let cleanup_phase: serde_json::Value = serde_json::from_slice(
        &fs::read(pro.join(".ctx-pro.graph-key-cleanup.json"))
            .expect("read durable graph-key cleanup phase"),
    )
    .expect("decode durable graph-key cleanup phase");
    assert_eq!(cleanup_phase["schema_version"], 2);
    assert_eq!(
        cleanup_phase["targets"],
        json!([
            {
                "namespace": "production",
                "installation_key_thumbprint": production_thumbprint,
            },
            {
                "namespace": "staging",
                "installation_key_thumbprint": staging_thumbprint,
            },
        ])
    );
}

#[cfg(target_os = "linux")]
#[test]
fn dual_namespace_graph_key_deletion_subprocess_runner() -> anyhow::Result<()> {
    if std::env::var(DUAL_NAMESPACE_DELETION_MODE_ENV).as_deref() != Ok("run") {
        return Ok(());
    }
    let data_root = std::env::var_os(DUAL_NAMESPACE_DELETION_ROOT_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("missing dual-namespace deletion data root"))?;
    LocalDeletionService::production().delete_graph_data(&data_root)
}

#[cfg(unix)]
#[test]
fn graph_key_deletion_fails_closed_when_post_delete_key_is_present() {
    let temp = tempdir().expect("temp dir");
    crate::identity::installation_id(temp.path()).expect("installation identity");
    let helper = temp.path().join("ctx-pro-retained-key");
    let challenge = ctx_pro_host_protocol::base64url(&[6; GRAPH_KEY_DELETION_CHALLENGE_BYTES]);
    write_graph_key_deletion_helper(&helper, true, true, &challenge);
    let required = BTreeSet::from([Capability::GraphKeyDeletion]);
    let mut client = ProClient::connect_to_path_with_authorization_mode(
        temp.path(),
        &helper,
        None,
        &required,
        None,
        false,
    )
    .expect("helper handshake");
    let authorization = RecordingAuthorization {
        calls: Cell::new(0),
    };
    let error = delete_graph_key_with_client(
        &mut client,
        &ctx_pro_host_protocol::base64url(&[10; 32]),
        |value| authorization.authorization_for_challenge(value),
    )
    .expect_err("retained key must fail verification");
    assert_eq!(stable_error_code(&error), Some("key_store_unavailable"));
    assert_eq!(authorization.calls.get(), 1);
}

#[cfg(unix)]
#[test]
fn graph_key_deletion_rejects_invalid_helper_challenge_before_authorization() {
    let temp = tempdir().expect("temp dir");
    crate::identity::installation_id(temp.path()).expect("installation identity");
    let helper = temp.path().join("ctx-pro-invalid-delete-challenge");
    write_graph_key_deletion_helper(&helper, true, false, "invalid");
    let required = BTreeSet::from([Capability::GraphKeyDeletion]);
    let mut client = ProClient::connect_to_path_with_authorization_mode(
        temp.path(),
        &helper,
        None,
        &required,
        None,
        false,
    )
    .expect("helper handshake");
    let authorization = RecordingAuthorization {
        calls: Cell::new(0),
    };
    let error = delete_graph_key_with_client(
        &mut client,
        &ctx_pro_host_protocol::base64url(&[11; 32]),
        |value| authorization.authorization_for_challenge(value),
    )
    .expect_err("invalid challenge must fail");
    assert_eq!(stable_error_code(&error), Some("invalid_response"));
    assert_eq!(authorization.calls.get(), 0);
}

#[path = "client_tests/transport.rs"]
mod transport;
