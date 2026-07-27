use rusqlite::params;

use super::fixtures::tempdir;
use crate::raw_sql::{RawSqlOptions, RawSqlValue};
use crate::Store;

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
