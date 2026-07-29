use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::ProviderAdapterContext;

use super::nativepath::source::{
    discover_forgecode_source, ForgeCodeDiscovery, ForgeCodeFrontier, ForgeCodeScanner,
};

const SUCCESS_SENTINEL: &str = "forgecode-success-body-must-stay-out-of-core";

#[test]
fn scanner_pages_messages_and_separates_success_output() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let mut messages = (0..18)
        .map(|index| {
            json!({
                "message": {
                    "text": {
                        "role": "user",
                        "content": format!("message-{index}")
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    messages.push(success_output(SUCCESS_SENTINEL));
    write_source(&source_path, "conversation-pages", Value::Array(messages));

    let source = live_source(&source_path);
    let mut scanner = ForgeCodeScanner::new(
        source,
        ForgeCodeFrontier::initial(),
        context(&source_path),
        true,
    )
    .unwrap();
    let mut pages = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        pages.push(page);
    }

    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].events.len(), 16);
    assert_eq!(pages[1].events.len(), 2);
    assert_eq!(pages[1].outputs.len(), 1);
    assert_eq!(pages[1].outputs[0].content, SUCCESS_SENTINEL.as_bytes());
    assert!(pages
        .iter()
        .flat_map(|page| &page.events)
        .all(|event| !event.event.payload.to_string().contains(SUCCESS_SENTINEL)));
    assert!(pages
        .iter()
        .filter_map(|page| page.row.as_ref())
        .all(|row| {
            row.context_metadata
                .as_object()
                .is_none_or(|metadata| !metadata.contains_key("messages"))
        }));
    assert!(pages.last().unwrap().terminal);
}

#[test]
fn malformed_row_is_bounded_and_does_not_hide_healthy_sibling() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let conn = Connection::open(&source_path).unwrap();
    create_schema(&conn);
    insert_row(&conn, "broken", Some("{not-json"), Some("[not-json"));
    insert_row(
        &conn,
        "healthy",
        Some(
            &json!({
                "messages": [{
                    "message": {"text": {"role": "assistant", "content": "healthy"}}
                }]
            })
            .to_string(),
        ),
        None,
    );
    drop(conn);

    let mut scanner = ForgeCodeScanner::new(
        live_source(&source_path),
        ForgeCodeFrontier::initial(),
        context(&source_path),
        false,
    )
    .unwrap();
    let first = scanner.next_page().unwrap().unwrap();
    let second = scanner.next_page().unwrap().unwrap();

    assert_eq!(first.rejections.len(), 2);
    assert!(first
        .rejections
        .iter()
        .all(|failure| failure.error.len() <= 4 * 1024));
    assert!(first.events.is_empty());
    assert_eq!(second.events.len(), 1);
    assert!(second.terminal);
}

fn context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "forgecode-nativepath-test".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: path.parent().map(Path::to_path_buf),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    }
}

fn live_source(path: &Path) -> super::nativepath::source::ForgeCodeSourceObservation {
    match discover_forgecode_source(path).unwrap() {
        ForgeCodeDiscovery::Live(source) => source,
        ForgeCodeDiscovery::Missing(_) => panic!("fixture source is missing"),
    }
}

fn write_source(path: &Path, conversation_id: &str, messages: Value) {
    let conn = Connection::open(path).unwrap();
    create_schema(&conn);
    insert_row(
        &conn,
        conversation_id,
        Some(&json!({"initiator": "forge", "messages": messages}).to_string()),
        Some(&json!({"files_accessed": ["Cargo.toml"]}).to_string()),
    );
}

fn create_schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE conversations (
            conversation_id TEXT NOT NULL,
            title TEXT,
            workspace_id INTEGER NOT NULL,
            context TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT,
            metrics TEXT
        );",
    )
    .unwrap();
}

fn insert_row(
    conn: &Connection,
    conversation_id: &str,
    context: Option<&str>,
    metrics: Option<&str>,
) {
    conn.execute(
        "INSERT INTO conversations
         (conversation_id, title, workspace_id, context, created_at, updated_at, metrics)
         VALUES (?1, 'test', 7, ?2, '2026-01-01T00:00:00Z',
                 '2026-01-01T00:00:01Z', ?3)",
        rusqlite::params![conversation_id, context, metrics],
    )
    .unwrap();
}

fn success_output(text: &str) -> Value {
    json!({
        "message": {
            "tool": {
                "name": "shell",
                "call_id": "call-success",
                "output": {
                    "is_error": false,
                    "values": [{"text": text}]
                }
            }
        }
    })
}
