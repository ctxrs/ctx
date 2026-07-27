use chrono::{DateTime, Utc};
use ctx_history_core::{Event, EventRole, EventType, SyncMetadata};
use rusqlite::params;
use serde_json::json;
use uuid::Uuid;

use super::fixtures::tempdir;
use crate::events::ProviderEventHashAuthority;
use crate::raw_sql::{RawSqlOptions, RawSqlValue};
use crate::{Store, FINAL_SCHEMA_IDENTITY, SCHEMA_VERSION};

#[test]
fn raw_sql_query_reads_stable_views() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let schema = store.schema().unwrap();
    for view in [
        "CREATE VIEW ctx_sessions",
        "CREATE VIEW ctx_events",
        "CREATE VIEW ctx_files_touched",
        "CREATE VIEW ctx_sources",
    ] {
        assert!(schema.contains(view), "schema missing {view}");
    }

    let result = store
        .raw_sql_query(
            "SELECT COUNT(*) AS session_count FROM ctx_sessions",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(result.columns[0].name, "session_count");
    assert_eq!(result.returned_rows, 1);
    assert_eq!(result.rows[0][0], RawSqlValue::Integer(0));
}

#[test]
fn v47_stable_events_expose_sparse_failure_without_transitional_result_schema() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let now: DateTime<Utc> = "2026-07-23T00:00:00Z".parse().unwrap();
    let output = |id, seq, event_type, hash, body| Event {
        id,
        seq,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type,
        role: Some(EventRole::Tool),
        occurred_at: now,
        capture_source_id: None,
        payload: json!({"body": body}),
        payload_blob_id: None,
        dedupe_key: Some(Store::provider_source_event_dedupe_key(
            Uuid::nil(),
            seq,
            hash,
        )),
        sync: SyncMetadata::default(),
    };
    let success = output(
        Uuid::new_v4(),
        1,
        EventType::ToolOutput,
        "success-output",
        json!({
            "result_outcome": "success",
            "output_preview": "successful output must be absent"
        }),
    );
    let failure = output(
        Uuid::new_v4(),
        2,
        EventType::CommandOutput,
        "failure-output",
        json!({
            "result_outcome": "failure",
            "exit_code": 1,
            "output_preview": "retained sparse failure"
        }),
    );

    assert!(!store
        .reconcile_provider_event(&success, ProviderEventHashAuthority::ProviderSupplied)
        .unwrap());
    assert!(store
        .reconcile_provider_event(&failure, ProviderEventHashAuthority::ProviderSupplied)
        .unwrap());

    let (version, identity): (i64, String) = store
        .conn
        .query_row(
            "SELECT (SELECT user_version FROM pragma_user_version),
                    schema_identity
             FROM ctx_store_schema_identity WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(identity, FINAL_SCHEMA_IDENTITY);
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE name = 'result_observations'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );

    let rows = store
        .raw_sql_query(
            "SELECT ctx_event_id, event_type, payload_json FROM ctx_events",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(rows.returned_rows, 1);
    assert_eq!(
        rows.rows[0][0],
        RawSqlValue::Text {
            value: failure.id.to_string(),
            bytes: 36,
            truncated: false,
        }
    );
    assert_eq!(
        rows.rows[0][1],
        RawSqlValue::Text {
            value: "command_output".to_owned(),
            bytes: "command_output".len(),
            truncated: false,
        }
    );
    let RawSqlValue::Text { value, .. } = &rows.rows[0][2] else {
        panic!("failure payload is not text");
    };
    assert!(value.contains("\"result_outcome\":\"failure\""));
    assert!(value.contains("\"exit_code\":1"));
    assert!(!value.contains("retained sparse failure"));
    assert!(!value.contains("successful output must be absent"));
}

#[test]
fn ctx_files_touched_resolves_session_from_source_id() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let record_id = "018f45d0-0000-7000-8000-000000080001";
    let source_id = "018f45d0-0000-7000-8000-000000080002";
    let session_id = "018f45d0-0000-7000-8000-000000080003";
    let touch_id = "018f45d0-0000-7000-8000-000000080004";
    let detached_source_id = "018f45d0-0000-7000-8000-000000080005";
    let detached_touch_id = "018f45d0-0000-7000-8000-000000080006";

    store
        .conn
        .execute(
            r#"
            INSERT INTO history_records
            (id, title, last_activity_at_ms, created_at_ms, updated_at_ms, body, created_at, updated_at)
            VALUES (?1, 'Touched file view record', 1, 1, 1, '', '', '')
            "#,
            [record_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, raw_source_path, external_session_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'codex', 'test-machine', '/tmp/session.jsonl', 'codex-session-1', 1, 'imported')
            "#,
            [source_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO capture_sources
            (id, kind, provider, machine_id, raw_source_path, external_session_id, started_at_ms, fidelity)
            VALUES (?1, 'provider_import', 'opencode', 'test-machine', '/tmp/opencode.db', 'opencode-session-1', 1, 'imported')
            "#,
            [detached_source_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO sessions
            (
                id, history_record_id, capture_source_id, provider, external_session_id,
                agent_type, is_primary, status, fidelity, started_at_ms, created_at_ms, updated_at_ms
            )
            VALUES (?1, ?2, ?3, 'codex', 'codex-session-1', 'primary', 1, 'imported', 'imported', 1, 1, 1)
            "#,
            params![session_id, record_id, source_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO files_touched
            (id, source_id, path, change_kind, confidence, created_at_ms, updated_at_ms, fidelity)
            VALUES (?1, ?2, 'src/main.rs', 'modified', 'explicit', 1, 1, 'imported')
            "#,
            params![touch_id, source_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            r#"
            INSERT INTO files_touched
            (id, source_id, path, change_kind, confidence, created_at_ms, updated_at_ms, fidelity)
            VALUES (?1, ?2, 'detached.rs', 'modified', 'explicit', 1, 1, 'imported')
            "#,
            params![detached_touch_id, detached_source_id],
        )
        .unwrap();

    let result = store
        .raw_sql_query(
            "SELECT provider, provider_session_id, ctx_session_id, history_record_id FROM ctx_files_touched WHERE path = 'src/main.rs'",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(result.returned_rows, 1);
    assert_eq!(
        result.rows[0][0],
        RawSqlValue::Text {
            value: "codex".to_owned(),
            bytes: 5,
            truncated: false,
        }
    );
    assert_eq!(
        result.rows[0][1],
        RawSqlValue::Text {
            value: "codex-session-1".to_owned(),
            bytes: 15,
            truncated: false,
        }
    );
    assert_eq!(
        result.rows[0][2],
        RawSqlValue::Text {
            value: session_id.to_owned(),
            bytes: session_id.len(),
            truncated: false,
        }
    );
    assert_eq!(
        result.rows[0][3],
        RawSqlValue::Text {
            value: record_id.to_owned(),
            bytes: record_id.len(),
            truncated: false,
        }
    );

    let detached = store
        .raw_sql_query(
            "SELECT provider, provider_session_id, ctx_session_id, history_record_id FROM ctx_files_touched WHERE path = 'detached.rs'",
            RawSqlOptions::default(),
        )
        .unwrap();
    assert_eq!(detached.returned_rows, 1);
    assert_eq!(
        detached.rows[0][0],
        RawSqlValue::Text {
            value: "opencode".to_owned(),
            bytes: 8,
            truncated: false,
        }
    );
    assert_eq!(
        detached.rows[0][1],
        RawSqlValue::Text {
            value: "opencode-session-1".to_owned(),
            bytes: 18,
            truncated: false,
        }
    );
    assert_eq!(detached.rows[0][2], RawSqlValue::Null);
    assert_eq!(detached.rows[0][3], RawSqlValue::Null);
}
