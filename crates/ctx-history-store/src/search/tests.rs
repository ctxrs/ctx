use std::collections::HashMap;
use std::fs;

use chrono::{DateTime, Duration, Utc};
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

fn insert_session(store: &Store, session_id: Uuid) {
    store
        .conn
        .execute(
            r#"
            INSERT INTO sessions
            (id, provider, external_session_id, agent_type, is_primary, status, fidelity,
             started_at_ms, created_at_ms, updated_at_ms)
            VALUES (?1, 'codex', ?2, 'primary', 1, 'imported', 'full', 1, 1, 1)
            "#,
            params![session_id.to_string(), format!("session-{session_id}")],
        )
        .unwrap();
}

fn session_event(
    seq: u64,
    session_id: Uuid,
    event_type: EventType,
    role: Option<EventRole>,
    text: &str,
) -> Event {
    let mut event = local_preview_event(seq, text);
    event.session_id = Some(session_id);
    event.event_type = event_type;
    event.role = role;
    event
}

fn with_occurred_at(mut event: Event, offset_minutes: i64) -> Event {
    event.occurred_at = fixed_time() + Duration::minutes(offset_minutes);
    event
}

#[test]
fn preferred_event_search_ranks_conversation_above_equal_tool_events() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let needle = "conversation-ranking-needle";
    let events = [
        policy_event(
            1,
            EventType::ToolCall,
            Some(EventRole::Assistant),
            serde_json::json!({"text": needle}),
        ),
        policy_event(
            2,
            EventType::CommandStarted,
            Some(EventRole::Assistant),
            serde_json::json!({"command": needle}),
        ),
        policy_event(
            3,
            EventType::Message,
            Some(EventRole::Assistant),
            serde_json::json!({"text": needle}),
        ),
        policy_event(
            4,
            EventType::Summary,
            Some(EventRole::Assistant),
            serde_json::json!({"summary": needle}),
        ),
    ];
    for event in &events {
        store.upsert_event(event).unwrap();
    }

    let hits = store
        .search_event_hits_page_prefer_conversation(needle, 10, 0)
        .unwrap();
    assert_eq!(hits.len(), events.len());
    assert!(matches!(
        hits[0].event_type,
        EventType::Message | EventType::Summary
    ));
    assert!(matches!(
        hits[1].event_type,
        EventType::Message | EventType::Summary
    ));
    assert!(matches!(
        hits[2].event_type,
        EventType::ToolCall | EventType::CommandStarted
    ));
    assert!(matches!(
        hits[3].event_type,
        EventType::ToolCall | EventType::CommandStarted
    ));
}

#[test]
fn preferred_event_search_keeps_materially_stronger_tool_evidence_first() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let needle = "exact-tool-evidence-needle";
    let message = policy_event(
        1,
        EventType::Message,
        Some(EventRole::Assistant),
        serde_json::json!({
            "text": format!("{needle} {}", "unrelated context ".repeat(100))
        }),
    );
    let tool = policy_event(
        2,
        EventType::ToolCall,
        Some(EventRole::Assistant),
        serde_json::json!({"text": format!("{needle} {needle} {needle}")}),
    );
    store.upsert_event(&message).unwrap();
    store.upsert_event(&tool).unwrap();

    let hits = store
        .search_event_hits_page_prefer_conversation(needle, 10, 0)
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].event_id, tool.id);
}

#[test]
fn preferred_event_search_keeps_term_coverage_ahead_of_event_type_preference() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let message = policy_event(
        1,
        EventType::Message,
        Some(EventRole::Assistant),
        serde_json::json!({"text": "coverage-alpha"}),
    );
    let tool = policy_event(
        2,
        EventType::ToolCall,
        Some(EventRole::Assistant),
        serde_json::json!({"text": "coverage-alpha coverage-beta"}),
    );
    store.upsert_event(&message).unwrap();
    store.upsert_event(&tool).unwrap();

    let hits = store
        .search_event_hits_page_prefer_conversation("coverage-alpha coverage-beta", 10, 0)
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].event_id, tool.id);
    assert_eq!(hits[1].event_id, message.id);
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

fn record_with_id(id: &str, title: &str, body: &str) -> HistoryRecord {
    let mut record = HistoryRecord::new(
        title,
        body,
        Vec::new(),
        "task",
        Some("/workspace/multilingual".into()),
    );
    record.id = Uuid::parse_str(id).unwrap();
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
fn multi_word_record_search_returns_partial_matches_and_orders_by_term_coverage() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let all_terms = record_with_id(
        "018f45d0-0000-7000-8000-000000080010",
        "Complete migration diagnosis",
        "sqlite rollback checksum",
    );
    let sqlite_only = record_with_id(
        "018f45d0-0000-7000-8000-000000080011",
        "SQLite diagnosis",
        "sqlite investigation",
    );
    let rollback_only = record_with_id(
        "018f45d0-0000-7000-8000-000000080012",
        "Rollback diagnosis",
        "rollback investigation",
    );
    for record in [&sqlite_only, &all_terms, &rollback_only] {
        store.insert_record(record).unwrap();
    }

    let first = store
        .search_records("sqlite rollback checksum", 10)
        .unwrap();
    let second = store
        .search_records("sqlite rollback checksum", 10)
        .unwrap();
    let ids = first.iter().map(|record| record.id).collect::<Vec<_>>();

    assert_eq!(ids.first(), Some(&all_terms.id));
    assert!(ids.contains(&sqlite_only.id));
    assert!(ids.contains(&rollback_only.id));
    assert_eq!(first, second, "multi-word ordering must be deterministic");
    assert_eq!(
        first,
        store
            .search_records("sqlite SQLITE rollback ROLLBACK checksum", 10)
            .unwrap(),
        "duplicate query words must not change coverage or ordering"
    );
}

#[test]
fn multi_word_record_search_fallback_uses_the_same_or_semantics() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let all_terms = record_with_id(
        "018f45d0-0000-7000-8000-000000080013",
        "Complete fallback match",
        "sqlite rollback",
    );
    let partial = record_with_id(
        "018f45d0-0000-7000-8000-000000080014",
        "Partial fallback match",
        "sqlite only",
    );
    store.insert_record(&partial).unwrap();
    store.insert_record(&all_terms).unwrap();
    store
        .conn
        .execute_batch("DROP TABLE ctx_history_search")
        .unwrap();

    let hits = store.search_records("sqlite rollback", 10).unwrap();
    let ids = hits.iter().map(|record| record.id).collect::<Vec<_>>();
    assert_eq!(ids, vec![all_terms.id, partial.id]);
}

#[test]
fn search_records_still_matches_latin_code_tokens() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let record = record_with_id(
        "018f45d0-0000-7000-8000-000000080001",
        "Latin code search",
        "SearchResultScope Event remains discoverable through normal Latin code tokens.",
    );
    store.insert_record(&record).unwrap();

    let hits = store.search_records("SearchResultScope", 10).unwrap();
    assert!(hits.iter().any(|hit| hit.id == record.id));
    let sidecar_rows: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM ctx_history_search_scriptgram",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sidecar_rows, 0);
}

#[test]
fn search_records_recalls_unspaced_cjk_and_mixed_script_terms() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let record = record_with_id(
        "018f45d0-0000-7000-8000-000000080002",
        "Multilingual script search",
        "OAuth認証の検索状態を確認し、寿司APIの状態を保存します。中文登录态异常需要重新认证。Korean OAuth인증오류를 재현했습니다.",
    );
    store.insert_record(&record).unwrap();

    let mut missing = Vec::new();
    for (label, query) in [
        ("japanese auth", "認証"),
        ("japanese search", "検索"),
        ("japanese sushi", "寿司"),
        ("mixed oauth auth", "OAuth 認証"),
        ("mixed api status", "API 状態"),
        ("chinese two-char", "认证"),
        ("chinese three-char", "登录态"),
        ("korean particle stem", "오류"),
        ("korean auth", "인증"),
    ] {
        let hits = store.search_records(query, 10).unwrap();
        if !hits.iter().any(|hit| hit.id == record.id) {
            missing.push(format!("{label}: {query}"));
        }
    }

    assert!(
        missing.is_empty(),
        "missing multilingual record hits for {}",
        missing.join(", ")
    );
}

#[test]
fn search_records_merges_fts_and_scriptgram_hits_for_same_query() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let spaced = record_with_id(
        "018f45d0-0000-7000-8000-000000080003",
        "Spaced Japanese auth",
        "認証 検索 tokens remain discoverable through the default FTS tokenizer.",
    );
    let unspaced = record_with_id(
        "018f45d0-0000-7000-8000-000000080004",
        "Unspaced Japanese auth",
        "OAuth認証の検索状態を確認します。",
    );
    store.insert_record(&spaced).unwrap();
    store.insert_record(&unspaced).unwrap();

    let hits = store.search_records("認証", 10).unwrap();
    let hit_ids = hits.iter().map(|hit| hit.id).collect::<Vec<_>>();

    assert!(hit_ids.contains(&spaced.id), "missing default FTS hit");
    assert!(hit_ids.contains(&unspaced.id), "missing scriptgram hit");
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
fn multi_word_event_search_returns_partial_matches_and_orders_by_term_coverage() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let all_terms = local_preview_event(1, "sqlite rollback checksum");
    let sqlite_only = local_preview_event(2, "sqlite investigation");
    let rollback_only = local_preview_event(3, "rollback investigation");
    for event in [&sqlite_only, &all_terms, &rollback_only] {
        store.upsert_event(event).unwrap();
    }

    let first = store
        .search_event_hits("sqlite rollback checksum", 10)
        .unwrap();
    let second = store
        .search_event_hits("sqlite rollback checksum", 10)
        .unwrap();
    let ids = first.iter().map(|hit| hit.event_id).collect::<Vec<_>>();

    assert_eq!(ids.first(), Some(&all_terms.id));
    assert!(ids.contains(&sqlite_only.id));
    assert!(ids.contains(&rollback_only.id));
    assert_eq!(first, second, "multi-word ordering must be deterministic");
    assert_eq!(
        first,
        store
            .search_event_hits("sqlite SQLITE rollback ROLLBACK checksum", 10)
            .unwrap(),
        "duplicate query words must not change coverage or ordering"
    );
}

#[test]
fn mixed_script_event_search_keeps_logical_term_coverage_aligned() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let all_terms = local_preview_event(1, "OAuth認証を修正しました");
    let oauth_only = local_preview_event(2, "OAuth integration notes");
    let auth_only = local_preview_event(3, "認証を確認しました");
    for event in [&oauth_only, &auth_only, &all_terms] {
        store.upsert_event(event).unwrap();
    }

    let hits = store.search_event_hits("OAuth 認証", 10).unwrap();
    let ids = hits.iter().map(|hit| hit.event_id).collect::<Vec<_>>();
    assert_eq!(ids.first(), Some(&all_terms.id));
    assert!(ids.contains(&oauth_only.id));
    assert!(ids.contains(&auth_only.id));
}

#[test]
fn event_search_recalls_unspaced_cjk_and_mixed_script_terms() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let japanese = local_preview_event(
        1,
        "OAuth認証の検索状態を確認し、寿司APIの状態を保存します。",
    );
    let chinese = local_preview_event(2, "中文登录态异常需要重新认证并检查搜索索引。");
    let korean = local_preview_event(3, "OAuth인증오류를 재현하고 API상태를 기록했습니다.");
    let latin = local_preview_event(
        4,
        "SearchResultScope Event remains discoverable through normal Latin code tokens.",
    );
    for event in [&japanese, &chinese, &korean, &latin] {
        store.upsert_event(event).unwrap();
    }
    let sidecar_rows: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM event_search_scriptgram", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(sidecar_rows, 3);

    let mut missing = Vec::new();
    for (label, query, expected_event_id) in [
        ("japanese auth", "認証", japanese.id),
        ("japanese search", "検索", japanese.id),
        ("japanese sushi", "寿司", japanese.id),
        ("mixed oauth auth", "OAuth 認証", japanese.id),
        ("mixed api status", "API 状態", japanese.id),
        ("chinese two-char", "认证", chinese.id),
        ("chinese three-char", "登录态", chinese.id),
        ("korean particle stem", "오류", korean.id),
        ("korean auth", "인증", korean.id),
        ("latin code", "SearchResultScope", latin.id),
    ] {
        let hits = store.search_event_hits(query, 10).unwrap();
        if !hits.iter().any(|hit| hit.event_id == expected_event_id) {
            missing.push(format!("{label}: {query}"));
        }
    }

    assert!(
        missing.is_empty(),
        "missing multilingual event hits for {}",
        missing.join(", ")
    );
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

#[test]
fn semantic_embedding_documents_use_user_assistant_lite_turns() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let session_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000080001").unwrap();
    insert_session(&store, session_id);

    let user = session_event(
        1,
        session_id,
        EventType::Message,
        Some(EventRole::User),
        "How should semantic snippets work?",
    );
    let tool_call = session_event(
        2,
        session_id,
        EventType::ToolCall,
        Some(EventRole::Assistant),
        "call search_index_probe",
    );
    let tool_output = session_event(
        3,
        session_id,
        EventType::ToolOutput,
        Some(EventRole::Tool),
        "probe output should not become its own semantic document",
    );
    let assistant = session_event(
        4,
        session_id,
        EventType::Message,
        Some(EventRole::Assistant),
        "Use deterministic lite turn text for snippets.",
    );

    for event in [&user, &tool_call, &tool_output, &assistant] {
        store.upsert_event(event).unwrap();
    }

    let lookup_rows = store
        .conn
        .query_row("SELECT COUNT(*) FROM event_search_lookup", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(lookup_rows, 2);

    assert_eq!(store.count_event_embedding_documents_exact().unwrap(), 1);
    store
        .conn
        .execute(
            "DELETE FROM search_projection_stats WHERE key = 'semantic_searchable_lite_turn_items_v3'",
            [],
        )
        .unwrap();
    assert_eq!(store.cached_event_embedding_document_count().unwrap(), None);
    assert_eq!(store.count_event_embedding_documents().unwrap(), 1);
    assert_eq!(store.cached_event_embedding_document_count().unwrap(), None);
    store
        .refresh_event_embedding_document_count_cache()
        .unwrap();
    assert_eq!(store.count_event_embedding_documents().unwrap(), 1);

    let docs = store.recent_event_embedding_documents(None, 10).unwrap();
    assert_eq!(docs.len(), 1);
    let doc = &docs[0];
    assert_eq!(doc.event_id, user.id);
    assert_eq!(doc.role, Some(EventRole::User));
    assert_eq!(doc.rank_bucket, "lite_turn");
    assert!(doc
        .text
        .contains("user:\nHow should semantic snippets work?"));
    assert!(doc
        .text
        .contains("assistant:\nUse deterministic lite turn text for snippets."));
    assert!(!doc.text.contains("probe output should not become"));

    let by_ids = store
        .event_embedding_documents_by_ids(&[user.id, tool_call.id, tool_output.id, assistant.id])
        .unwrap();
    assert_eq!(
        by_ids.iter().map(|doc| doc.event_id).collect::<Vec<_>>(),
        vec![user.id]
    );

    let matching = store
        .event_embedding_documents_matching_terms(&["deterministic".to_owned()], 10)
        .unwrap();
    assert_eq!(
        matching.iter().map(|doc| doc.event_id).collect::<Vec<_>>(),
        vec![user.id]
    );

    let eligible = store
        .semantic_eligible_event_ids(&[user.id, tool_call.id, tool_output.id, assistant.id])
        .unwrap();
    assert_eq!(eligible.len(), 1);
    assert!(eligible.contains(&user.id));

    let hit_preview_chars = doc.text.chars().count();
    let hits = store
        .semantic_event_hits_by_id(&HashMap::from([(user.id, (0, hit_preview_chars))]))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_id, user.id);
    assert!(hits[0]
        .preview
        .contains("assistant:\nUse deterministic lite turn text for snippets."));
    assert!(!hits[0].preview.contains("probe output should not become"));
}

#[test]
fn semantic_lite_turn_count_ignores_payloads_without_preview_text() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let session_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000080005").unwrap();
    insert_session(&store, session_id);

    let mut user_without_preview = session_event(
        1,
        session_id,
        EventType::Message,
        Some(EventRole::User),
        "placeholder",
    );
    user_without_preview.payload = serde_json::json!({ "opaque": ["not", "previewable"] });
    let assistant_without_preview = session_event(
        2,
        session_id,
        EventType::Message,
        Some(EventRole::Assistant),
        "assistant should not leak through raw JSON fallback",
    );
    let valid_user = session_event(
        3,
        session_id,
        EventType::Message,
        Some(EventRole::User),
        "Valid semantic user prompt",
    );
    let mut opaque_assistant = session_event(
        4,
        session_id,
        EventType::Message,
        Some(EventRole::Assistant),
        "placeholder",
    );
    opaque_assistant.payload = serde_json::json!({ "opaque": ["assistant", "object"] });

    for event in [
        &user_without_preview,
        &assistant_without_preview,
        &valid_user,
        &opaque_assistant,
    ] {
        store.upsert_event(event).unwrap();
    }

    assert_eq!(store.count_event_embedding_documents_exact().unwrap(), 1);
    let docs = store.recent_event_embedding_documents(None, 10).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].event_id, valid_user.id);
    assert!(docs[0].text.contains("user:\nValid semantic user prompt"));
    assert!(!docs[0].text.contains("assistant:\n"));
    assert!(!docs[0].text.contains("opaque"));
}

#[test]
fn semantic_lite_turn_ignores_transcript_control_user_messages() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let session_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000080009").unwrap();
    insert_session(&store, session_id);

    let control_messages = [
        "<environment_context>\n  <cwd>/tmp</cwd>\n</environment_context>",
        "<turn_aborted>\nThe user interrupted the previous turn on purpose.\n</turn_aborted>",
        "<subagent_notification>\n{\"status\":\"shutdown\"}\n</subagent_notification>",
        "Warning: The maximum number of unified exec processes you can keep open is 60",
    ];
    let mut control_ids = Vec::new();
    for (index, text) in control_messages.iter().enumerate() {
        let event = session_event(
            (index + 1) as u64,
            session_id,
            EventType::Message,
            Some(EventRole::User),
            text,
        );
        control_ids.push(event.id);
        store.upsert_event(&event).unwrap();
    }

    let valid = session_event(
        10,
        session_id,
        EventType::Message,
        Some(EventRole::User),
        "Please remember this ordinary semantic user request.",
    );
    store.upsert_event(&valid).unwrap();

    assert_eq!(store.count_event_embedding_documents_exact().unwrap(), 1);
    let docs = store.recent_event_embedding_documents(None, 10).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].event_id, valid.id);

    let mut ids = control_ids;
    ids.push(valid.id);
    let eligible = store.semantic_eligible_event_ids(&ids).unwrap();
    assert_eq!(eligible.len(), 1);
    assert!(eligible.contains(&valid.id));
}

#[test]
fn semantic_lite_turn_cached_count_ignores_control_user_messages() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    store
        .refresh_event_embedding_document_count_cache()
        .unwrap();
    assert_eq!(
        store.cached_event_embedding_document_count().unwrap(),
        Some(0)
    );

    let control = local_preview_event(
        1,
        "<environment_context>\n{\"cwd\":\"/tmp\"}\n</environment_context>",
    );
    store.upsert_event(&control).unwrap();
    assert_eq!(
        store.cached_event_embedding_document_count().unwrap(),
        Some(0)
    );
    assert_eq!(store.count_event_embedding_documents_exact().unwrap(), 0);

    let mut valid = local_preview_event(2, "Real semantic indexing task.");
    store.upsert_event(&valid).unwrap();
    assert_eq!(
        store.cached_event_embedding_document_count().unwrap(),
        Some(1)
    );
    assert_eq!(store.count_event_embedding_documents_exact().unwrap(), 1);

    valid.payload = serde_json::json!({
        "text": "<subagent_notification>\n{\"status\":\"completed\"}\n</subagent_notification>",
    });
    store.upsert_event(&valid).unwrap();
    assert_eq!(
        store.cached_event_embedding_document_count().unwrap(),
        Some(0)
    );
    assert_eq!(store.count_event_embedding_documents_exact().unwrap(), 0);
}

#[test]
fn semantic_lite_turn_control_user_messages_do_not_split_turns() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let session_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000080010").unwrap();
    insert_session(&store, session_id);

    let user = session_event(
        1,
        session_id,
        EventType::Message,
        Some(EventRole::User),
        "Find the semantic indexing status bug.",
    );
    let draft = session_event(
        2,
        session_id,
        EventType::Message,
        Some(EventRole::Assistant),
        "Initial draft should be replaced.",
    );
    let control = session_event(
        3,
        session_id,
        EventType::Message,
        Some(EventRole::User),
        "<subagent_notification>\n{\"status\":\"completed\"}\n</subagent_notification>",
    );
    let final_answer = session_event(
        4,
        session_id,
        EventType::Message,
        Some(EventRole::Assistant),
        "Final answer includes the actual status cache fix.",
    );
    let next_user = session_event(
        5,
        session_id,
        EventType::Message,
        Some(EventRole::User),
        "Start the next real turn.",
    );

    for event in [&user, &draft, &control, &final_answer, &next_user] {
        store.upsert_event(event).unwrap();
    }

    let docs = store.recent_event_embedding_documents(None, 10).unwrap();
    let user_doc = docs
        .iter()
        .find(|doc| doc.event_id == user.id)
        .expect("user semantic doc");
    assert!(user_doc
        .text
        .contains("assistant:\nFinal answer includes the actual status cache fix."));
    assert!(!user_doc.text.contains("Initial draft should be replaced."));
    assert!(!docs.iter().any(|doc| doc.event_id == control.id));

    let term_docs = store
        .event_embedding_documents_matching_terms(&["actual".to_owned(), "cache".to_owned()], 10)
        .unwrap();
    assert!(term_docs.iter().any(|doc| doc.event_id == user.id));
    assert!(!term_docs.iter().any(|doc| doc.event_id == control.id));
}

#[test]
fn semantic_lite_turn_uses_last_assistant_before_next_user() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let session_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000080002").unwrap();
    insert_session(&store, session_id);

    let first_user = session_event(
        1,
        session_id,
        EventType::Message,
        Some(EventRole::User),
        "First user prompt",
    );
    let early_assistant = session_event(
        2,
        session_id,
        EventType::Message,
        Some(EventRole::Assistant),
        "early assistant draft",
    );
    let last_assistant = session_event(
        3,
        session_id,
        EventType::Message,
        Some(EventRole::Assistant),
        "last assistant before boundary",
    );
    let second_user = session_event(
        4,
        session_id,
        EventType::Message,
        Some(EventRole::User),
        "Second user prompt",
    );
    let second_assistant = session_event(
        5,
        session_id,
        EventType::Message,
        Some(EventRole::Assistant),
        "second assistant answer",
    );

    for event in [
        &first_user,
        &early_assistant,
        &last_assistant,
        &second_user,
        &second_assistant,
    ] {
        store.upsert_event(event).unwrap();
    }

    let docs = store
        .event_embedding_documents_by_ids(&[first_user.id, second_user.id])
        .unwrap();
    assert_eq!(docs.len(), 2);
    let first_doc = docs
        .iter()
        .find(|doc| doc.event_id == first_user.id)
        .unwrap();
    assert!(first_doc.text.contains("user:\nFirst user prompt"));
    assert!(first_doc
        .text
        .contains("assistant:\nlast assistant before boundary"));
    assert!(!first_doc.text.contains("early assistant draft"));
    assert!(!first_doc.text.contains("second assistant answer"));

    let second_doc = docs
        .iter()
        .find(|doc| doc.event_id == second_user.id)
        .unwrap();
    assert!(second_doc.text.contains("user:\nSecond user prompt"));
    assert!(second_doc
        .text
        .contains("assistant:\nsecond assistant answer"));
}

#[test]
fn semantic_recent_documents_order_by_lite_turn_activity() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let older_session = Uuid::parse_str("018f45d0-0000-7000-8000-000000080003").unwrap();
    let newer_session = Uuid::parse_str("018f45d0-0000-7000-8000-000000080004").unwrap();
    insert_session(&store, older_session);
    insert_session(&store, newer_session);

    let older_user = with_occurred_at(
        session_event(
            1,
            older_session,
            EventType::Message,
            Some(EventRole::User),
            "Older user prompt",
        ),
        -30,
    );
    let newer_user = with_occurred_at(
        session_event(
            2,
            newer_session,
            EventType::Message,
            Some(EventRole::User),
            "Newer user prompt without assistant yet",
        ),
        0,
    );
    let late_assistant = with_occurred_at(
        session_event(
            3,
            older_session,
            EventType::Message,
            Some(EventRole::Assistant),
            "Late assistant makes older turn active again",
        ),
        30,
    );

    for event in [&older_user, &newer_user, &late_assistant] {
        store.upsert_event(event).unwrap();
    }

    let docs = store.recent_event_embedding_documents(None, 10).unwrap();
    assert_eq!(
        docs.iter().map(|doc| doc.event_id).collect::<Vec<_>>(),
        vec![older_user.id, newer_user.id]
    );
    assert!(docs[0].text.contains("Late assistant makes older turn"));
    assert!(docs[0].occurred_at_ms > docs[1].occurred_at_ms);
}
