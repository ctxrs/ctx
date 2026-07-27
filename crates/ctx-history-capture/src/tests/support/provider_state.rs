use crate::provider::importer::{
    import_normalized_provider_captures, provider_scoped_source_uuid, provider_source_event_uuid,
    provider_source_root_identity, provider_source_session_uuid,
};
use crate::tests::support::paths::tempdir;
use crate::{
    FixtureOptions, NormalizedProviderImportOptions, ProviderFileTouchedEnvelope,
    ProviderFixtureImportOptions, ProviderImportSummary, ProviderNormalizationResult,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Confidence, EventRole, EventType, Fidelity, FileChangeKind,
    ProviderCaptureEnvelope, ProviderEventEnvelope, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(in crate::tests) fn delete_event_and_downgrade_provider_policy_cursor(
    database: &Path,
    store: &Store,
    machine_id: &str,
    stream: &str,
    event_id: Uuid,
) -> u64 {
    let cursor = store
        .get_sync_cursor(None, machine_id, stream)
        .unwrap()
        .expect("provider cursor exists after initial import");
    let mut encoded: Value = serde_json::from_str(&cursor.cursor).unwrap();
    let current_policy = encoded["o"]
        .as_u64()
        .expect("certified provider cursor has a policy revision");
    assert!(current_policy > 0);
    encoded["o"] = json!(current_policy - 1);

    let connection = Connection::open(database).unwrap();
    assert_eq!(
        connection
            .execute("DELETE FROM events WHERE id = ?1", [event_id.to_string()])
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .execute(
                "UPDATE sync_cursors SET cursor = ?1 WHERE device_id = ?2 AND stream = ?3",
                rusqlite::params![serde_json::to_string(&encoded).unwrap(), machine_id, stream],
            )
            .unwrap(),
        1
    );
    current_policy
}

pub(in crate::tests) fn only_provider_cursor_stream(database: &Path, machine_id: &str) -> String {
    let connection = Connection::open(database).unwrap();
    let mut statement = connection
        .prepare("SELECT stream FROM sync_cursors WHERE device_id = ?1 ORDER BY stream")
        .unwrap();
    let streams = statement
        .query_map([machine_id], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        streams.len(),
        1,
        "expected one provider cursor: {streams:?}"
    );
    streams.into_iter().next().unwrap()
}

pub(in crate::tests) fn assert_provider_policy_cursor_restored(
    store: &Store,
    machine_id: &str,
    stream: &str,
    expected_policy: u64,
) {
    let cursor = store
        .get_sync_cursor(None, machine_id, stream)
        .unwrap()
        .expect("provider cursor exists after repair");
    let encoded: Value = serde_json::from_str(&cursor.cursor).unwrap();
    assert_eq!(encoded["o"].as_u64(), Some(expected_policy));
}

pub(in crate::tests) fn fixture_options(dedupe_key: &str, title: &str) -> FixtureOptions {
    FixtureOptions {
        title: title.to_owned(),
        body: "captured body".to_owned(),
        tags: vec!["capture-test".to_owned()],
        dedupe_key: Some(dedupe_key.to_owned()),
        machine_id: Some("test-machine".to_owned()),
        cwd: Some(PathBuf::from("/tmp/work")),
        occurred_at: DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    }
}

pub(in crate::tests) fn fixed_import_options(path: PathBuf) -> ProviderFixtureImportOptions {
    ProviderFixtureImportOptions {
        machine_id: "test-machine".into(),
        source_path: Some(path),
        imported_at: DateTime::parse_from_rfc3339("2026-06-23T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        history_record_id: None,
        expected_provider: None,
        ..ProviderFixtureImportOptions::default()
    }
}

pub(in crate::tests) fn provider_fixture_session_id(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_path: &Path,
) -> Uuid {
    provider_import_session_id_for_path(
        provider,
        "normalized_provider_fixture_jsonl",
        source_path,
        provider_session_id,
    )
}

pub(in crate::tests) fn provider_import_session_id_for_path(
    provider: CaptureProvider,
    source_format: &str,
    source_path: &Path,
    provider_session_id: &str,
) -> Uuid {
    let source_path = source_path.display().to_string();
    let source_identity = provider_source_root_identity(provider, source_format, &source_path);
    provider_source_session_uuid(&source_identity, provider_session_id)
}

pub(in crate::tests) fn stored_provider_session_id(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
) -> Uuid {
    let sessions = store
        .sessions_by_external_session_limited(provider, provider_session_id, 10)
        .unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "expected exactly one stored session for {}/{}",
        provider.as_str(),
        provider_session_id
    );
    sessions[0].id
}

pub(in crate::tests) fn assert_provider_source_collision_is_distinct(
    first_source_format: &str,
    first_source_path: &str,
    second_source_format: &str,
    second_source_path: &str,
) {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let provider_session_id = "shared-provider-session";
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let first_source_id = provider_scoped_source_uuid(
        provider,
        provider_session_id,
        first_source_format,
        Some(first_source_path),
    );
    let second_source_id = provider_scoped_source_uuid(
        provider,
        provider_session_id,
        second_source_format,
        Some(second_source_path),
    );
    let first_source_identity =
        provider_source_root_identity(provider, first_source_format, first_source_path);
    let second_source_identity =
        provider_source_root_identity(provider, second_source_format, second_source_path);
    assert_ne!(first_source_id, second_source_id);
    assert_ne!(first_source_identity, second_source_identity);

    let normalization = ProviderNormalizationResult {
        summary: ProviderImportSummary::default(),
        captures: vec![
            (
                1,
                provider_collision_capture(
                    provider,
                    provider_session_id,
                    first_source_format,
                    first_source_path,
                    occurred_at,
                ),
            ),
            (
                2,
                provider_collision_capture(
                    provider,
                    provider_session_id,
                    second_source_format,
                    second_source_path,
                    occurred_at,
                ),
            ),
        ],
        files_touched: vec![
            (
                1,
                provider_collision_file_touch(
                    provider,
                    provider_session_id,
                    first_source_format,
                    first_source_path,
                    occurred_at,
                ),
            ),
            (
                2,
                provider_collision_file_touch(
                    provider,
                    provider_session_id,
                    second_source_format,
                    second_source_path,
                    occurred_at,
                ),
            ),
        ],
    };

    let summary = import_normalized_provider_captures(
        &mut store,
        normalization,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(store.capture_source_count().unwrap(), 2);

    let first_source = store.get_capture_source(first_source_id).unwrap();
    let second_source = store.get_capture_source(second_source_id).unwrap();
    assert_eq!(
        first_source.descriptor.raw_source_path.as_deref(),
        Some(first_source_path)
    );
    assert_eq!(
        first_source.sync.metadata["source_format"].as_str(),
        Some(first_source_format)
    );
    assert_eq!(
        first_source.descriptor.source_identity.as_deref(),
        Some(first_source_identity.as_str())
    );
    assert_eq!(
        second_source.descriptor.raw_source_path.as_deref(),
        Some(second_source_path)
    );
    assert_eq!(
        second_source.sync.metadata["source_format"].as_str(),
        Some(second_source_format)
    );
    assert_eq!(
        second_source.descriptor.source_identity.as_deref(),
        Some(second_source_identity.as_str())
    );

    let first_session_id =
        provider_source_session_uuid(&first_source_identity, provider_session_id);
    let second_session_id =
        provider_source_session_uuid(&second_source_identity, provider_session_id);
    let sessions = store
        .sessions_by_external_session_limited(provider, provider_session_id, 10)
        .unwrap()
        .into_iter()
        .map(|session| session.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        sessions,
        BTreeSet::from([first_session_id, second_session_id])
    );

    let first_event_source_ids = store
        .events_for_session(first_session_id)
        .unwrap()
        .into_iter()
        .map(|event| event.capture_source_id.unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(first_event_source_ids, BTreeSet::from([first_source_id]));
    let second_event_source_ids = store
        .events_for_session(second_session_id)
        .unwrap()
        .into_iter()
        .map(|event| event.capture_source_id.unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(second_event_source_ids, BTreeSet::from([second_source_id]));

    let archive = store.export_archive().unwrap();
    assert_eq!(archive.files_touched.len(), 2);
    let touched_source_ids = archive
        .files_touched
        .iter()
        .map(|file| file.source_id.unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        touched_source_ids,
        BTreeSet::from([first_source_id, second_source_id])
    );
    for file in archive.files_touched {
        let source_id = file.source_id.unwrap();
        assert_eq!(
            file.event_id,
            Some(provider_source_event_uuid(source_id, 0))
        );
    }
}

pub(in crate::tests) fn provider_collision_capture(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: &str,
    occurred_at: DateTime<Utc>,
) -> ProviderCaptureEnvelope {
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider,
        source: ProviderSourceEnvelope {
            source_format: source_format.to_owned(),
            machine_id: "test-machine".to_owned(),
            observed_at: occurred_at,
            raw_source_path: Some(raw_source_path.to_owned()),
            source_root: Some(raw_source_path.to_owned()),
            trust: ProviderSourceTrust::ProviderExport,
            fidelity: Fidelity::Imported,
            cursor: None,
            idempotency_key: Some(format!(
                "provider-source:{}:{}:{}",
                provider.as_str(),
                source_format,
                provider_session_id
            )),
            metadata: json!({}),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            started_at: occurred_at,
            ended_at: None,
            cwd: Some("/workspace/example".to_owned()),
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!(
                "provider-session:{}:{}",
                provider.as_str(),
                provider_session_id
            )),
            artifacts: Vec::new(),
            metadata: json!({}),
        },
        event: Some(ProviderEventEnvelope {
            provider_event_index: 0,
            provider_event_hash: None,
            cursor: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at,
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!(
                "provider-event:{}:{}:0",
                provider.as_str(),
                provider_session_id
            )),
            artifacts: Vec::new(),
            payload: json!({"text": "same provider event payload"}),
            metadata: json!({}),
        }),
    }
}

pub(in crate::tests) fn provider_collision_file_touch(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: &str,
    occurred_at: DateTime<Utc>,
) -> ProviderFileTouchedEnvelope {
    ProviderFileTouchedEnvelope {
        provider,
        provider_session_id: provider_session_id.to_owned(),
        provider_touch_index: 0,
        provider_event_index: Some(0),
        raw_source_path: Some(raw_source_path.to_owned()),
        source_root: Some(raw_source_path.to_owned()),
        path: "src/lib.rs".to_owned(),
        change_kind: Some(FileChangeKind::Modified),
        old_path: None,
        line_count_delta: Some(1),
        confidence: Confidence::Explicit,
        occurred_at,
        source_format: source_format.to_owned(),
        metadata: json!({}),
    }
}
