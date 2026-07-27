use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
    time::Duration,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, EntityTimestamps, Event, EventRole, EventType, FileChangeKind, FileTouched,
    HistoryRecord, HistoryRecordLink, HistoryRecordLinkTargetType, HistoryRecordLinkType, Run,
    RunStatus, RunType, Session, SessionStatus, SyncMetadata, VcsChange, VcsChangeKind, VcsHost,
    VcsKind, VcsWorkspace,
};
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

use super::*;
use crate::ProviderEventHashAuthority;

const FINGERPRINT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn now() -> DateTime<Utc> {
    "2026-07-22T00:00:00Z".parse().unwrap()
}

fn timestamps() -> EntityTimestamps {
    EntityTimestamps {
        created_at: now(),
        updated_at: now(),
    }
}

fn source(id: Uuid, path: &str) -> CaptureSource {
    CaptureSource {
        id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: "machine".to_owned(),
            process_id: None,
            cwd: Some("/private/repository".to_owned()),
            raw_source_path: Some(path.to_owned()),
            source_format: Some("codex_session_jsonl_tree".to_owned()),
            source_root: Some("/private/provider-root".to_owned()),
            source_identity: Some(format!("identity-{id}")),
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
        command_preview: Some("git status".to_owned()),
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(),
        source_id: Some(source_id),
        sync: SyncMetadata::default(),
    }
}

fn event(id: Uuid, seq: u64, body: serde_json::Value) -> Event {
    Event {
        id,
        seq,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::ToolOutput,
        role: Some(EventRole::User),
        occurred_at: now(),
        capture_source_id: None,
        payload: body,
        payload_blob_id: None,
        dedupe_key: None,
        sync: SyncMetadata {
            metadata: json!({"source_record_ordinal": seq}),
            ..SyncMetadata::default()
        },
    }
}

fn file_touch(id: Uuid, source_id: Uuid) -> FileTouched {
    FileTouched {
        id,
        history_record_id: None,
        run_id: None,
        event_id: None,
        vcs_workspace_id: None,
        path: "src/lib.rs".to_owned(),
        change_kind: Some(FileChangeKind::Modified),
        old_path: None,
        line_count_delta: Some(1),
        confidence: Confidence::Explicit,
        timestamps: timestamps(),
        source_id: Some(source_id),
        sync: SyncMetadata::default(),
    }
}

fn vcs_change(store: &Store, id: Uuid, source_id: Uuid) -> VcsChange {
    let workspace = VcsWorkspace {
        id: Uuid::from_u128(id.as_u128().wrapping_add(1)),
        kind: VcsKind::Git,
        root_path: "/private/repo".to_owned(),
        repo_fingerprint: format!("repo-{id}"),
        primary_remote_url_normalized: None,
        host: VcsHost::Local,
        owner: None,
        name: None,
        monorepo_subpath: None,
        timestamps: timestamps(),
        source_id: Some(source_id),
        sync: SyncMetadata::default(),
    };
    let workspace_id = store.upsert_vcs_workspace(&workspace).unwrap();
    VcsChange {
        id,
        vcs_workspace_id: workspace_id,
        kind: VcsChangeKind::GitCommit,
        change_id: format!("change-{id}"),
        parent_change_ids: Vec::new(),
        branch_or_bookmark: Some("main".to_owned()),
        tree_hash: None,
        author_time: Some(now()),
        confidence: Confidence::Explicit,
        timestamps: timestamps(),
        source_id: Some(source_id),
        sync: SyncMetadata::default(),
    }
}

fn history_record(id: Uuid, title: &str) -> HistoryRecord {
    HistoryRecord {
        id,
        title: title.to_owned(),
        body: String::new(),
        tags: Vec::new(),
        kind: "session".to_owned(),
        workspace: None,
        created_at: now(),
        updated_at: now(),
    }
}

fn vcs_link(id: Uuid, record_id: Uuid, change_id: Uuid) -> HistoryRecordLink {
    HistoryRecordLink {
        id,
        history_record_id: record_id,
        target_type: HistoryRecordLinkTargetType::VcsChange,
        target_id: change_id,
        link_type: HistoryRecordLinkType::Produced,
        confidence: Confidence::Explicit,
        source_id: None,
        timestamps: timestamps(),
        sync: SyncMetadata::default(),
    }
}

fn all_records(store: &Store) -> Vec<ProjectionJournalRecord> {
    let mut records = Vec::new();
    let mut after = None;
    loop {
        let page = store.projection_journal_snapshot(after).unwrap();
        records.extend(page.records);
        if !page.has_more {
            return records;
        }
        after = Some(page.next_position);
    }
}

type ReplayedState = BTreeMap<(JournalEntityKind, Uuid), (String, String, String)>;

fn replayed_state(records: &[ProjectionJournalRecord]) -> ReplayedState {
    let mut state = BTreeMap::new();
    for record in records {
        let key = (record.entity_kind, record.stable_entity_id);
        match record.operation {
            JournalOperation::Upsert => {
                state.insert(
                    key,
                    (
                        serde_json::to_string(record.canonical_payload.as_ref().unwrap()).unwrap(),
                        serde_json::to_string(&record.evidence).unwrap(),
                        serde_json::to_string(&record.provenance).unwrap(),
                    ),
                );
            }
            JournalOperation::Delete => {
                state.remove(&key);
            }
        }
    }
    state
}

#[test]
fn moving_session_to_another_source_revises_old_source_sidecars() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let old_source_id = Uuid::new_v4();
    let new_source_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let file_id = Uuid::new_v4();
    let change_id = Uuid::new_v4();

    store
        .upsert_capture_source(&source(old_source_id, "/old/session.jsonl"))
        .unwrap();
    store
        .upsert_capture_source(&source(new_source_id, "/new/session.jsonl"))
        .unwrap();
    let mut actor = session(session_id, old_source_id);
    store.upsert_session(&actor).unwrap();
    store
        .upsert_file_touched(&file_touch(file_id, old_source_id))
        .unwrap();
    store
        .upsert_vcs_change(&vcs_change(&store, change_id, old_source_id))
        .unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();

    actor.capture_source_id = Some(new_source_id);
    store.upsert_session(&actor).unwrap();

    let snapshot = store.projection_journal_snapshot(None).unwrap();
    for id in [file_id, change_id] {
        let records = snapshot
            .records
            .iter()
            .filter(|record| record.stable_entity_id == id)
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2, "old-source sidecar {id} was not revised");
        assert_eq!(records[1].entity_revision, 2);
        assert!(records[0].canonical_payload.as_ref().unwrap()["actor"].is_object());
        assert!(records[1].canonical_payload.as_ref().unwrap()["actor"].is_null());
    }
}

#[test]
fn archive_overwrite_revises_old_session_dependency_relations() {
    let temp = tempdir().unwrap();
    let mut store = Store::open(temp.path().join("ctx.db")).unwrap();
    let old_source_id = Uuid::new_v4();
    let new_source_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let file_id = Uuid::new_v4();
    let change_id = Uuid::new_v4();

    store
        .upsert_capture_source(&source(old_source_id, "/old/session.jsonl"))
        .unwrap();
    store
        .upsert_capture_source(&source(new_source_id, "/new/session.jsonl"))
        .unwrap();
    store
        .upsert_session(&session(session_id, old_source_id))
        .unwrap();
    store
        .upsert_file_touched(&file_touch(file_id, old_source_id))
        .unwrap();
    store
        .upsert_vcs_change(&vcs_change(&store, change_id, old_source_id))
        .unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();

    let mut archive = ctx_history_core::SessionHistoryArchive::default();
    let mut moved = session(session_id, old_source_id);
    moved.capture_source_id = Some(new_source_id);
    archive.sessions.push(moved);
    store.import_archive(&archive, true).unwrap();

    let snapshot = store.projection_journal_snapshot(None).unwrap();
    for id in [file_id, change_id] {
        let records = snapshot
            .records
            .iter()
            .filter(|record| record.stable_entity_id == id)
            .collect::<Vec<_>>();
        assert_eq!(
            records.len(),
            2,
            "archive overwrite lost the old dependency set for {id}"
        );
        assert!(records[1].canonical_payload.as_ref().unwrap()["actor"].is_null());
    }
}

#[test]
fn archive_event_identity_collision_replays_the_canonical_id() {
    let incremental_temp = tempdir().unwrap();
    let fresh_temp = tempdir().unwrap();
    let mut incremental = Store::open(incremental_temp.path().join("ctx.db")).unwrap();
    let mut fresh = Store::open(fresh_temp.path().join("ctx.db")).unwrap();
    let source_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let canonical_event_id = Uuid::new_v4();
    let incoming_event_id = Uuid::new_v4();
    let dedupe_key = "archive-event-canonical-identity";

    let initial_source = source(source_id, "/old/session.jsonl");
    let initial_session = session(session_id, source_id);
    let mut initial_event = event(canonical_event_id, 1, json!({"body": "initial"}));
    initial_event.dedupe_key = Some(dedupe_key.to_owned());
    for store in [&incremental, &fresh] {
        store.upsert_capture_source(&initial_source).unwrap();
        store.upsert_session(&initial_session).unwrap();
        assert_eq!(
            store.upsert_event(&initial_event).unwrap(),
            canonical_event_id
        );
    }
    incremental
        .activate_projection_journal(FINGERPRINT)
        .unwrap();

    let mut final_source = initial_source;
    final_source.descriptor.raw_source_path = Some("/moved/session.jsonl".to_owned());
    let mut final_session = initial_session;
    final_session.role_hint = Some("reviewer".to_owned());
    let mut incoming_event = event(incoming_event_id, 1, json!({"body": "final"}));
    incoming_event.dedupe_key = Some(dedupe_key.to_owned());
    let archive = ctx_history_core::SessionHistoryArchive {
        capture_sources: vec![final_source],
        sessions: vec![final_session],
        events: vec![incoming_event],
        ..ctx_history_core::SessionHistoryArchive::default()
    };
    incremental.import_archive(&archive, true).unwrap();
    fresh.import_archive(&archive, true).unwrap();
    fresh.activate_projection_journal(FINGERPRINT).unwrap();

    assert_eq!(
        incremental.get_event(canonical_event_id).unwrap().payload,
        json!({})
    );
    assert!(incremental.get_event(incoming_event_id).is_err());
    assert_eq!(
        incremental
            .get_capture_source(source_id)
            .unwrap()
            .descriptor
            .raw_source_path
            .as_deref(),
        Some("/moved/session.jsonl")
    );
    assert_eq!(
        incremental
            .get_session(session_id)
            .unwrap()
            .role_hint
            .as_deref(),
        Some("reviewer")
    );

    let incremental_records = all_records(&incremental);
    let canonical_revisions = incremental_records
        .iter()
        .filter(|record| record.stable_entity_id == canonical_event_id)
        .map(|record| record.entity_revision)
        .collect::<Vec<_>>();
    assert_eq!(canonical_revisions, vec![1]);
    assert!(incremental_records
        .iter()
        .all(|record| record.stable_entity_id != incoming_event_id));
    assert_eq!(
        replayed_state(&incremental_records),
        replayed_state(&all_records(&fresh))
    );
    incremental.import_archive(&archive, true).unwrap();
    assert_eq!(all_records(&incremental), incremental_records);
}

#[test]
fn archive_vcs_identity_collision_replays_the_canonical_id() {
    let incremental_temp = tempdir().unwrap();
    let fresh_temp = tempdir().unwrap();
    let mut incremental = Store::open(incremental_temp.path().join("ctx.db")).unwrap();
    let mut fresh = Store::open(fresh_temp.path().join("ctx.db")).unwrap();
    let source_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let canonical_change_id = Uuid::new_v4();
    let incoming_change_id = Uuid::new_v4();

    let initial_source = source(source_id, "/old/vcs-session.jsonl");
    let initial_session = session(session_id, source_id);
    for store in [&incremental, &fresh] {
        store.upsert_capture_source(&initial_source).unwrap();
        store.upsert_session(&initial_session).unwrap();
    }
    let mut initial_change = vcs_change(&incremental, canonical_change_id, source_id);
    initial_change.source_id = None;
    fresh
        .upsert_vcs_workspace(
            &incremental
                .list_vcs_workspaces()
                .unwrap()
                .into_iter()
                .next()
                .unwrap(),
        )
        .unwrap();
    for store in [&incremental, &fresh] {
        assert_eq!(
            store.upsert_vcs_change(&initial_change).unwrap(),
            canonical_change_id
        );
    }
    incremental
        .activate_projection_journal(FINGERPRINT)
        .unwrap();

    let mut final_source = initial_source;
    final_source.descriptor.raw_source_path = Some("/moved/vcs-session.jsonl".to_owned());
    let mut final_session = initial_session;
    final_session.external_agent_id = Some("agent-2".to_owned());
    let mut incoming_change = initial_change.clone();
    incoming_change.id = incoming_change_id;
    incoming_change.branch_or_bookmark = Some("release".to_owned());
    let archive = ctx_history_core::SessionHistoryArchive {
        capture_sources: vec![final_source],
        sessions: vec![final_session],
        vcs_changes: vec![incoming_change],
        ..ctx_history_core::SessionHistoryArchive::default()
    };
    incremental.import_archive(&archive, true).unwrap();
    fresh.import_archive(&archive, true).unwrap();
    fresh.activate_projection_journal(FINGERPRINT).unwrap();

    let canonical_change = incremental
        .list_vcs_changes()
        .unwrap()
        .into_iter()
        .find(|change| change.id == canonical_change_id)
        .unwrap();
    assert_eq!(
        canonical_change.branch_or_bookmark.as_deref(),
        Some("release")
    );
    assert!(incremental
        .list_vcs_changes()
        .unwrap()
        .into_iter()
        .all(|change| change.id != incoming_change_id));
    assert_eq!(
        incremental
            .get_capture_source(source_id)
            .unwrap()
            .descriptor
            .raw_source_path
            .as_deref(),
        Some("/moved/vcs-session.jsonl")
    );
    assert_eq!(
        incremental
            .get_session(session_id)
            .unwrap()
            .external_agent_id
            .as_deref(),
        Some("agent-2")
    );

    let incremental_records = all_records(&incremental);
    let canonical_revisions = incremental_records
        .iter()
        .filter(|record| record.stable_entity_id == canonical_change_id)
        .map(|record| record.entity_revision)
        .collect::<Vec<_>>();
    assert_eq!(canonical_revisions, vec![1, 2]);
    assert!(incremental_records
        .iter()
        .all(|record| record.stable_entity_id != incoming_change_id));
    assert_eq!(
        replayed_state(&incremental_records),
        replayed_state(&all_records(&fresh))
    );
    incremental.import_archive(&archive, true).unwrap();
    assert_eq!(all_records(&incremental), incremental_records);
}

#[test]
fn direct_entity_apis_append_rewrite_tombstone_and_undelete_without_churn() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();

    let event_id = Uuid::new_v4();
    let file_id = Uuid::new_v4();
    let change_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let mut event = event(event_id, 1, json!({"body": "first"}));
    let mut file = file_touch(file_id, source_id);

    store
        .upsert_capture_source(&source(source_id, "/source/session.jsonl"))
        .unwrap();
    let mut change = vcs_change(&store, change_id, source_id);
    assert_eq!(store.upsert_event(&event).unwrap(), event_id);
    store.upsert_file_touched(&file).unwrap();
    assert_eq!(store.upsert_vcs_change(&change).unwrap(), change_id);
    let initial = all_records(&store);
    assert_eq!(initial.len(), 3);

    store.upsert_event(&event).unwrap();
    store.upsert_file_touched(&file).unwrap();
    store.upsert_vcs_change(&change).unwrap();
    assert_eq!(all_records(&store), initial, "exact replays must be no-ops");

    event.payload = json!({"body": "rewritten"});
    file.path = "src/moved.rs".to_owned();
    file.change_kind = Some(FileChangeKind::Renamed);
    file.old_path = Some("src/lib.rs".to_owned());
    change.branch_or_bookmark = Some("release".to_owned());
    store.upsert_event(&event).unwrap();
    store.upsert_file_touched(&file).unwrap();
    store.upsert_vcs_change(&change).unwrap();

    event.sync.deleted_at = Some(now());
    file.sync.deleted_at = Some(now());
    change.sync.deleted_at = Some(now());
    store.upsert_event(&event).unwrap();
    store.upsert_file_touched(&file).unwrap();
    store.upsert_vcs_change(&change).unwrap();

    event.sync.deleted_at = None;
    file.sync.deleted_at = None;
    change.sync.deleted_at = None;
    store.upsert_event(&event).unwrap();
    store.upsert_file_touched(&file).unwrap();
    store.upsert_vcs_change(&change).unwrap();

    let records = all_records(&store);
    assert_eq!(records.len(), 11);
    assert_eq!(
        records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        (1..=11).collect::<Vec<_>>()
    );
    for id in [file_id, change_id] {
        let revisions = records
            .iter()
            .filter(|record| record.stable_entity_id == id)
            .map(|record| (record.entity_revision, record.operation))
            .collect::<Vec<_>>();
        assert_eq!(
            revisions,
            vec![
                (1, JournalOperation::Upsert),
                (2, JournalOperation::Upsert),
                (3, JournalOperation::Delete),
                (4, JournalOperation::Upsert),
            ]
        );
    }
    assert_eq!(
        records
            .iter()
            .filter(|record| record.stable_entity_id == event_id)
            .map(|record| (record.entity_revision, record.operation))
            .collect::<Vec<_>>(),
        vec![
            (1, JournalOperation::Upsert),
            (2, JournalOperation::Delete),
            (3, JournalOperation::Upsert),
        ]
    );
}

#[test]
fn insert_if_absent_and_provider_reconcile_write_only_semantic_changes() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();

    let id = Uuid::new_v4();
    let mut first = event(id, 1, json!({"body": {"text": "first"}}));
    first.dedupe_key = Some(Store::provider_event_dedupe_key(
        CaptureProvider::Codex,
        "session",
        1,
        "oldhash",
    ));
    first.sync.metadata["provider_event_hash_authority"] =
        json!(ProviderEventHashAuthority::NormalizedPayloadFallback.as_str());
    assert!(store.insert_event_if_absent(&first).unwrap());
    assert!(!store.insert_event_if_absent(&first).unwrap());
    assert_eq!(all_records(&store).len(), 1);

    let mut replay = first.clone();
    replay.payload = json!({"body": {"text": "ignored replay payload"}});
    assert!(!store
        .reconcile_provider_event(
            &replay,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap());
    assert_eq!(all_records(&store).len(), 1);

    let mut rewrite = first.clone();
    rewrite.id = Uuid::new_v4();
    rewrite.payload = json!({"body": {"text": "normalized rewrite"}});
    rewrite.dedupe_key = Some(Store::provider_event_dedupe_key(
        CaptureProvider::Codex,
        "session",
        1,
        "newhash",
    ));
    assert!(!store
        .reconcile_provider_event(
            &rewrite,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )
        .unwrap());
    let records = all_records(&store);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].stable_entity_id, id);
    assert_eq!(records[0].entity_revision, 1);
    assert_eq!(
        records[0].canonical_payload.as_ref().unwrap()["payload"],
        json!({})
    );
}

#[test]
fn event_move_and_source_relocation_revise_every_dependent_observation() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let source_id = Uuid::new_v4();
    let first_session_id = Uuid::new_v4();
    let second_session_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let file_id = Uuid::new_v4();
    let change_id = Uuid::new_v4();
    let mut source = source(source_id, "/old/session.jsonl");
    let first_session = session(first_session_id, source_id);
    let mut second_session = session(second_session_id, source_id);
    second_session.external_session_id = Some("external-session-2".to_owned());
    second_session.external_agent_id = Some("agent-2".to_owned());
    let first_run = run(run_id, first_session_id, source_id);
    let mut event = event(event_id, 1, json!({"body": "move"}));
    event.session_id = Some(first_session_id);
    event.run_id = Some(run_id);
    event.capture_source_id = Some(source_id);
    let mut file = file_touch(file_id, source_id);
    file.event_id = Some(event_id);

    store.upsert_capture_source(&source).unwrap();
    store.upsert_session(&first_session).unwrap();
    store.upsert_session(&second_session).unwrap();
    store.upsert_run(&first_run).unwrap();
    store.upsert_event(&event).unwrap();
    store.upsert_file_touched(&file).unwrap();
    store
        .upsert_vcs_change(&vcs_change(&store, change_id, source_id))
        .unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();

    event.session_id = Some(second_session_id);
    event.run_id = None;
    store.upsert_event(&event).unwrap();
    let after_move = all_records(&store);
    assert_eq!(
        after_move
            .iter()
            .filter(|record| record.stable_entity_id == event_id)
            .count(),
        2
    );
    assert_eq!(
        after_move
            .iter()
            .filter(|record| record.stable_entity_id == file_id)
            .count(),
        2,
        "event association moves must revise linked file actor evidence"
    );

    source.descriptor.raw_source_path = Some("/moved/session.jsonl".to_owned());
    store.upsert_capture_source(&source).unwrap();
    let records = all_records(&store);
    for (id, expected_revision) in [(event_id, 3), (file_id, 3), (change_id, 2)] {
        let latest = records
            .iter()
            .rev()
            .find(|record| record.stable_entity_id == id)
            .unwrap();
        assert_eq!(
            latest.entity_revision, expected_revision,
            "source relocation missed {id}"
        );
        let encoded = serde_json::to_string(latest.canonical_payload.as_ref().unwrap()).unwrap();
        assert!(encoded.contains("/moved/session.jsonl"));
        assert!(!encoded.contains("/private/provider-root"));
        assert!(!encoded.contains("/private/repository"));
    }
}

#[test]
fn session_record_assignment_revises_vcs_actor_from_the_old_record() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let source_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let old_record_id = Uuid::new_v4();
    let new_record_id = Uuid::new_v4();
    let change_id = Uuid::new_v4();

    store
        .insert_record(&history_record(old_record_id, "old"))
        .unwrap();
    store
        .insert_record(&history_record(new_record_id, "new"))
        .unwrap();
    store
        .upsert_capture_source(&source(source_id, "/source/session.jsonl"))
        .unwrap();
    let mut actor = session(session_id, source_id);
    actor.history_record_id = Some(old_record_id);
    store.upsert_session(&actor).unwrap();
    let mut change = vcs_change(&store, change_id, source_id);
    change.source_id = None;
    store.upsert_vcs_change(&change).unwrap();
    store
        .upsert_history_record_link(&vcs_link(Uuid::new_v4(), old_record_id, change_id))
        .unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();

    store
        .assign_session_to_record(session_id, new_record_id)
        .unwrap();
    let records = all_records(&store)
        .into_iter()
        .filter(|record| record.stable_entity_id == change_id)
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert!(records[0].canonical_payload.as_ref().unwrap()["actor"].is_object());
    assert!(records[1].canonical_payload.as_ref().unwrap()["actor"].is_null());
    assert_eq!(records[1].entity_revision, 2);
}

#[test]
fn actor_and_run_deletion_revise_event_and_file_payloads() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let source_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let file_id = Uuid::new_v4();
    let mut actor = session(session_id, source_id);
    let mut execution = run(run_id, session_id, source_id);
    let mut event = event(event_id, 1, json!({"body": "result"}));
    event.run_id = Some(run_id);
    let mut file = file_touch(file_id, source_id);
    file.run_id = Some(run_id);

    store
        .upsert_capture_source(&source(source_id, "/source/session.jsonl"))
        .unwrap();
    store.upsert_session(&actor).unwrap();
    store.upsert_run(&execution).unwrap();
    store.upsert_event(&event).unwrap();
    store.upsert_file_touched(&file).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();

    execution.sync.deleted_at = Some(now());
    store.upsert_run(&execution).unwrap();
    actor.sync.deleted_at = Some(now());
    store.upsert_session(&actor).unwrap();

    let records = all_records(&store);
    let event_records = records
        .iter()
        .filter(|record| record.stable_entity_id == event_id)
        .collect::<Vec<_>>();
    assert_eq!(event_records.len(), 2);
    assert!(event_records[0].canonical_payload.as_ref().unwrap()["run"].is_object());
    assert!(event_records[1].canonical_payload.as_ref().unwrap()["run"].is_null());
    assert!(event_records[1].canonical_payload.as_ref().unwrap()["actor"].is_null());

    let file_records = records
        .iter()
        .filter(|record| record.stable_entity_id == file_id)
        .collect::<Vec<_>>();
    assert_eq!(file_records.len(), 2);
    assert!(file_records[0].canonical_payload.as_ref().unwrap()["actor"].is_object());
    assert!(file_records[1].canonical_payload.as_ref().unwrap()["actor"].is_null());
}

#[test]
fn result_evidence_rewrites_but_complete_content_only_changes_do_not() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let id = Uuid::new_v4();
    let mut value = event(
        id,
        1,
        json!({
            "body": {"output_preview": "safe"},
            "result_outcome": "success",
            "result_evidence": [{"kind": "call_id", "value": "call-1"}]
        }),
    );
    value.sync.metadata["complete_content_locator_v1"] =
        json!({"family": "jsonl_range", "path": "/secret/first"});
    value.sync.metadata["complete_content_body_sha256"] = json!("a".repeat(64));
    store.upsert_event(&value).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();

    value.sync.metadata["complete_content_locator_v1"] =
        json!({"family": "jsonl_range", "path": "/secret/second"});
    value.sync.metadata["complete_content_body_sha256"] = json!("b".repeat(64));
    store.upsert_event(&value).unwrap();
    assert_eq!(all_records(&store).len(), 1);

    value.payload["result_outcome"] = json!("failure");
    value.payload["result_evidence"] = json!([{"kind": "git_oid", "value": "c".repeat(40)}]);
    store.upsert_event(&value).unwrap();
    let records = all_records(&store);
    assert_eq!(records.len(), 2);
    let payload = records[1].canonical_payload.as_ref().unwrap();
    assert_eq!(payload["result"]["outcome"], "failure");
    assert_eq!(payload["result"]["identifiers"][0]["kind"], "git_oid");
    assert_eq!(payload["payload"], json!({}));
    assert!(payload["payload"].get("result_outcome").is_none());
    assert!(payload["payload"].get("result_evidence").is_none());
    let encoded = serde_json::to_string(payload).unwrap();
    assert!(!encoded.contains("complete_content"));
    assert!(!encoded.contains("output_preview"));
    assert!(!encoded.contains("/secret/"));
}

#[test]
fn outer_transaction_rollback_restores_canonical_rows_and_exact_journal_prefix() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let event_id = Uuid::new_v4();
    let mut value = event(event_id, 1, json!({"body": "committed"}));
    value.event_type = EventType::Message;
    store.upsert_event(&value).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let before = all_records(&store);

    store.begin_immediate_batch().unwrap();
    value.payload = json!({"body": "rolled back"});
    store.upsert_event(&value).unwrap();
    let file_id = Uuid::new_v4();
    let mut file = file_touch(file_id, Uuid::new_v4());
    file.source_id = None;
    store.upsert_file_touched(&file).unwrap();
    store.rollback_batch().unwrap();

    assert_eq!(all_records(&store), before);
    assert_eq!(
        store.get_event(event_id).unwrap().payload,
        json!({"body": "committed"})
    );
    assert!(!store.file_touched_exists(file_id).unwrap());
}

#[test]
fn busy_writer_cannot_publish_canonical_or_journal_half_transactions() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("ctx.db");
    let owner = Store::open_with_busy_timeout(&path, Duration::ZERO).unwrap();
    owner.activate_projection_journal(FINGERPRINT).unwrap();
    let contender = Store::open_with_busy_timeout(&path, Duration::ZERO).unwrap();
    let held_id = Uuid::new_v4();
    let contender_id = Uuid::new_v4();

    owner.begin_immediate_batch().unwrap();
    owner
        .upsert_event(&event(held_id, 1, json!({"body": "held"})))
        .unwrap();
    assert!(matches!(
        contender.upsert_event(&event(contender_id, 2, json!({"body": "busy"}))),
        Err(StoreError::Sql(_))
    ));
    owner.rollback_batch().unwrap();

    assert!(owner.get_event(held_id).is_err());
    assert!(owner.get_event(contender_id).is_err());
    assert!(all_records(&owner).is_empty());
    contender
        .upsert_event(&event(contender_id, 2, json!({"body": "committed"})))
        .unwrap();
    let records = all_records(&contender);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].sequence, 1);
}

#[test]
fn wal_checkpoint_contention_preserves_committed_journal_and_recovers() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("ctx.db");
    let store = Store::open_with_busy_timeout(&path, Duration::ZERO).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    store
        .upsert_event(&event(Uuid::new_v4(), 1, json!({"body": "before-reader"})))
        .unwrap();

    let reader = rusqlite::Connection::open(&path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    store
        .upsert_event(&event(Uuid::new_v4(), 2, json!({"body": "in-wal"})))
        .unwrap();
    let before_checkpoint = all_records(&store);
    assert_eq!(before_checkpoint.len(), 2);
    assert!(matches!(
        store.checkpoint_wal_truncate_required(),
        Err(StoreError::WalCheckpointBusy { .. })
    ));
    assert_eq!(all_records(&store), before_checkpoint);

    reader.execute_batch("ROLLBACK").unwrap();
    store.checkpoint_wal_truncate_required().unwrap();
    drop(store);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(all_records(&reopened), before_checkpoint);
}

#[test]
fn sqlite_full_rolls_back_canonical_and_journal_writes_together() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    store.checkpoint_wal_truncate_required().unwrap();
    let page_count = store
        .conn
        .query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0))
        .unwrap();
    store
        .conn
        .pragma_update(None, "max_page_count", page_count)
        .unwrap();

    let id = Uuid::new_v4();
    let mut oversized = event(id, 1, json!({"body": "x".repeat(512 * 1024)}));
    oversized.event_type = EventType::Message;
    let error = store.upsert_event(&oversized).unwrap_err();
    assert!(matches!(error, StoreError::Sql(_)), "{error:?}");
    assert!(store.get_event(id).is_err());
    assert!(all_records(&store).is_empty());
}

#[test]
fn concurrent_identical_writers_serialize_to_one_immutable_record() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("ctx.db");
    Store::open(&path)
        .unwrap()
        .activate_projection_journal(FINGERPRINT)
        .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let id = Uuid::new_v4();
    let mut handles = Vec::new();
    for _ in 0..2 {
        let path = path.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let store = Store::open_with_busy_timeout(path, Duration::from_secs(5)).unwrap();
            barrier.wait();
            store
                .upsert_event(&event(id, 1, json!({"body": "same"})))
                .unwrap();
        }));
    }
    barrier.wait();
    for handle in handles {
        handle.join().unwrap();
    }

    let store = Store::open(&path).unwrap();
    let records = all_records(&store);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].stable_entity_id, id);
    assert_eq!(records[0].sequence, 1);
    assert_eq!(records[0].entity_revision, 1);
}

#[test]
fn baseline_plus_mixed_deltas_equals_a_fresh_full_baseline() {
    let incremental_temp = tempdir().unwrap();
    let fresh_temp = tempdir().unwrap();
    let incremental = Store::open(incremental_temp.path().join("ctx.db")).unwrap();
    let fresh = Store::open(fresh_temp.path().join("ctx.db")).unwrap();
    let source_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let deleted_event_id = Uuid::new_v4();
    let file_id = Uuid::new_v4();
    let change_id = Uuid::new_v4();

    let mut initial_source = source(source_id, "/initial/session.jsonl");
    let mut initial_session = session(session_id, source_id);
    let mut initial_run = run(run_id, session_id, source_id);
    let mut initial_event = event(
        event_id,
        1,
        json!({
            "body": "initial",
            "result_outcome": "unknown",
            "result_evidence": []
        }),
    );
    initial_event.session_id = Some(session_id);
    initial_event.run_id = Some(run_id);
    initial_event.capture_source_id = Some(source_id);
    let mut deleted_event = event(deleted_event_id, 2, json!({"body": "obsolete"}));
    deleted_event.session_id = Some(session_id);
    let mut initial_file = file_touch(file_id, source_id);
    initial_file.event_id = Some(event_id);
    initial_file.run_id = Some(run_id);

    incremental.upsert_capture_source(&initial_source).unwrap();
    let mut initial_change = vcs_change(&incremental, change_id, source_id);
    incremental.upsert_session(&initial_session).unwrap();
    incremental.upsert_run(&initial_run).unwrap();
    incremental.upsert_event(&initial_event).unwrap();
    incremental.upsert_event(&deleted_event).unwrap();
    incremental.upsert_file_touched(&initial_file).unwrap();
    incremental.upsert_vcs_change(&initial_change).unwrap();
    incremental
        .activate_projection_journal(FINGERPRINT)
        .unwrap();

    initial_source.descriptor.raw_source_path = Some("/final/session.jsonl".to_owned());
    initial_session.role_hint = Some("reviewer".to_owned());
    initial_run.command_preview = Some("cargo test".to_owned());
    initial_event.payload = json!({
        "body": "final",
        "result_outcome": "success",
        "result_evidence": [{"kind": "call_id", "value": "call-final"}]
    });
    initial_file.path = "src/final.rs".to_owned();
    initial_file.change_kind = Some(FileChangeKind::Renamed);
    initial_file.old_path = Some("src/lib.rs".to_owned());
    initial_change.branch_or_bookmark = Some("final".to_owned());
    deleted_event.sync.deleted_at = Some(now());
    incremental.upsert_capture_source(&initial_source).unwrap();
    incremental.upsert_session(&initial_session).unwrap();
    incremental.upsert_run(&initial_run).unwrap();
    incremental.upsert_event(&initial_event).unwrap();
    incremental.upsert_file_touched(&initial_file).unwrap();
    incremental.upsert_vcs_change(&initial_change).unwrap();
    incremental.upsert_event(&deleted_event).unwrap();

    fresh.upsert_capture_source(&initial_source).unwrap();
    fresh.upsert_session(&initial_session).unwrap();
    fresh.upsert_run(&initial_run).unwrap();
    fresh.upsert_event(&initial_event).unwrap();
    fresh.upsert_file_touched(&initial_file).unwrap();
    let fresh_change = vcs_change(&fresh, change_id, source_id);
    assert_eq!(
        fresh_change.vcs_workspace_id,
        initial_change.vcs_workspace_id
    );
    let mut fresh_change = fresh_change;
    fresh_change.branch_or_bookmark = Some("final".to_owned());
    fresh.upsert_vcs_change(&fresh_change).unwrap();
    fresh.activate_projection_journal(FINGERPRINT).unwrap();

    assert_eq!(
        replayed_state(&all_records(&incremental)),
        replayed_state(&all_records(&fresh))
    );
}

#[test]
fn capture_source_archive_import_uses_the_same_transactional_journal_path() {
    let temp = tempdir().unwrap();
    let mut store = Store::open(temp.path().join("ctx.db")).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let source_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let mut archive = ctx_history_core::SessionHistoryArchive::default();
    archive
        .events
        .push(event(event_id, 1, json!({"body": "archive-source"})));
    let descriptor = source(source_id, "/archive/session.jsonl").descriptor;

    store
        .import_archive_from_capture_source(
            &archive,
            source_id,
            &descriptor,
            now(),
            ctx_history_core::Fidelity::Imported,
            true,
        )
        .unwrap();
    let records = all_records(&store);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].stable_entity_id, event_id);
    assert!(store.get_capture_source(source_id).is_ok());
}

#[test]
fn historical_records_remain_byte_stable_across_noops_and_later_appends() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    let first = event(Uuid::new_v4(), 1, json!({"body": "first"}));
    store.upsert_event(&first).unwrap();
    let prefix = all_records(&store);
    store.upsert_event(&first).unwrap();
    store
        .upsert_event(&event(Uuid::new_v4(), 2, json!({"body": "second"})))
        .unwrap();

    let records = all_records(&store);
    assert_eq!(&records[..prefix.len()], prefix.as_slice());
    assert_eq!(records[0].sequence, 1);
    assert_eq!(records[1].sequence, 2);
    assert_ne!(records[0].cumulative_digest, records[1].cumulative_digest);
    assert_ne!(records[0].cumulative_digest, ZERO_DIGEST);
    assert_ne!(records[1].cumulative_digest, ZERO_DIGEST);
}

#[test]
fn sequence_and_digest_corruption_fail_closed() {
    let sequence_temp = tempdir().unwrap();
    let sequence_store = Store::open(sequence_temp.path().join("ctx.db")).unwrap();
    let id = Uuid::new_v4();
    let mut value = event(id, 1, json!({"body": "one"}));
    sequence_store.upsert_event(&value).unwrap();
    sequence_store
        .activate_projection_journal(FINGERPRINT)
        .unwrap();
    value.payload = json!({"body": "two"});
    sequence_store.upsert_event(&value).unwrap();
    sequence_store
        .conn
        .execute(
            "DELETE FROM projection_journal_chunks WHERE generation = 1 AND first_sequence = 1",
            [],
        )
        .unwrap();
    assert!(matches!(
        sequence_store.projection_journal_snapshot(None),
        Err(StoreError::InvalidProjectionJournalData(_))
    ));

    let digest_temp = tempdir().unwrap();
    let digest_store = Store::open(digest_temp.path().join("ctx.db")).unwrap();
    digest_store
        .upsert_event(&event(Uuid::new_v4(), 1, json!({"body": "digest"})))
        .unwrap();
    digest_store
        .activate_projection_journal(FINGERPRINT)
        .unwrap();
    let encoded = digest_store
        .conn
        .query_row(
            "SELECT records_zstd FROM projection_journal_chunks
             WHERE generation = 1 AND first_sequence = 1",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .unwrap();
    let mut records = decode_record_chunk(&encoded).unwrap();
    records[0].cumulative_digest = ZERO_DIGEST.to_owned();
    digest_store
        .conn
        .execute(
            "DELETE FROM projection_journal_chunks
             WHERE generation = 1 AND first_sequence = 1",
            [],
        )
        .unwrap();
    insert_record_chunk(&digest_store.conn, &records).unwrap();
    digest_store
        .conn
        .execute(
            "UPDATE projection_journal_state SET cumulative_digest = ?1",
            [ZERO_DIGEST],
        )
        .unwrap();
    assert!(matches!(
        digest_store.projection_journal_snapshot(None),
        Err(StoreError::InvalidProjectionJournalData(_))
    ));
}

#[test]
fn insert_only_session_and_run_apis_append_dependencies_once() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let source_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let file_id = Uuid::new_v4();
    store
        .upsert_capture_source(&source(source_id, "/source/session.jsonl"))
        .unwrap();
    store
        .upsert_file_touched(&file_touch(file_id, source_id))
        .unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();
    assert!(all_records(&store)[0].canonical_payload.as_ref().unwrap()["actor"].is_null());

    let actor = session(session_id, source_id);
    assert!(store.insert_session_if_absent(&actor).unwrap());
    assert!(!store.insert_session_if_absent(&actor).unwrap());
    let execution = run(run_id, session_id, source_id);
    assert!(store.insert_run_if_absent(&execution).unwrap());
    assert!(!store.insert_run_if_absent(&execution).unwrap());

    let records = all_records(&store);
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].stable_entity_id, file_id);
    assert_eq!(records[1].entity_revision, 2);
    assert!(records[1].canonical_payload.as_ref().unwrap()["actor"].is_object());
}

#[test]
fn history_link_add_delete_and_undelete_revision_the_vcs_observation() {
    let temp = tempdir().unwrap();
    let store = Store::open(temp.path().join("ctx.db")).unwrap();
    let source_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let record_id = Uuid::new_v4();
    let change_id = Uuid::new_v4();
    store
        .insert_record(&history_record(record_id, "linked"))
        .unwrap();
    store
        .upsert_capture_source(&source(source_id, "/source/session.jsonl"))
        .unwrap();
    let mut actor = session(session_id, source_id);
    actor.history_record_id = Some(record_id);
    store.upsert_session(&actor).unwrap();
    let mut change = vcs_change(&store, change_id, source_id);
    change.source_id = None;
    store.upsert_vcs_change(&change).unwrap();
    store.activate_projection_journal(FINGERPRINT).unwrap();

    let mut link = vcs_link(Uuid::new_v4(), record_id, change_id);
    store.upsert_history_record_link(&link).unwrap();
    link.sync.deleted_at = Some(now());
    store.upsert_history_record_link(&link).unwrap();
    link.sync.deleted_at = None;
    store.upsert_history_record_link(&link).unwrap();

    let records = all_records(&store)
        .into_iter()
        .filter(|record| record.stable_entity_id == change_id)
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 4);
    assert!(records[0].canonical_payload.as_ref().unwrap()["actor"].is_null());
    assert!(records[1].canonical_payload.as_ref().unwrap()["actor"].is_object());
    assert!(records[2].canonical_payload.as_ref().unwrap()["actor"].is_null());
    assert!(records[3].canonical_payload.as_ref().unwrap()["actor"].is_object());
    assert_eq!(
        records
            .iter()
            .map(|record| record.entity_revision)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
}
