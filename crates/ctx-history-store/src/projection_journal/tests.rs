use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, EntityTimestamps, Event, EventRole, EventType, Fidelity, FileChangeKind,
    FileTouched, Run, RunStatus, RunType, Session, SessionHistoryArchive, SessionStatus,
    SyncMetadata, VcsChange, VcsChangeKind, VcsHost, VcsKind, VcsWorkspace,
};
use rusqlite::{
    hooks::{AuthAction, Authorization},
    Connection,
};
use serde_json::json;
use tempfile::tempdir;

use super::*;

const FINGERPRINT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn now() -> DateTime<Utc> {
    "2026-07-22T00:00:00Z".parse().unwrap()
}

fn event(id: Uuid, seq: u64, body: Value) -> Event {
    Event {
        id,
        seq,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: now(),
        capture_source_id: None,
        payload: body,
        payload_blob_id: None,
        dedupe_key: None,
        sync: SyncMetadata {
            fidelity: Fidelity::Imported,
            metadata: json!({"fixture_line": seq}),
            ..SyncMetadata::default()
        },
    }
}

fn timestamps() -> EntityTimestamps {
    EntityTimestamps {
        created_at: now(),
        updated_at: now(),
    }
}

fn source(id: Uuid) -> CaptureSource {
    CaptureSource {
        id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: "machine".to_owned(),
            process_id: None,
            cwd: Some("/private/repository".to_owned()),
            raw_source_path: Some("/provider/session.jsonl".to_owned()),
            source_format: Some("codex_session_jsonl_tree".to_owned()),
            source_root: Some("/private/provider-root".to_owned()),
            source_identity: Some("source-identity".to_owned()),
            external_session_id: Some("external-session".to_owned()),
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
        external_session_id: Some("external-session".to_owned()),
        external_agent_id: Some("agent-1".to_owned()),
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: now(),
        ended_at: None,
        timestamps: timestamps(),
        sync: SyncMetadata::default(),
    }
}

fn run(id: Uuid, session_id: Uuid, source_id: Uuid) -> Run {
    Run {
        id,
        history_record_id: None,
        session_id: Some(session_id),
        run_type: RunType::Command,
        status: RunStatus::Succeeded,
        started_at: now(),
        ended_at: Some(now()),
        exit_code: Some(0),
        cwd: Some("/private/repository".to_owned()),
        command_preview: Some("first command".to_owned()),
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(),
        source_id: Some(source_id),
        sync: SyncMetadata::default(),
    }
}

fn open_store() -> (tempfile::TempDir, Store) {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    (temp, store)
}

fn record_checkpoint(record: &ProjectionJournalRecord) -> JournalCheckpoint {
    JournalCheckpoint {
        position: JournalPosition {
            generation: record.generation,
            sequence: record.sequence,
        },
        contract_fingerprint: FINGERPRINT.to_owned(),
        cumulative_digest: record.cumulative_digest.clone(),
    }
}

fn sqlite_family_bytes(path: &std::path::Path) -> u64 {
    ["", "-wal", "-shm", "-journal", ".previous"]
        .into_iter()
        .map(|suffix| {
            let candidate = std::path::PathBuf::from(format!("{}{}", path.display(), suffix));
            std::fs::metadata(candidate)
                .map(|metadata| metadata.len())
                .unwrap_or(0)
        })
        .sum()
}

#[path = "tests/context.rs"]
mod context;

#[test]
fn activation_is_deterministic_and_free_users_have_no_rows() {
    let entity_id = Uuid::parse_str("018fe2e4-2266-7000-8000-000000000001").unwrap();
    let mut snapshots = Vec::new();
    for _ in 0..2 {
        let (_temp, store) = open_store();
        store
            .upsert_event(&event(entity_id, 1, json!({"body": "same"})))
            .unwrap();
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT COALESCE(SUM(record_count), 0) FROM projection_journal_chunks",
                    [],
                    |row| { row.get::<_, i64>(0) }
                )
                .unwrap(),
            0
        );
        let checkpoint = store.activate_projection_journal(FINGERPRINT).unwrap();
        assert_eq!(
            checkpoint.position,
            JournalPosition {
                generation: 1,
                sequence: 1
            }
        );
        let snapshot = store.projection_journal_snapshot(None).unwrap();
        assert_eq!(
            snapshot.canonical_schema_identity,
            crate::CANONICAL_PROJECTION_SCHEMA_IDENTITY
        );
        snapshots.push(snapshot);
    }
    assert_eq!(snapshots[0], snapshots[1]);
}

#[test]
fn many_source_noop_exact_state_checks_never_read_retained_chunks() {
    let (_temp, store) = open_store();
    store
        .upsert_event(&event(Uuid::new_v4(), 1, json!({"body": "high water"})))
        .unwrap();
    let high_water = store.activate_projection_journal(FINGERPRINT).unwrap();

    store
        .conn
        .authorizer(Some(|context: rusqlite::hooks::AuthContext<'_>| {
            if matches!(
                context.action,
                AuthAction::Read {
                    table_name: "projection_journal_chunks",
                    ..
                }
            ) {
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        }));
    // Model one journal readiness/checkpoint validation for every source in a
    // large unchanged Codex root. The authorizer makes any retained-record
    // decode fail, so this also prevents an accidental O(sources * journal)
    // regression in the no-op path.
    for _ in 0..4_096 {
        assert_eq!(store.projection_journal_checkpoint().unwrap(), high_water);
        assert!(store
            .verify_projection_journal_checkpoint(&high_water)
            .unwrap());
    }
    let mut forged = high_water;
    forged.cumulative_digest = "f".repeat(64);
    assert!(!store.verify_projection_journal_checkpoint(&forged).unwrap());
    store
        .conn
        .authorizer(None::<fn(rusqlite::hooks::AuthContext<'_>) -> Authorization>);
}

#[test]
fn retained_interior_checkpoint_verification_fails_closed_when_its_chunk_is_missing() {
    let (_temp, store) = open_store();
    store
        .upsert_event(&event(Uuid::new_v4(), 1, json!({"body": "first"})))
        .unwrap();
    store
        .upsert_event(&event(Uuid::new_v4(), 2, json!({"body": "second"})))
        .unwrap();
    let high_water = store.activate_projection_journal(FINGERPRINT).unwrap();
    let snapshot = store.projection_journal_snapshot(None).unwrap();
    let interior = record_checkpoint(&snapshot.records[0]);
    assert!(store
        .verify_projection_journal_checkpoint(&interior)
        .unwrap());

    store
        .conn
        .execute("DELETE FROM projection_journal_chunks", [])
        .unwrap();

    assert!(store
        .verify_projection_journal_checkpoint(&high_water)
        .unwrap());
    assert!(matches!(
        store.verify_projection_journal_checkpoint(&interior),
        Err(StoreError::InvalidProjectionJournalData(_))
    ));
}

#[test]
fn semantic_epoch_and_projection_journal_commit_or_roll_back_together() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let initial_version = store.canonical_semantic_projection_version().unwrap();
    let mut searchable = event(Uuid::new_v4(), 1, json!({"text": "semantic input"}));
    searchable.event_type = EventType::Message;
    searchable.role = Some(EventRole::User);

    store.begin_immediate_batch().unwrap();
    store.upsert_event(&searchable).unwrap();
    assert!(
        store
            .canonical_semantic_projection_version()
            .unwrap()
            .mutation_epoch
            > initial_version.mutation_epoch
    );
    store.rollback_batch().unwrap();
    assert_eq!(
        store.canonical_semantic_projection_version().unwrap(),
        initial_version
    );
    assert!(store
        .projection_journal_snapshot(None)
        .unwrap()
        .records
        .is_empty());

    store.upsert_event(&searchable).unwrap();
    assert!(
        store
            .canonical_semantic_projection_version()
            .unwrap()
            .mutation_epoch
            > initial_version.mutation_epoch
    );
    assert_eq!(
        store
            .projection_journal_snapshot(None)
            .unwrap()
            .records
            .last()
            .expect("committed event journal record")
            .stable_entity_id,
        searchable.id
    );
}

#[test]
fn incremental_mutations_are_atomic_revisioned_and_noop_stable() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let id = Uuid::new_v4();
    let mut value = event(id, 1, json!({"body": "first"}));

    store.upsert_event(&value).unwrap();
    store.upsert_event(&value).unwrap();
    let snapshot = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(snapshot.records[0].entity_revision, 1);

    value.payload = json!({"body": "second"});
    store.upsert_event(&value).unwrap();
    value.sync.deleted_at = Some(now());
    store.upsert_event(&value).unwrap();
    let snapshot = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(snapshot.records.len(), 3);
    assert_eq!(snapshot.records[1].entity_revision, 2);
    assert_eq!(snapshot.records[2].entity_revision, 3);
    assert_eq!(snapshot.records[2].operation, JournalOperation::Delete);
    assert!(snapshot.records[2].canonical_payload.is_none());

    store.begin_immediate_batch().unwrap();
    let rolled_back = Uuid::new_v4();
    store
        .upsert_event(&event(rolled_back, 2, json!({"body": "rollback"})))
        .unwrap();
    store.rollback_batch().unwrap();
    let snapshot = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(snapshot.frozen_through.position.sequence, 3);
    assert!(store.get_event(rolled_back).is_err());
}

#[test]
fn nested_activation_rollback_restores_cached_activity_before_later_mutations() {
    let (_temp, store) = open_store();
    let before_activation = Uuid::from_u128(81);
    let after_rollback = Uuid::from_u128(82);
    let after_commit = Uuid::from_u128(83);

    store.begin_immediate_batch().unwrap();
    store
        .upsert_event(&event(before_activation, 1, json!({"body": "before"})))
        .unwrap();
    assert_eq!(store.projection_journal_active_in_batch.get(), Some(false));

    store.begin_immediate_batch().unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    store.rollback_batch().unwrap();
    store
        .upsert_event(&event(after_rollback, 2, json!({"body": "restored"})))
        .unwrap();
    assert_eq!(store.projection_journal_active_in_batch.get(), Some(false));

    store.begin_immediate_batch().unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    store.commit_batch().unwrap();
    store
        .upsert_event(&event(after_commit, 3, json!({"body": "active"})))
        .unwrap();
    store.commit_batch().unwrap();

    let snapshot = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(
        snapshot
            .records
            .iter()
            .map(|record| record.stable_entity_id)
            .collect::<Vec<_>>(),
        vec![before_activation, after_rollback, after_commit]
    );
    assert_eq!(
        snapshot.frozen_through.cumulative_digest,
        snapshot.records.last().unwrap().cumulative_digest
    );
}

#[test]
fn nested_deactivation_rollback_restores_cached_activity_before_later_mutations() {
    let (_temp, store) = open_store();
    let before_deactivation = Uuid::from_u128(91);
    let after_rollback = Uuid::from_u128(92);
    let after_commit = Uuid::from_u128(93);
    store.activate_projection_journal(FINGERPRINT).unwrap();

    store.begin_immediate_batch().unwrap();
    store
        .upsert_event(&event(before_deactivation, 1, json!({"body": "before"})))
        .unwrap();

    store.begin_immediate_batch().unwrap();
    store.disable_projection_journal().unwrap();
    store.rollback_batch().unwrap();
    store
        .upsert_event(&event(after_rollback, 2, json!({"body": "restored"})))
        .unwrap();
    assert_eq!(store.projection_journal_active_in_batch.get(), Some(true));
    let snapshot = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(
        snapshot
            .records
            .iter()
            .map(|record| record.stable_entity_id)
            .collect::<Vec<_>>(),
        vec![before_deactivation, after_rollback]
    );
    assert_eq!(
        snapshot.frozen_through.cumulative_digest,
        snapshot.records.last().unwrap().cumulative_digest
    );

    store.begin_immediate_batch().unwrap();
    store.disable_projection_journal().unwrap();
    store.commit_batch().unwrap();
    store
        .upsert_event(&event(after_commit, 3, json!({"body": "inactive"})))
        .unwrap();
    store.commit_batch().unwrap();

    store.activate_projection_journal(FINGERPRINT).unwrap();
    let baseline = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(
        baseline
            .records
            .iter()
            .map(|record| record.stable_entity_id)
            .collect::<Vec<_>>(),
        vec![before_deactivation, after_rollback, after_commit]
    );
}

#[test]
fn atomic_event_and_run_writes_match_standalone_and_roll_back_together() {
    let (_standalone_temp, standalone) = open_store();
    let (_batched_temp, batched) = open_store();
    let run_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let mut execution = run(run_id, Uuid::nil(), Uuid::nil());
    execution.session_id = None;
    execution.source_id = None;
    let mut value = event(
        event_id,
        1,
        json!({"text": "import-batch-persisted-row-oracle"}),
    );
    value.event_type = EventType::Message;
    value.role = Some(EventRole::User);
    value.run_id = Some(run_id);

    standalone.upsert_run(&execution).unwrap();
    standalone.upsert_event(&value).unwrap();

    batched
        .with_atomic_write(|| {
            batched.upsert_run(&execution)?;
            batched.upsert_event(&value)?;
            Ok(())
        })
        .unwrap();

    assert_eq!(
        standalone.get_run(run_id).unwrap(),
        batched.get_run(run_id).unwrap()
    );
    assert_eq!(
        standalone.get_event(event_id).unwrap(),
        batched.get_event(event_id).unwrap()
    );
    assert_eq!(
        standalone
            .search_event_hits("import-batch-persisted-row-oracle", 10)
            .unwrap(),
        batched
            .search_event_hits("import-batch-persisted-row-oracle", 10)
            .unwrap()
    );

    let rolled_back_run_id = Uuid::new_v4();
    let rolled_back_event_id = Uuid::new_v4();
    execution.id = rolled_back_run_id;
    value.id = rolled_back_event_id;
    value.seq = 2;
    value.run_id = Some(rolled_back_run_id);
    value.payload = json!({"text": "import-batch-rollback-oracle"});
    let rollback: crate::Result<()> = batched.with_atomic_write(|| {
        batched.upsert_run(&execution)?;
        batched.upsert_event(&value)?;
        Err(StoreError::Sql(rusqlite::Error::InvalidQuery))
    });
    assert!(rollback.is_err());

    assert!(batched.get_run(rolled_back_run_id).is_err());
    assert!(batched.get_event(rolled_back_event_id).is_err());
    assert!(batched
        .search_event_hits("import-batch-rollback-oracle", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn atomic_write_inactive_journal_cache_observes_later_activation() {
    let (_temp, store) = open_store();
    let before_activation_id = Uuid::new_v4();
    let after_activation_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let mut execution = run(run_id, Uuid::nil(), Uuid::nil());
    execution.session_id = None;
    execution.source_id = None;

    store
        .with_atomic_write(|| {
            store.upsert_event(&event(
                before_activation_id,
                1,
                json!({"body": "before activation"}),
            ))?;
            store.activate_projection_journal(FINGERPRINT)?;
            store.upsert_run(&execution)?;
            let mut after_activation =
                event(after_activation_id, 2, json!({"body": "after activation"}));
            after_activation.run_id = Some(run_id);
            store.upsert_event(&after_activation)?;
            execution.command_preview = Some("updated command".to_owned());
            store.upsert_run(&execution)?;
            Ok(())
        })
        .unwrap();

    let snapshot = store.projection_journal_snapshot(None).unwrap();
    let before_records = snapshot
        .records
        .iter()
        .filter(|record| record.stable_entity_id == before_activation_id)
        .collect::<Vec<_>>();
    let after_records = snapshot
        .records
        .iter()
        .filter(|record| record.stable_entity_id == after_activation_id)
        .collect::<Vec<_>>();
    assert_eq!(
        before_records.len(),
        1,
        "activation baseline must include the prior event"
    );
    assert_eq!(
        after_records.len(),
        2,
        "event insert and run update must both journal"
    );
    assert_eq!(after_records[0].entity_revision, 1);
    assert_eq!(after_records[1].entity_revision, 2);
    assert!(serde_json::to_string(&after_records[1].canonical_payload)
        .unwrap()
        .contains("updated command"));
}

#[test]
fn actor_run_and_source_mutations_revise_only_semantic_payloads() {
    let (_temp, store) = open_store();
    let source_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let mut source = source(source_id);
    let mut session = session(session_id, source_id);
    let mut run = run(run_id, session_id, source_id);
    let mut event = event(event_id, 1, json!({"body": "event"}));
    event.session_id = Some(session_id);
    event.run_id = Some(run_id);
    store.upsert_capture_source(&source).unwrap();
    store.upsert_session(&session).unwrap();
    store.upsert_run(&run).unwrap();
    store.upsert_event(&event).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();

    session.role_hint = Some("reviewer".to_owned());
    store.upsert_session(&session).unwrap();
    run.command_preview = Some("second command".to_owned());
    store.upsert_run(&run).unwrap();
    let before_root_only = store
        .projection_journal_snapshot(None)
        .unwrap()
        .frozen_through
        .position
        .sequence;
    run.cwd = Some("/another/private/repository".to_owned());
    store.upsert_run(&run).unwrap();
    assert_eq!(
        store
            .projection_journal_snapshot(None)
            .unwrap()
            .frozen_through
            .position
            .sequence,
        before_root_only
    );
    source.descriptor.raw_source_path = Some("/provider/moved-session.jsonl".to_owned());
    store.upsert_capture_source(&source).unwrap();

    let snapshot = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(snapshot.records.len(), 4);
    assert_eq!(snapshot.records.last().unwrap().entity_revision, 4);
    let payload = serde_json::to_string(
        snapshot
            .records
            .last()
            .unwrap()
            .canonical_payload
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert!(payload.contains("reviewer"));
    assert!(payload.contains("second command"));
    assert!(payload.contains("/provider/moved-session.jsonl"));
    assert!(!payload.contains("/another/private/repository"));
}

#[test]
fn incremental_final_state_matches_a_fresh_baseline() {
    let id = Uuid::parse_str("018fe2e4-2266-7000-8000-000000000002").unwrap();
    let final_event = event(id, 1, json!({"body": "final"}));

    let (_temp_a, incremental) = open_store();
    incremental
        .activate_projection_journal(FINGERPRINT)
        .unwrap();
    incremental
        .upsert_event(&event(id, 1, json!({"body": "initial"})))
        .unwrap();
    incremental.upsert_event(&final_event).unwrap();
    let incremental_record = incremental
        .projection_journal_snapshot(None)
        .unwrap()
        .records
        .pop()
        .unwrap();

    let (_temp_b, baseline) = open_store();
    baseline.upsert_event(&final_event).unwrap();
    baseline.activate_projection_journal(FINGERPRINT).unwrap();
    let baseline_record = baseline
        .projection_journal_snapshot(None)
        .unwrap()
        .records
        .pop()
        .unwrap();
    assert_eq!(
        incremental_record.payload_sha256,
        baseline_record.payload_sha256
    );
    assert_eq!(
        incremental_record.canonical_payload,
        baseline_record.canonical_payload
    );
    assert_eq!(incremental_record.evidence, baseline_record.evidence);
    assert_eq!(incremental_record.provenance, baseline_record.provenance);
}

#[test]
fn file_and_vcs_records_remain_revisioned_when_linked_output_is_elided() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let event_id = Uuid::new_v4();
    let mut result_event = event(
        event_id,
        1,
        json!({
            "body": {"output_preview": "done"},
            "result_outcome": "success",
            "result_evidence": [{"kind": "call_id", "value": "call-1"}]
        }),
    );
    result_event.event_type = EventType::CommandOutput;
    store.upsert_event(&result_event).unwrap();
    let mut file = FileTouched {
        id: Uuid::new_v4(),
        history_record_id: None,
        run_id: None,
        event_id: Some(event_id),
        vcs_workspace_id: None,
        path: "src/lib.rs".to_owned(),
        change_kind: Some(FileChangeKind::Modified),
        old_path: None,
        line_count_delta: Some(1),
        confidence: Confidence::Explicit,
        timestamps: timestamps(),
        source_id: None,
        sync: SyncMetadata::default(),
    };
    store.upsert_file_touched(&file).unwrap();

    let workspace = VcsWorkspace {
        id: Uuid::new_v4(),
        kind: VcsKind::Git,
        root_path: "/private/repo".to_owned(),
        repo_fingerprint: "repo-fingerprint".to_owned(),
        primary_remote_url_normalized: None,
        host: VcsHost::Local,
        owner: None,
        name: None,
        monorepo_subpath: None,
        timestamps: timestamps(),
        source_id: None,
        sync: SyncMetadata::default(),
    };
    let workspace_id = store.upsert_vcs_workspace(&workspace).unwrap();
    let change = VcsChange {
        id: Uuid::new_v4(),
        vcs_workspace_id: workspace_id,
        kind: VcsChangeKind::GitCommit,
        change_id: "abcdef1".to_owned(),
        parent_change_ids: Vec::new(),
        branch_or_bookmark: Some("main".to_owned()),
        tree_hash: None,
        author_time: Some(now()),
        confidence: Confidence::Explicit,
        timestamps: timestamps(),
        source_id: None,
        sync: SyncMetadata::default(),
    };
    let change_id = store.upsert_vcs_change(&change).unwrap();

    let snapshot = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(
        snapshot
            .records
            .iter()
            .map(|record| record.entity_kind)
            .collect::<Vec<_>>(),
        vec![JournalEntityKind::FileTouch, JournalEntityKind::VcsChange]
    );
    assert_eq!(snapshot.records[0].stable_entity_id, file.id);
    assert_eq!(snapshot.records[1].stable_entity_id, change_id);

    file.sync.deleted_at = Some(now());
    store.upsert_file_touched(&file).unwrap();
    let snapshot = store.projection_journal_snapshot(None).unwrap();
    let tombstone = snapshot.records.last().unwrap();
    assert_eq!(tombstone.entity_kind, JournalEntityKind::FileTouch);
    assert_eq!(tombstone.entity_revision, 2);
    assert_eq!(tombstone.operation, JournalOperation::Delete);
}

#[test]
fn complete_content_and_repository_roots_never_enter_payloads() {
    let (_temp, store) = open_store();
    let source_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    store
        .conn
        .execute(
            "INSERT INTO capture_sources
             (id, kind, provider, machine_id, cwd, raw_source_path, source_format,
              source_root, started_at_ms, fidelity)
             VALUES (?1, 'provider_import', 'codex', 'machine', '/secret/repo',
                     '/provider/session.jsonl', 'codex_session_jsonl_tree',
                     '/secret/provider-root', 1, 'imported')",
            [source_id.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO runs
             (id, run_type, status, started_at_ms, cwd, command_preview,
              created_at_ms, updated_at_ms, source_id, fidelity)
             VALUES (?1, 'command', 'succeeded', 1, '/secret/repo', 'git status',
                     1, 1, ?2, 'imported')",
            [run_id.to_string(), source_id.to_string()],
        )
        .unwrap();
    let mut value = event(Uuid::new_v4(), 1, json!({"body": "safe"}));
    value.run_id = Some(run_id);
    value.capture_source_id = Some(source_id);
    value.sync.metadata = json!({
        "source_record_ordinal": 0,
        "verified_content_locators_v1": {"version": 1, "locators": [{"path": "/secret/body"}]}
    });
    store.upsert_event(&value).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let payload = serde_json::to_string(
        store.projection_journal_snapshot(None).unwrap().records[0]
            .canonical_payload
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert!(!payload.contains("verified_content_locators_v1"));
    assert!(!payload.contains("/secret/repo"));
    assert!(!payload.contains("/secret/provider-root"));
    assert!(payload.contains("/provider/session.jsonl"));
    assert_eq!(
        store
            .projection_journal_snapshot(None)
            .unwrap()
            .authorized_repository_roots,
        vec!["/secret/repo"]
    );
}

#[test]
fn outputs_are_absent_from_journal_baselines_and_deltas() {
    let (_temp, store) = open_store();
    let source_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    store
        .upsert_capture_source(&source(source_id))
        .expect("capture source");
    store
        .upsert_session(&session(session_id, source_id))
        .expect("session");

    let command_id = Uuid::parse_str("ffffffff-ffff-4fff-8fff-ffffffffffff").unwrap();
    let result_id = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
    let mut command = event(command_id, 40, json!({"body": {"call_id": "call-1"}}));
    command.event_type = EventType::ToolCall;
    command.session_id = Some(session_id);
    command.capture_source_id = Some(source_id);
    command.sync.metadata = json!({"source_record_ordinal": 40});
    let mut result = event(result_id, 41, json!({"body": {"call_id": "call-1"}}));
    result.event_type = EventType::ToolOutput;
    result.session_id = Some(session_id);
    result.capture_source_id = Some(source_id);
    result.sync.metadata = json!({"source_record_ordinal": 41});
    store.upsert_event(&result).expect("lexically first result");
    store
        .upsert_event(&command)
        .expect("lexically last command");

    store.activate_projection_journal(FINGERPRINT).unwrap();
    let baseline = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(
        baseline
            .records
            .iter()
            .map(|record| record.stable_entity_id)
            .collect::<Vec<_>>(),
        vec![command_id]
    );

    let mut later_result = event(
        Uuid::new_v4(),
        42,
        json!({
            "body": {
                "call_id": "call-2",
                "result_outcome": "failure",
                "output_preview": "sparse failure diagnostic"
            }
        }),
    );
    later_result.event_type = EventType::CommandOutput;
    later_result.session_id = Some(session_id);
    later_result.capture_source_id = Some(source_id);
    later_result.sync.metadata = json!({"source_record_ordinal": 42});
    store.upsert_event(&later_result).expect("later output");

    let later_call_id = Uuid::new_v4();
    let mut later_call = event(later_call_id, 43, json!({"body": {"call_id": "call-2"}}));
    later_call.session_id = Some(session_id);
    later_call.capture_source_id = Some(source_id);
    later_call.sync.metadata = json!({"source_record_ordinal": 43});
    store.upsert_event(&later_call).expect("later non-output");

    let delta = store
        .projection_journal_snapshot(Some(baseline.next_position))
        .unwrap();
    assert_eq!(delta.records.len(), 1);
    assert_eq!(delta.records[0].stable_entity_id, later_call_id);
    assert_eq!(delta.records[0].entity_kind, JournalEntityKind::Event);
}

#[test]
fn protocol_invalid_optional_identities_are_omitted_and_orphans_are_normalized() {
    let (_temp, store) = open_store();
    let source_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let mut source = source(source_id);
    source.descriptor.external_session_id = Some("x".repeat(4 * 1024 + 1));
    store.upsert_capture_source(&source).unwrap();
    let mut session = session(session_id, source_id);
    session.external_session_id = Some("x".repeat(4 * 1024 + 1));
    store.upsert_session(&session).unwrap();
    let mut value = event(Uuid::new_v4(), 1, json!({"body": "safe"}));
    value.session_id = Some(session_id);
    value.capture_source_id = Some(source_id);
    value.sync.metadata = json!({"source_record_subrecord_index": 7});
    store.upsert_event(&value).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();

    let record = store
        .projection_journal_snapshot(None)
        .unwrap()
        .records
        .remove(0);
    let payload = record.canonical_payload.unwrap();
    assert!(payload["actor"]["external_session_id"].is_null());
    assert!(payload["citation"]["source_record_ordinal"].is_null());
    assert!(payload["citation"]["source_record_subrecord_index"].is_null());
    assert!(record.provenance.provider_external_id.is_none());
    assert!(record
        .evidence
        .iter()
        .all(|identity| identity.source_record_subrecord_index.is_none()));
}

#[test]
fn protocol_invalid_required_uuid_rolls_back_active_journal_write() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let error = store
        .upsert_event(&event(Uuid::nil(), 1, json!({"body": "unsafe"})))
        .unwrap_err();
    assert!(matches!(error, StoreError::InvalidProjectionJournalData(_)));
    assert_eq!(
        store
            .projection_journal_snapshot(None)
            .unwrap()
            .frozen_through
            .position
            .sequence,
        0
    );
}

#[test]
fn authorized_repository_roots_are_sorted_deduplicated_and_count_bounded() {
    let (_temp, store) = open_store();
    for index in (0..=MAX_AUTHORIZED_REPOSITORY_ROOTS).rev() {
        let root = format!("/repositories/{index:03}");
        store
            .register_local_workspace(&root, &format!("fingerprint-{index}"), None)
            .unwrap();
    }
    store
        .register_local_workspace("/repositories/000", "duplicate", None)
        .unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();

    let roots = store
        .projection_journal_snapshot(None)
        .unwrap()
        .authorized_repository_roots;
    assert_eq!(roots.len(), MAX_AUTHORIZED_REPOSITORY_ROOTS);
    assert_eq!(roots.first().map(String::as_str), Some("/repositories/000"));
    assert_eq!(roots.last().map(String::as_str), Some("/repositories/127"));
    assert!(roots.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn bounded_payload_failure_rolls_back_canonical_write() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let id = Uuid::new_v4();
    let oversized = "x".repeat(PROJECTION_JOURNAL_RECORD_MAX_BYTES + 1);
    let error = store
        .upsert_event(&event(id, 1, json!({"body": oversized})))
        .unwrap_err();
    assert!(matches!(
        error,
        StoreError::ProjectionJournalPayloadTooLarge { .. }
    ));
    assert!(store.get_event(id).is_err());
    assert_eq!(
        store
            .projection_journal_snapshot(None)
            .unwrap()
            .frozen_through
            .position
            .sequence,
        0
    );

    store.begin_immediate_batch().unwrap();
    let ignored = Uuid::new_v4();
    assert!(matches!(
        store.upsert_event(&event(
            ignored,
            2,
            json!({"body": "x".repeat(PROJECTION_JOURNAL_RECORD_MAX_BYTES + 1)})
        )),
        Err(StoreError::ProjectionJournalPayloadTooLarge { .. })
    ));
    store.commit_batch().unwrap();
    assert!(store.get_event(ignored).is_err());
}

#[test]
fn archive_import_appends_inside_the_import_transaction() {
    let (temp, mut store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let id = Uuid::new_v4();
    let mut archive = SessionHistoryArchive::default();
    archive
        .events
        .push(event(id, 1, json!({"body": "archive"})));

    store.import_archive(&archive, false).unwrap();
    let snapshot = store.projection_journal_snapshot(None).unwrap();
    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(snapshot.records[0].stable_entity_id, id);

    drop(store);
    let reopened = Store::open(temp.path().join("ctx.db")).unwrap();
    assert_eq!(
        reopened
            .projection_journal_snapshot(None)
            .unwrap()
            .frozen_through
            .position
            .sequence,
        1
    );
}

#[test]
fn disabling_clears_records_and_next_activation_uses_new_generation() {
    let (_temp, store) = open_store();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    store
        .upsert_event(&event(Uuid::new_v4(), 1, json!({"body": "one"})))
        .unwrap();
    store.disable_projection_journal().unwrap();
    assert!(matches!(
        store.projection_journal_snapshot(None),
        Err(StoreError::ProjectionJournalInactive)
    ));
    let checkpoint = store.activate_projection_journal(FINGERPRINT).unwrap();
    assert_eq!(checkpoint.position.generation, 2);
    assert_eq!(checkpoint.position.sequence, 1);
}

#[test]
fn stale_draft_schema_47_is_rejected_by_identity() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("draft.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE draft_only (id INTEGER); PRAGMA user_version = 47;")
        .unwrap();
    drop(conn);
    assert!(matches!(
        Store::open(&path),
        Err(StoreError::UnsupportedSchemaIdentity(_))
    ));
}

#[test]
fn schema_46_upgrade_preserves_canonical_rows_and_installs_only_final_state() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("upgrade.db");
    let id = Uuid::new_v4();
    {
        let store = Store::open(&path).unwrap();
        store
            .upsert_event(&event(id, 1, json!({"body": "preserved"})))
            .unwrap();
        store
            .conn
            .execute_batch(
                "DROP TABLE projection_journal_entities;
                 DROP TABLE projection_journal_chunks;
                 DROP TABLE projection_journal_state;
                 DROP TABLE ctx_store_schema_identity;
                 PRAGMA user_version = 46;",
            )
            .unwrap();
    }
    let upgraded = Store::open(&path).unwrap();
    assert_eq!(
        upgraded.get_event(id).unwrap().payload,
        json!({"body": "preserved"})
    );
    assert_eq!(
        upgraded
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        47
    );
    assert_eq!(
        upgraded
            .conn
            .query_row(
                "SELECT schema_identity FROM ctx_store_schema_identity WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        FINAL_SCHEMA_IDENTITY
    );
    for obsolete in [
        "cloud_publication_state",
        "cloud_publication_cursor_permits",
        "canonical_observation_state",
        "canonical_observation_items",
        "canonical_observation_projection_state",
        "canonical_projection_revisions",
    ] {
        assert_eq!(
            upgraded
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [obsolete],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0,
            "obsolete table {obsolete}"
        );
    }
}
