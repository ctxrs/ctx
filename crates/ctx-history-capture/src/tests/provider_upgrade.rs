use crate::provider::importer::{
    import_normalized_provider_captures, provider_event_uuid, provider_scoped_source_uuid,
    provider_session_uuid, provider_source_event_uuid, provider_source_root_identity,
    provider_source_session_uuid, provider_source_uuid, provider_sync_metadata, timestamps,
};
use crate::tests::support::paths::tempdir;
use crate::tests::support::provider_state::provider_collision_capture;
use crate::{NormalizedProviderImportOptions, ProviderImportSummary, ProviderNormalizationResult};
use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Fidelity, Session, SessionStatus,
};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::json;

#[test]
fn provider_import_reuses_pre_identity_session_with_exact_source_path_proof() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let provider_session_id = "pre-identity-provider-session";
    let source_format = "claude_projects_jsonl_tree";
    let raw_source_path = temp
        .path()
        .join("projects/workspace/pre-identity-provider-session.jsonl")
        .display()
        .to_string();
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let legacy_source_id = provider_source_uuid(provider, provider_session_id);
    let scoped_source_id = provider_scoped_source_uuid(
        provider,
        provider_session_id,
        source_format,
        Some(&raw_source_path),
    );
    let legacy_session_id = provider_session_uuid(provider, provider_session_id);
    assert_ne!(legacy_source_id, scoped_source_id);

    store
        .upsert_capture_source(&CaptureSource {
            id: legacy_source_id,
            descriptor: CaptureSourceDescriptor {
                kind: CaptureSourceKind::ProviderImport,
                provider,
                machine_id: "test-machine".to_owned(),
                process_id: None,
                cwd: Some("/workspace/example".to_owned()),
                raw_source_path: Some(raw_source_path.clone()),
                source_format: None,
                source_root: Some(raw_source_path.clone()),
                source_identity: None,
                external_session_id: Some(provider_session_id.to_owned()),
            },
            started_at: occurred_at,
            ended_at: None,
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();
    store
        .upsert_session(&Session {
            id: legacy_session_id,
            history_record_id: None,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(legacy_source_id),
            provider,
            external_session_id: Some(provider_session_id.to_owned()),
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at: occurred_at,
            ended_at: None,
            timestamps: timestamps(occurred_at),
            sync: provider_sync_metadata(Fidelity::Imported, json!({"legacy": true})),
        })
        .unwrap();

    let capture = provider_collision_capture(
        provider,
        provider_session_id,
        source_format,
        &raw_source_path,
        occurred_at,
    );
    for iteration in 0..2 {
        let summary = import_normalized_provider_captures(
            &mut store,
            ProviderNormalizationResult {
                summary: ProviderImportSummary::default(),
                captures: vec![(1, capture.clone())],
                files_touched: vec![],
            },
            NormalizedProviderImportOptions::default(),
        )
        .unwrap_or_else(|err| panic!("import iteration {iteration} failed: {err:?}"));
        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(store.list_sessions().unwrap().len(), 1);
        assert_eq!(
            store
                .get_session(legacy_session_id)
                .unwrap()
                .capture_source_id,
            Some(scoped_source_id)
        );
        assert_eq!(
            store.events_for_session(legacy_session_id).unwrap().len(),
            1
        );
    }

    let different_path = temp
        .path()
        .join("projects/workspace/copied-pre-identity-provider-session.jsonl")
        .display()
        .to_string();
    let summary = import_normalized_provider_captures(
        &mut store,
        ProviderNormalizationResult {
            summary: ProviderImportSummary::default(),
            captures: vec![(
                1,
                provider_collision_capture(
                    provider,
                    provider_session_id,
                    source_format,
                    &different_path,
                    occurred_at + chrono::Duration::seconds(1),
                ),
            )],
            files_touched: vec![],
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(store.list_sessions().unwrap().len(), 2);
}

#[test]
fn provider_import_reuses_canonical_event_after_duplicate_repair() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let provider = CaptureProvider::Codex;
    let provider_session_id = "duplicate-provider-session";
    let source_format = "codex_session_jsonl_tree";
    let raw_source_path = temp
        .path()
        .join("sessions/duplicate-provider-session.jsonl")
        .display()
        .to_string();
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let old_source_id = provider_source_uuid(provider, provider_session_id);
    let new_source_id = provider_scoped_source_uuid(
        provider,
        provider_session_id,
        source_format,
        Some(&raw_source_path),
    );
    let source_identity = provider_source_root_identity(provider, source_format, &raw_source_path);
    let old_session_id = provider_session_uuid(provider, provider_session_id);
    let new_session_id = provider_source_session_uuid(&source_identity, provider_session_id);
    let old_event_id = provider_event_uuid(provider, provider_session_id, 0);
    let new_event_id = provider_source_event_uuid(new_source_id, 0);
    let event_hash = "event-hash";

    Store::open(&path).unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            DROP TRIGGER IF EXISTS trg_sessions_provider_source_identity_insert;
            DROP TRIGGER IF EXISTS trg_sessions_provider_source_identity_update;
            DROP INDEX IF EXISTS idx_sessions_unique_capture_source_external_session;
            DROP TABLE event_aliases;
            DROP TABLE session_aliases;
            "#,
        )
        .unwrap();
        for (id, source_format, source_identity) in [
            (old_source_id, None, None),
            (
                new_source_id,
                Some(source_format),
                Some(source_identity.as_str()),
            ),
        ] {
            conn.execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, raw_source_path, source_format,
                 source_root, source_identity, external_session_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', 'codex', 'test-machine', ?2, ?3,
                        ?2, ?4, ?5, ?6, 'imported')
                "#,
                rusqlite::params![
                    id.to_string(),
                    raw_source_path,
                    source_format,
                    source_identity,
                    provider_session_id,
                    occurred_at.timestamp_millis(),
                ],
            )
            .unwrap();
        }
        for (id, source_id, created_at_ms) in [
            (old_session_id, old_source_id, 1),
            (new_session_id, new_source_id, 2),
        ] {
            conn.execute(
                r#"
                INSERT INTO sessions
                (id, capture_source_id, provider, external_session_id, agent_type,
                 is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms)
                VALUES (?1, ?2, 'codex', ?3, 'primary', 1, 'imported', 'imported',
                        ?4, ?5, ?5)
                "#,
                rusqlite::params![
                    id.to_string(),
                    source_id.to_string(),
                    provider_session_id,
                    occurred_at.timestamp_millis(),
                    created_at_ms,
                ],
            )
            .unwrap();
        }
        for (id, seq, session_id, source_id, dedupe_key) in [
            (
                old_event_id,
                1,
                old_session_id,
                old_source_id,
                Store::provider_event_dedupe_key(provider, provider_session_id, 0, event_hash),
            ),
            (
                new_event_id,
                2,
                new_session_id,
                new_source_id,
                Store::provider_source_event_dedupe_key(new_source_id, 0, event_hash),
            ),
        ] {
            conn.execute(
                r#"
                INSERT INTO events
                (id, seq, session_id, event_type, role, occurred_at_ms,
                 capture_source_id, payload_json, dedupe_key, fidelity, metadata_json)
                VALUES (?1, ?2, ?3, 'message', 'user', ?4, ?5, '{}', ?6,
                        'imported', json_object(
                            'provider_event_index', 0,
                            'provider_event_hash', ?7
                        ))
                "#,
                rusqlite::params![
                    id.to_string(),
                    seq as i64,
                    session_id.to_string(),
                    occurred_at.timestamp_millis(),
                    source_id.to_string(),
                    dedupe_key,
                    event_hash,
                ],
            )
            .unwrap();
        }
        conn.execute_batch("PRAGMA user_version = 46;").unwrap();
    }

    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(store.events_for_session(old_session_id).unwrap().len(), 1);
    assert_eq!(store.get_event(new_event_id).unwrap().id, old_event_id);

    let mut capture = provider_collision_capture(
        provider,
        provider_session_id,
        source_format,
        &raw_source_path,
        occurred_at,
    );
    capture.event.as_mut().unwrap().provider_event_hash = Some(event_hash.to_owned());
    for iteration in 0..2 {
        let summary = import_normalized_provider_captures(
            &mut store,
            ProviderNormalizationResult {
                summary: ProviderImportSummary::default(),
                captures: vec![(1, capture.clone())],
                files_touched: vec![],
            },
            NormalizedProviderImportOptions::default(),
        )
        .unwrap_or_else(|err| panic!("import iteration {iteration} failed: {err:?}"));
        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
        assert_eq!(store.list_sessions().unwrap().len(), 1);
        assert_eq!(store.events_for_session(old_session_id).unwrap().len(), 1);
        assert_eq!(store.get_event(new_event_id).unwrap().id, old_event_id);
    }
}
