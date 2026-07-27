use std::{fs, path::Path};

use chrono::{TimeZone, Utc};
use ctx_history_core::HistoryRecord;
use rusqlite::Connection;
use serde_json::json;
use tempfile::TempDir;

use super::{
    build_codex_cold_store,
    cold_store::{
        build_codex_cold_store_with_begin_hook, build_codex_cold_store_with_hooks,
        CodexColdPromptHistoryOptions, CodexColdStoreOptions, CodexColdStoreOutcome,
    },
    import_codex_native_session_root,
};
use crate::{catalog_codex_session_tree, CodexSessionCatalogOptions, CodexSessionImportOptions};

const SUCCESS_SECRET: &str = "SUCCESS_OUTPUT_BODY_MUST_NOT_BE_STORED";

fn line(value: serde_json::Value) -> String {
    format!("{}\n", serde_json::to_string(&value).unwrap())
}

fn fixture(session_id: &str) -> String {
    [
        line(json!({
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": "2026-01-01T00:00:00Z",
                "cwd": "/workspace",
                "source": "cli"
            }
        })),
        line(json!({
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "retain this prompt"}]
            }
        })),
    ]
    .concat()
}

fn user_message(text: &str) -> String {
    line(json!({
        "timestamp": "2026-01-01T00:00:04Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": text}]
        }
    }))
}

fn authority_fixture(session_id: &str, parent_id: Option<&str>) -> String {
    let source = parent_id.map_or_else(
        || json!("cli"),
        |parent| {
            json!({
                "subagent": {
                    "thread_spawn": {
                        "parent_thread_id": parent,
                        "depth": 1,
                        "agent_nickname": "child",
                        "agent_role": "worker"
                    }
                }
            })
        },
    );
    [
        line(json!({
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": "2026-01-01T00:00:00Z",
                "cwd": "/workspace",
                "source": source
            }
        })),
        line(json!({
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "retain this prompt"}]
            }
        })),
        line(json!({
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": format!("ok-{session_id}"),
                "arguments": {"cmd": "true"}
            }
        })),
        line(json!({
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": format!("ok-{session_id}"),
                "output": format!("Process exited with code 1\n{SUCCESS_SECRET}")
            }
        })),
    ]
    .concat()
}

fn write_authority_sources(root: &Path) {
    fs::create_dir_all(root).unwrap();
    let parent = "00000000-0000-7000-8000-000000000001";
    let child = "00000000-0000-7000-8000-000000000000";
    fs::write(
        root.join("00-child.jsonl"),
        authority_fixture(child, Some(parent)),
    )
    .unwrap();
    fs::write(
        root.join("99-parent.jsonl"),
        authority_fixture(parent, None),
    )
    .unwrap();
}

fn multi_page_fixture(session_id: &str, messages: usize) -> String {
    let mut rows = vec![line(json!({
        "timestamp": "2026-01-01T00:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": session_id,
            "timestamp": "2026-01-01T00:00:00Z",
            "cwd": "/workspace",
            "source": "cli"
        }
    }))];
    rows.extend((0..messages).map(|index| {
        line(json!({
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("bounded page message {index}")
                }]
            }
        }))
    }));
    rows.concat()
}

fn skipped_only_middle_page_fixture(session_id: &str) -> String {
    let mut rows = vec![line(json!({
        "timestamp": "2026-01-01T00:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": session_id,
            "timestamp": "2026-01-01T00:00:00Z",
            "cwd": "/workspace",
            "source": "cli"
        }
    }))];
    rows.push(line(json!({
        "timestamp": "2026-01-01T00:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "retained before skipped page"}]
        }
    })));
    rows.extend((0..126).map(|index| {
        line(json!({
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": format!("result-{index}"),
                "output": "Process exited with code 0"
            }
        }))
    }));
    rows.concat()
}

fn options(temp: &TempDir) -> CodexColdStoreOptions {
    let source = temp.path().join("sessions");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("session.jsonl"),
        fixture("00000000-0000-7000-8000-000000000001"),
    )
    .unwrap();
    CodexColdStoreOptions {
        source_path: source,
        target_store_path: temp.path().join("history.sqlite"),
        machine_id: "cold-test-machine".to_owned(),
        imported_at: Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap(),
        history_record: None,
        max_source_files: None,
        max_total_bytes: None,
        prompt_history: None,
    }
}

fn owner_then_rejected(session_id: &str) -> String {
    [
        line(json!({
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": "2026-01-01T00:00:00Z",
                "cwd": "/workspace",
                "source": "cli"
            }
        })),
        "{not-json}\n".to_owned(),
    ]
    .concat()
}

#[test]
fn cold_rejection_only_session_reports_failure_without_source_authority() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("sessions");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("rejected.jsonl"),
        owner_then_rejected("00000000-0000-7000-8000-000000000099"),
    )
    .unwrap();
    let target = temp.path().join("history.sqlite");

    let outcome = build_codex_cold_store(CodexColdStoreOptions {
        source_path: source.clone(),
        target_store_path: target.clone(),
        machine_id: "cold-rejected-machine".to_owned(),
        imported_at: Utc.with_ymd_and_hms(2026, 7, 27, 12, 0, 0).unwrap(),
        history_record: None,
        max_source_files: None,
        max_total_bytes: None,
        prompt_history: None,
    })
    .unwrap();
    let CodexColdStoreOutcome::Installed { summary, store, .. } = outcome else {
        panic!("fresh target must use cold Store");
    };

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert_eq!(store.counts.sources, 0);
    assert_eq!(store.counts.capture_sources, 0);
    assert_eq!(store.counts.batches, 0);
    let connection = Connection::open(&target).unwrap();
    for table in [
        "provider_source_locators",
        "capture_sources",
        "capture_source_provider_routes",
        "sessions",
        "sync_cursors",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0,
            "{table}"
        );
    }
    drop(connection);

    let mut installed = ctx_history_store::Store::open(&target).unwrap();
    catalog_codex_session_tree(
        &source,
        &installed,
        CodexSessionCatalogOptions {
            source_root: Some(source.clone()),
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    let repeated = import_codex_native_session_root(
        &source,
        &mut installed,
        CodexSessionImportOptions {
            source_path: Some(source.clone()),
            machine_id: "cold-rejected-machine".to_owned(),
            imported_at: Utc.with_ymd_and_hms(2026, 7, 27, 12, 1, 0).unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(repeated.failed, 1);
    assert_eq!(repeated.imported_sessions, 0);
    assert_eq!(repeated.imported_events, 0);
    assert_eq!(repeated.failures.len(), 1);
    assert!(repeated.failures[0].error.contains("malformed Codex JSON"));
    let connection = Connection::open(target).unwrap();
    for table in [
        "provider_source_locators",
        "capture_sources",
        "capture_source_provider_routes",
        "sessions",
        "sync_cursors",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0,
            "repeat unexpectedly added {table} authority"
        );
    }
}

#[test]
fn cold_terminal_nul_padding_completes_catalog_and_repeats_as_authority_noop() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("sessions");
    fs::create_dir_all(&source).unwrap();
    let session_id = "00000000-0000-7000-8000-000000000098";
    let path = source.join("nul-padded.jsonl");
    let mut contents = fixture(session_id).into_bytes();
    contents.resize(contents.len().saturating_add(128 * 1024), 0);
    fs::write(&path, contents).unwrap();
    let target = temp.path().join("history.sqlite");

    let outcome = build_codex_cold_store(CodexColdStoreOptions {
        source_path: source.clone(),
        target_store_path: target.clone(),
        machine_id: "cold-nul-machine".to_owned(),
        imported_at: Utc.with_ymd_and_hms(2026, 7, 27, 12, 0, 0).unwrap(),
        history_record: None,
        max_source_files: None,
        max_total_bytes: None,
        prompt_history: None,
    })
    .unwrap();
    let CodexColdStoreOutcome::Installed { summary, store, .. } = outcome else {
        panic!("fresh target must use cold Store");
    };
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(store.counts.sources, 1);

    let connection = Connection::open(&target).unwrap();
    let authority_before = (
        connection
            .query_row("SELECT COUNT(*) FROM events", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        connection
            .query_row("SELECT COUNT(*) FROM capture_sources", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        connection
            .query_row("SELECT cursor FROM sync_cursors", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM catalog_sessions WHERE indexed_status = 'indexed'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    drop(connection);

    let mut installed = ctx_history_store::Store::open(&target).unwrap();
    catalog_codex_session_tree(
        &source,
        &installed,
        CodexSessionCatalogOptions {
            source_root: Some(source.clone()),
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    let repeated = import_codex_native_session_root(
        &source,
        &mut installed,
        CodexSessionImportOptions {
            source_path: Some(source.clone()),
            machine_id: "cold-nul-machine".to_owned(),
            imported_at: Utc.with_ymd_and_hms(2026, 7, 27, 12, 1, 0).unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(repeated.failed, 0);
    assert_eq!(repeated.imported_sessions, 0);
    assert_eq!(repeated.imported_events, 0);
    assert_eq!(repeated.skipped_sessions, 1);

    let connection = Connection::open(target).unwrap();
    let authority_after = (
        connection
            .query_row("SELECT COUNT(*) FROM events", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        connection
            .query_row("SELECT COUNT(*) FROM capture_sources", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        connection
            .query_row("SELECT cursor FROM sync_cursors", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    );
    assert_eq!(authority_after, authority_before);
}

#[test]
fn cold_ignored_prompt_history_adds_no_source_authority() {
    let temp = TempDir::new().unwrap();
    let mut cold = options(&temp);
    let prompt = temp.path().join("history.jsonl");
    fs::write(&prompt, b"  \n\t\n").unwrap();
    cold.prompt_history = Some(CodexColdPromptHistoryOptions {
        source_path: prompt,
        history_record: None,
    });
    let target = cold.target_store_path.clone();

    let outcome = build_codex_cold_store(cold).unwrap();
    let CodexColdStoreOutcome::Installed {
        prompt_history_summary: Some(prompt_summary),
        store,
        ..
    } = outcome
    else {
        panic!("fresh target must use cold Store with prompt summary");
    };

    assert_eq!(prompt_summary.imported_events, 0);
    assert_eq!(prompt_summary.failed, 0);
    assert!(!prompt_summary.has_accepted_content());
    assert_eq!(store.counts.sources, 1);
    assert_eq!(store.counts.capture_sources, 1);
    assert_eq!(store.counts.batches, 1);
    let connection = Connection::open(target).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM provider_source_locators
                 WHERE source_format = 'codex_history_jsonl'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn cold_store_uses_canonical_root_authority_catalog_fts_and_privacy() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("sessions");
    write_authority_sources(&source);
    let target = temp.path().join("history.sqlite");
    let outcome = build_codex_cold_store(CodexColdStoreOptions {
        source_path: source,
        target_store_path: target.clone(),
        machine_id: "cold-test-machine".to_owned(),
        imported_at: Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap(),
        history_record: None,
        max_source_files: None,
        max_total_bytes: None,
        prompt_history: None,
    })
    .unwrap();
    let CodexColdStoreOutcome::Installed { summary, store, .. } = outcome else {
        panic!("fresh target must use cold Store");
    };
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_edges, 1);
    assert_eq!(store.counts.sources, 2);

    let connection = Connection::open(target).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    for (sql, expected) in [
        ("SELECT COUNT(*) FROM sync_cursors", 2),
        (
            "SELECT COUNT(*) FROM provider_source_locators
             WHERE is_current = 1",
            2,
        ),
        ("SELECT COUNT(*) FROM capture_source_provider_routes", 2),
        (
            "SELECT COUNT(*) FROM catalog_sessions
             WHERE indexed_status = 'indexed' AND indexed_event_count IS NOT NULL",
            2,
        ),
        ("SELECT COUNT(*) FROM session_edges", 1),
    ] {
        assert_eq!(
            connection
                .query_row(sql, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            expected,
            "{sql}"
        );
    }
    let stored = connection
        .query_row(
            "SELECT group_concat(payload_json || metadata_json, '') FROM events",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert!(!stored.contains(SUCCESS_SECRET));
    for table in [
        "ctx_history_search",
        "event_search",
        "artifact_search",
        "ctx_history_search_scriptgram",
        "event_search_scriptgram",
    ] {
        connection
            .execute(
                &format!("INSERT INTO {table}({table}) VALUES('integrity-check')"),
                [],
            )
            .unwrap();
    }
}

#[test]
fn cold_store_combines_session_tree_and_multi_session_prompt_history() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("sessions");
    write_authority_sources(&source);
    let prompt = temp.path().join("history.jsonl");
    fs::write(
        &prompt,
        [
            line(json!({"session_id":"prompt-a","ts":1,"text":"first"})),
            line(json!({"session_id":"prompt-b","ts":2,"text":"second"})),
            line(json!({"session_id":"prompt-a","ts":3,"text":"third"})),
        ]
        .concat(),
    )
    .unwrap();
    let target = temp.path().join("history.sqlite");
    let session_record = HistoryRecord::new(
        "Codex sessions",
        "session import authority",
        Vec::new(),
        "provider_import",
        None,
    );
    let prompt_record = HistoryRecord::new(
        "Codex prompt history",
        "prompt import authority",
        Vec::new(),
        "provider_import",
        None,
    );

    let outcome = build_codex_cold_store(CodexColdStoreOptions {
        source_path: source,
        target_store_path: target.clone(),
        machine_id: "cold-combined-machine".to_owned(),
        imported_at: Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap(),
        history_record: Some(session_record),
        max_source_files: None,
        max_total_bytes: None,
        prompt_history: Some(CodexColdPromptHistoryOptions {
            source_path: prompt,
            history_record: Some(prompt_record),
        }),
    })
    .unwrap();
    let CodexColdStoreOutcome::Installed {
        catalog_summary,
        summary,
        prompt_history_summary: Some(prompt_summary),
        store,
    } = outcome
    else {
        panic!("combined first run must use the cold Store");
    };
    assert_eq!(catalog_summary.cataloged_sessions, 2);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(prompt_summary.imported_sessions, 2);
    assert_eq!(prompt_summary.imported_events, 3);
    assert_eq!(store.counts.history_records, 2);
    assert_eq!(store.counts.sources, 3);
    assert_eq!(store.counts.capture_sources, 3);
    assert_eq!(store.counts.sessions, 4);
    assert_eq!(store.counts.events, summary.imported_events + 3);
    assert!(store.counts.runs > 0);
    assert_eq!(store.deferred_index_count, 0);

    let connection = Connection::open(target).unwrap();
    for (sql, expected) in [
        ("SELECT COUNT(*) FROM history_records", 2),
        ("SELECT COUNT(*) FROM ctx_history_search", 2),
        (
            "SELECT COUNT(*) FROM provider_source_locators WHERE is_current = 1",
            3,
        ),
        ("SELECT COUNT(*) FROM sync_cursors", 3),
        ("SELECT COUNT(*) FROM sessions", 4),
    ] {
        assert_eq!(
            connection
                .query_row(sql, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            expected,
            "{sql}"
        );
    }
}

#[test]
fn cold_store_accepts_prompt_and_session_route_identity_overlap() {
    let temp = TempDir::new().unwrap();
    let session_id = "00000000-0000-7000-8000-000000000031";
    let source = temp.path().join("sessions");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("session.jsonl"), fixture(session_id)).unwrap();
    let prompt = temp.path().join("history.jsonl");
    fs::write(
        &prompt,
        line(json!({
            "session_id": session_id,
            "ts": 1,
            "text": "same canonical session from prompt history"
        })),
    )
    .unwrap();
    let target = temp.path().join("history.sqlite");

    let outcome = build_codex_cold_store(CodexColdStoreOptions {
        source_path: source,
        target_store_path: target.clone(),
        machine_id: "cold-overlap-machine".to_owned(),
        imported_at: Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap(),
        history_record: None,
        max_source_files: None,
        max_total_bytes: None,
        prompt_history: Some(CodexColdPromptHistoryOptions {
            source_path: prompt,
            history_record: None,
        }),
    })
    .unwrap();
    let CodexColdStoreOutcome::Installed {
        catalog_summary,
        summary,
        prompt_history_summary: Some(prompt_summary),
        store,
    } = outcome
    else {
        panic!("overlapping first run must use the cold Store");
    };
    assert_eq!(catalog_summary.cataloged_sessions, 1);

    let reported_sessions = summary
        .imported_sessions
        .saturating_add(prompt_summary.imported_sessions);
    let reported_events = summary
        .imported_events
        .saturating_add(prompt_summary.imported_events);
    assert!(store.counts.sessions <= reported_sessions);
    assert!(store.counts.events <= reported_events);
    assert_eq!(store.counts.sources, 2);
    assert_eq!(store.counts.capture_sources, 2);
    assert_eq!(store.counts.batches, 2);

    let connection = Connection::open(target).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM event_search", [], |row| {
                row.get::<_, usize>(0)
            })
            .unwrap(),
        store.counts.events
    );
}

#[test]
fn cold_store_folds_skipped_only_nonterminal_page_into_next_core_chunk() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("sessions");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("folded.jsonl"),
        skipped_only_middle_page_fixture("00000000-0000-7000-8000-000000000010"),
    )
    .unwrap();
    let target = temp.path().join("history.sqlite");
    let outcome = build_codex_cold_store(CodexColdStoreOptions {
        source_path: source,
        target_store_path: target.clone(),
        machine_id: "cold-test-machine".to_owned(),
        imported_at: Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap(),
        history_record: None,
        max_source_files: None,
        max_total_bytes: None,
        prompt_history: None,
    })
    .unwrap();
    let CodexColdStoreOutcome::Installed { summary, .. } = outcome else {
        panic!("fresh target must use cold Store");
    };
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);

    let connection = Connection::open(target).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sync_cursors", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
}

#[test]
fn cold_store_omits_unavailable_parent_linkage_without_dangling_authority() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("sessions");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("orphan.jsonl"),
        authority_fixture(
            "00000000-0000-7000-8000-000000000020",
            Some("00000000-0000-7000-8000-000000000099"),
        ),
    )
    .unwrap();
    let target = temp.path().join("history.sqlite");
    let outcome = build_codex_cold_store(CodexColdStoreOptions {
        source_path: source,
        target_store_path: target.clone(),
        machine_id: "cold-test-machine".to_owned(),
        imported_at: Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap(),
        history_record: None,
        max_source_files: None,
        max_total_bytes: None,
        prompt_history: None,
    })
    .unwrap();
    let CodexColdStoreOutcome::Installed { summary, .. } = outcome else {
        panic!("fresh target must use cold Store");
    };
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_edges, 0);

    let connection = Connection::open(target).unwrap();
    let (session_id, parent, root) = connection
        .query_row(
            "SELECT id, parent_session_id, root_session_id FROM sessions",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(parent, None);
    assert_eq!(root.as_deref(), Some(session_id.as_str()));
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM session_edges", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn cold_store_installs_current_nativepath_store() {
    let temp = TempDir::new().unwrap();
    let options = options(&temp);
    let outcome = build_codex_cold_store(options.clone()).unwrap();
    let CodexColdStoreOutcome::Installed { summary, store, .. } = outcome else {
        panic!("fresh absent target did not use cold Store");
    };
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(store.counts.sources, 1);
    assert_eq!(store.counts.sessions, 1);
    assert!(store.counts.events >= 1);
    assert!(options.target_store_path.is_file());
    assert_no_live_stage(&temp);
}

#[test]
fn cold_store_empty_target_requires_ordinary_writer_without_mutation() {
    let temp = TempDir::new().unwrap();
    let options = options(&temp);
    fs::File::create(&options.target_store_path).unwrap();

    let outcome = build_codex_cold_store(options.clone()).unwrap();

    assert!(matches!(
        outcome,
        CodexColdStoreOutcome::OrdinaryStoreRequired
    ));
    assert_eq!(fs::read(&options.target_store_path).unwrap(), b"");
    assert_no_live_stage(&temp);
}

#[test]
fn cold_store_nonempty_target_requires_ordinary_writer_without_mutation() {
    let temp = TempDir::new().unwrap();
    let options = options(&temp);
    fs::write(&options.target_store_path, b"existing-store-bytes").unwrap();
    let outcome = build_codex_cold_store(options.clone()).unwrap();
    assert!(matches!(
        outcome,
        CodexColdStoreOutcome::OrdinaryStoreRequired
    ));
    assert_eq!(
        fs::read(options.target_store_path).unwrap(),
        b"existing-store-bytes"
    );
}

#[test]
fn unsupported_hard_link_preflight_skips_the_corpus_loader_hook() {
    let temp = TempDir::new().unwrap();
    let mut cold = options(&temp);
    cold.source_path = temp.path().join("source-must-not-be-opened");
    let target = cold.target_store_path.clone();
    let loader_called = std::cell::Cell::new(false);

    let outcome = build_codex_cold_store_with_begin_hook(
        cold,
        |target| {
            ctx_history_store::ColdStoreBuild::begin_with_hard_link_probe(target, |_, _| {
                Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
            })
        },
        || {
            loader_called.set(true);
            Ok(())
        },
    )
    .unwrap();

    assert!(matches!(
        outcome,
        CodexColdStoreOutcome::OrdinaryStoreRequired
    ));
    assert!(!loader_called.get());
    assert!(!target.exists());
    assert_no_live_stage(&temp);
}

#[test]
fn append_after_committed_cold_capture_installs_and_next_refresh_ingests_suffix() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("sessions");
    fs::create_dir_all(&source).unwrap();
    let session_id = "00000000-0000-7000-8000-000000000040";
    let mutated = source.join("session.jsonl");
    fs::write(&mutated, fixture(session_id)).unwrap();
    let target = temp.path().join("history.sqlite");
    let outcome = build_codex_cold_store_with_hooks(
        CodexColdStoreOptions {
            source_path: source.clone(),
            target_store_path: target.clone(),
            machine_id: "cold-test-machine".to_owned(),
            imported_at: Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap(),
            history_record: None,
            max_source_files: None,
            max_total_bytes: None,
            prompt_history: None,
        },
        |_| Ok(()),
        move |_| {
            use std::io::Write;
            fs::OpenOptions::new()
                .append(true)
                .open(mutated)?
                .write_all(user_message("appended after committed cold prefix").as_bytes())?;
            Ok(())
        },
    )
    .unwrap();
    let CodexColdStoreOutcome::Installed { summary, store, .. } = outcome else {
        panic!("post-commit append must not disable the cold install");
    };
    assert_eq!(summary.imported_events, 1);
    assert_eq!(store.counts.events, 1);
    assert!(target.exists());
    assert_no_live_stage(&temp);

    let mut installed = ctx_history_store::Store::open(&target).unwrap();
    catalog_codex_session_tree(
        &source,
        &installed,
        CodexSessionCatalogOptions {
            source_root: Some(source.clone()),
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    let appended = import_codex_native_session_root(
        &source,
        &mut installed,
        CodexSessionImportOptions {
            source_path: Some(source.clone()),
            machine_id: "cold-test-machine".to_owned(),
            imported_at: Utc.with_ymd_and_hms(2026, 7, 26, 12, 1, 0).unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(appended.imported_events, 1);
    assert_eq!(
        Connection::open(target)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, usize>(0))
            .unwrap(),
        2
    );
}

#[test]
fn cold_store_pre_install_failure_leaves_target_absent() {
    let temp = TempDir::new().unwrap();
    let options = options(&temp);
    let target = options.target_store_path.clone();
    let result = build_codex_cold_store_with_hooks(
        options,
        |_| Ok(()),
        |_| {
            Err(crate::CaptureError::InvalidPayload(
                "injected pre-install failure".to_owned(),
            ))
        },
    );
    assert!(result.is_err());
    assert!(!target.exists());
    assert_no_live_stage(&temp);
}

#[test]
fn cold_store_target_race_preserves_the_new_target() {
    let temp = TempDir::new().unwrap();
    let options = options(&temp);
    let target = options.target_store_path.clone();
    let raced_target = target.clone();
    let result = build_codex_cold_store_with_hooks(
        options,
        |_| Ok(()),
        move |_| {
            fs::write(&raced_target, b"concurrent-owner")?;
            Ok(())
        },
    );
    assert!(result.is_err());
    assert_eq!(fs::read(target).unwrap(), b"concurrent-owner");
    assert_no_live_stage(&temp);
}

#[test]
fn cold_store_preserves_multi_page_terminal_cursor_authority() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("sessions");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("multi-page.jsonl"),
        multi_page_fixture("multi-page", 160),
    )
    .unwrap();
    let target = temp.path().join("history.sqlite");
    let outcome = build_codex_cold_store(CodexColdStoreOptions {
        source_path: source,
        target_store_path: target.clone(),
        machine_id: "cold-test-machine".to_owned(),
        imported_at: Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap(),
        history_record: None,
        max_source_files: None,
        max_total_bytes: None,
        prompt_history: None,
    })
    .unwrap();
    let CodexColdStoreOutcome::Installed { summary, store, .. } = outcome else {
        panic!("fresh target must use cold Store");
    };
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 160);
    assert_eq!(store.counts.sources, 1);
    assert_eq!(store.counts.batches, 1, "{:?}", store.counts);
    assert!(store.counts.groups > 1, "{:?}", store.counts);

    let connection = Connection::open(target).unwrap();
    let cursor = connection
        .query_row("SELECT cursor FROM sync_cursors", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap();
    let committed = ctx_history_store::decode_native_path_committed_cursor(&cursor).unwrap();
    assert!(!committed.publication_id().is_empty());
    assert!(committed.journal_checkpoint().is_some());
    assert!(!committed.provider_cursor().is_empty());
}

#[test]
fn cold_core_publication_defers_journal_until_one_post_load_baseline() {
    let temp = TempDir::new().unwrap();
    let options = options(&temp);
    let target = options.target_store_path.clone();
    let observed_inactive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_observation = observed_inactive.clone();
    let outcome = build_codex_cold_store_with_hooks(
        options,
        move |store| {
            assert!(store.native_cold_load_active());
            assert!(store.capture_source_count()? > 0);
            assert!(matches!(
                store.projection_journal_snapshot(None),
                Err(ctx_history_store::StoreError::ProjectionJournalInactive)
            ));
            hook_observation.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
        |_| Ok(()),
    )
    .unwrap();
    let CodexColdStoreOutcome::Installed { store, .. } = outcome else {
        panic!("fresh target must use cold Store");
    };
    assert!(observed_inactive.load(std::sync::atomic::Ordering::SeqCst));
    assert!(store.counts.groups > 0);
    assert!(store.timings.projection_journal_build > std::time::Duration::ZERO);

    let installed = ctx_history_store::Store::open_read_only(target).unwrap();
    let snapshot = installed.projection_journal_snapshot(None).unwrap();
    assert!(!snapshot.records.is_empty());
}

fn assert_no_live_stage(temp: &TempDir) {
    let live = fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".ctx-native-cold-"))
        })
        .collect::<Vec<_>>();
    assert!(live.is_empty(), "live cold stages: {live:?}");
}
