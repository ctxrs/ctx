use std::fs;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    new_id, Event, EventRole, EventType, Fidelity, HistoryRecord, SyncMetadata, SyncState,
    Visibility,
};
use rusqlite::params;
use uuid::Uuid;

use crate::Store;

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("TEST_TMPDIR")
        .map(|path| std::path::PathBuf::from(path).join("test-data"))
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("target/test-data"));
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("ctx-history-store-search-order-")
        .tempdir_in(root)
        .unwrap()
}

fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn sync_metadata() -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::LocalOnly,
        fidelity: Fidelity::Imported,
        sync_state: SyncState::LocalOnly,
        sync_version: 0,
        deleted_at: None,
        metadata: serde_json::json!({}),
    }
}

fn local_preview_event(seq: u64, text: &str) -> Event {
    Event {
        id: new_id(),
        seq,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload: serde_json::json!({ "text": text }),
        payload_blob_id: None,
        dedupe_key: None,
        sync: sync_metadata(),
    }
}

fn policy_event(
    seq: u64,
    event_type: EventType,
    role: Option<EventRole>,
    payload: serde_json::Value,
) -> Event {
    Event {
        id: new_id(),
        seq,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type,
        role,
        occurred_at: fixed_time(),
        capture_source_id: None,
        payload,
        payload_blob_id: None,
        dedupe_key: None,
        sync: sync_metadata(),
    }
}

#[test]
fn indexed_history_item_count_uses_sessions_and_events() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    for (idx, session_id) in [
        "018f45d0-0000-7000-8000-000000050001",
        "018f45d0-0000-7000-8000-000000050002",
    ]
    .into_iter()
    .enumerate()
    {
        store
            .conn
            .execute(
                r#"
                INSERT INTO sessions
                (id, provider, external_session_id, agent_type, is_primary, status, fidelity,
                 started_at_ms, created_at_ms, updated_at_ms)
                VALUES (?1, 'codex', ?2, 'primary', 1, 'imported', 'full', 1, 1, 1)
                "#,
                params![session_id, format!("external-session-{idx}")],
            )
            .unwrap();
    }

    for (seq, event_id, session_id) in [
        (
            1_i64,
            "018f45d0-0000-7000-8000-000000060001",
            "018f45d0-0000-7000-8000-000000050001",
        ),
        (
            2_i64,
            "018f45d0-0000-7000-8000-000000060002",
            "018f45d0-0000-7000-8000-000000050001",
        ),
        (
            3_i64,
            "018f45d0-0000-7000-8000-000000060003",
            "018f45d0-0000-7000-8000-000000050002",
        ),
    ] {
        store
            .conn
            .execute(
                r#"
                INSERT INTO events
                (id, seq, session_id, event_type, role, occurred_at_ms, payload_json)
                VALUES (?1, ?2, ?3, 'message', 'user', 1, '{}')
                "#,
                params![event_id, seq, session_id],
            )
            .unwrap();
    }

    assert_eq!(store.indexed_history_item_count().unwrap(), 5);
}

#[test]
fn capture_source_count_uses_aggregate_count() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    for index in 1..=3 {
        store
            .conn
            .execute(
                r#"
                INSERT INTO capture_sources
                (id, kind, provider, machine_id, started_at_ms, fidelity)
                VALUES (?1, 'provider_import', 'codex', 'test-machine', ?2, 'full')
                "#,
                params![
                    format!("018f45d0-0000-7000-8000-000000070{index:03}"),
                    i64::from(index),
                ],
            )
            .unwrap();
    }

    assert_eq!(store.capture_source_count().unwrap(), 3);
}

fn stable_tie_record(index: u16) -> HistoryRecord {
    let mut record = HistoryRecord::new(
        "Stable tie title",
        "stabletie exact equal body for deterministic fts ranking",
        vec!["stabletie".into()],
        "task",
        None,
    );
    record.id = Uuid::parse_str(&format!("018f45d0-0000-7000-8000-000000010{index:03}")).unwrap();
    record.created_at = fixed_time();
    record.updated_at = fixed_time();
    record
}

fn assert_search_order(store: &Store, expected: &[Uuid]) {
    let actual = store
        .search_records("stabletie", 10)
        .unwrap()
        .into_iter()
        .map(|record| record.id)
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn search_records_equal_fts_scores_use_record_id_across_refresh_and_reopen() {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    for index in [4, 1, 3, 2] {
        store.insert_record(&stable_tie_record(index)).unwrap();
    }

    let expected = vec![
        stable_tie_record(1).id,
        stable_tie_record(2).id,
        stable_tie_record(3).id,
        stable_tie_record(4).id,
    ];
    assert_search_order(&store, &expected);

    store.upsert_record(&stable_tie_record(3)).unwrap();
    assert_search_order(&store, &expected);

    drop(store);
    let reopened = Store::open(&path).unwrap();
    assert_search_order(&reopened, &expected);
}

#[test]
fn search_records_empty_or_no_token_query_returns_empty() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let record = stable_tie_record(1);
    store.insert_record(&record).unwrap();

    assert!(store.search_records("", 10).unwrap().is_empty());
    assert!(store.search_records("!!!", 10).unwrap().is_empty());
    assert!(store.search_records("---", 10).unwrap().is_empty());
    assert!(store.search_records("___", 10).unwrap().is_empty());
    assert!(store.search_records_page("", 10, 0).unwrap().is_empty());
}

#[test]
fn event_search_preserves_local_payload_text() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let local_event =
        local_preview_event(1, "cwd=/home/example/private token=ghp_1234567890abcdef");
    let raw_event = local_preview_event(
        2,
        "rawmarker cwd=/home/example/private token=ghp_1234567890abcdef",
    );

    store.upsert_event(&local_event).unwrap();
    store.upsert_event(&raw_event).unwrap();

    let local_preview: String = store
        .conn
        .query_row(
            "SELECT preview_text FROM event_search WHERE event_id = ?1",
            [local_event.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(local_preview.contains("/home/example/private"));
    assert!(local_preview.contains("ghp_1234567890abcdef"));

    let raw_preview: String = store
        .conn
        .query_row(
            "SELECT preview_text FROM event_search WHERE event_id = ?1",
            [raw_event.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(raw_preview.contains("rawmarker"));
    assert!(raw_preview.contains("/home/example/private"));
    assert!(raw_preview.contains("ghp_1234567890abcdef"));

    let hits = store.search_event_hits("rawmarker", 10).unwrap();
    assert!(hits.iter().any(|hit| hit.event_id == raw_event.id));
}

#[test]
fn event_search_indexes_policy_allowed_agent_content_only() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let events = vec![
        policy_event(
            1,
            EventType::Message,
            Some(EventRole::User),
            serde_json::json!({ "text": "conversation-oracle" }),
        ),
        policy_event(
            2,
            EventType::ToolCall,
            Some(EventRole::Assistant),
            serde_json::json!({
                "tool": "exec_command",
                "command": "cargo test policy-command-oracle",
                "arguments_preview": "{\"cmd\":\"cargo test policy-command-oracle\"}"
            }),
        ),
        policy_event(
            3,
            EventType::ToolCall,
            Some(EventRole::Assistant),
            serde_json::json!({ "text": "tooltoporacle" }),
        ),
        policy_event(
            4,
            EventType::ToolCall,
            Some(EventRole::Assistant),
            serde_json::json!({
                "body": {
                    "text": "toolnestoracle"
                }
            }),
        ),
        policy_event(
            5,
            EventType::CommandOutput,
            Some(EventRole::Tool),
            serde_json::json!({
                "exit_code": 0,
                "output_preview": "success-output-oracle"
            }),
        ),
        policy_event(
            6,
            EventType::CommandOutput,
            Some(EventRole::Tool),
            serde_json::json!({
                "exit_code": 101,
                "output_preview": "failure-output-oracle"
            }),
        ),
        policy_event(
            7,
            EventType::CommandOutput,
            Some(EventRole::Tool),
            serde_json::json!({
                "text": "success-native-output-oracle",
                "content_retention": "metadata_only",
                "body": {
                    "content_retention": "metadata_only"
                }
            }),
        ),
        policy_event(
            8,
            EventType::CommandOutput,
            Some(EventRole::Tool),
            serde_json::json!({
                "content_retention": "failed_output_preview",
                "body": {
                    "content_retention": "failed_output_preview",
                    "output_preview": "failed-native-output-oracle"
                }
            }),
        ),
        policy_event(
            9,
            EventType::Notice,
            Some(EventRole::System),
            serde_json::json!({ "text": "notice-oracle" }),
        ),
        policy_event(
            10,
            EventType::Message,
            Some(EventRole::Assistant),
            serde_json::json!({ "unexpected_field": "json-fallback-oracle" }),
        ),
    ];
    for event in &events {
        store.upsert_event(event).unwrap();
    }

    assert_eq!(
        store
            .search_event_hits("conversation-oracle", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .search_event_hits("policy-command-oracle", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store.search_event_hits("tooltoporacle", 10).unwrap().len(),
        1
    );
    assert_eq!(
        store.search_event_hits("toolnestoracle", 10).unwrap().len(),
        1
    );
    assert_eq!(
        store
            .search_event_hits("failure-output-oracle", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .search_event_hits("failed-native-output-oracle", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search_event_hits("success-output-oracle", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("success-native-output-oracle", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("notice-oracle", 10)
        .unwrap()
        .is_empty());
    assert!(store
        .search_event_hits("json-fallback-oracle", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn upsert_record_updates_record_search_without_rebuilding_event_search() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO event_search
            (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
            VALUES ('sentinel-event', NULL, NULL, 'user', 'preserve-event-search-row', 'message')
            "#,
            [],
        )
        .unwrap();

    let record = stable_tie_record(5);
    store.upsert_record(&record).unwrap();

    let sentinel_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM event_search WHERE event_id = 'sentinel-event'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sentinel_count, 1);
    assert_search_order(&store, &[record.id]);
}
