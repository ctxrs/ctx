mod support;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use support::*;

fn explicit_import(temp: &TempDir, provider: &str, path: &str) -> Value {
    json_output(ctx(temp).args([
        "import",
        "--provider",
        provider,
        "--path",
        path,
        "--no-daemon",
        "--progress",
        "none",
        "--format=json",
    ]))
}

fn downgrade_certified_cursor(
    database: &Path,
    provider: &str,
    expected_parser: Option<u64>,
    expected_policy: Option<u64>,
) -> (u64, u64) {
    let connection = Connection::open(database).unwrap();
    let stream_pattern = format!("provider:{provider}:%");
    let (stream, encoded): (String, String) = connection
        .query_row(
            "SELECT stream, cursor FROM sync_cursors WHERE stream LIKE ?1",
            [&stream_pattern],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut cursor: Value = serde_json::from_str(&encoded).unwrap();
    assert!(cursor.get("parser_revision").is_none());
    assert!(cursor.get("policy_revision").is_none());
    let (parser, policy) = cursor_revisions(&cursor);
    if let Some(expected) = expected_parser {
        assert_eq!(parser, expected);
    }
    if let Some(expected) = expected_policy {
        assert_eq!(policy, expected);
    }
    if cursor.get("p").is_some() {
        assert!(parser > 1);
        cursor["p"] = json!(parser - 1);
        if expected_policy.is_some() {
            assert!(policy > 1);
            cursor["o"] = json!(policy - 1);
        }
    } else {
        let encoded_provider_cursor = cursor["provider_cursor"].as_str().unwrap();
        let mut provider_cursor: Value = serde_json::from_str(encoded_provider_cursor).unwrap();
        if provider_cursor.get("p").is_some() {
            assert!(parser > 1);
            provider_cursor["p"] = json!(parser - 1);
            if expected_policy.is_some() {
                assert!(policy > 1);
                provider_cursor["o"] = json!(policy - 1);
            }
        } else {
            assert!(policy > 1);
            for state_name in ["pending_state", "completed_state"] {
                let state = &mut provider_cursor[state_name];
                if state.is_null() {
                    continue;
                }
                assert_eq!(state["parser_revision"].as_u64(), Some(parser));
                assert_eq!(state["policy_revision"].as_u64(), Some(policy));
                state["policy_revision"] = json!(policy - 1);
            }
        }
        cursor["provider_cursor"] = Value::String(serde_json::to_string(&provider_cursor).unwrap());
    }
    connection
        .execute(
            "UPDATE sync_cursors SET cursor = ?1 WHERE stream = ?2",
            params![serde_json::to_string(&cursor).unwrap(), stream],
        )
        .unwrap();
    (parser, policy)
}

fn cursor_revisions(cursor: &Value) -> (u64, u64) {
    if let (Some(parser), Some(policy)) = (cursor["p"].as_u64(), cursor["o"].as_u64()) {
        return (parser, policy);
    }
    let provider_cursor: Value =
        serde_json::from_str(cursor["provider_cursor"].as_str().unwrap()).unwrap();
    if let (Some(parser), Some(policy)) =
        (provider_cursor["p"].as_u64(), provider_cursor["o"].as_u64())
    {
        return (parser, policy);
    }
    (
        provider_cursor["pending_state"]["parser_revision"]
            .as_u64()
            .unwrap(),
        provider_cursor["pending_state"]["policy_revision"]
            .as_u64()
            .unwrap(),
    )
}

fn projection_writer_connection(database: &Path) -> Connection {
    let connection = Connection::open(database).unwrap();
    connection
        .create_scalar_function(
            "ctx_projection_writer_authorized_v1",
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8
                | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC
                | rusqlite::functions::FunctionFlags::SQLITE_INNOCUOUS,
            |_| Ok(1_i64),
        )
        .unwrap();
    connection
}

fn certified_cursor_revisions(database: &Path, provider: &str) -> (u64, u64) {
    let connection = Connection::open(database).unwrap();
    let stream_pattern = format!("provider:{provider}:%");
    let encoded: String = connection
        .query_row(
            "SELECT cursor FROM sync_cursors WHERE stream LIKE ?1",
            [&stream_pattern],
            |row| row.get(0),
        )
        .unwrap();
    let cursor: Value = serde_json::from_str(&encoded).unwrap();
    cursor_revisions(&cursor)
}

fn replace_qwen_native_cursor_with_released_cursor(database: &Path) {
    let connection = Connection::open(database).unwrap();
    let (stream, encoded): (String, String) = connection
        .query_row(
            "SELECT stream, cursor
             FROM sync_cursors
             WHERE stream LIKE 'provider:qwen_code:%'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let envelope: Value = serde_json::from_str(&encoded).unwrap();
    let direct: Value =
        serde_json::from_str(envelope["provider_cursor"].as_str().unwrap()).unwrap();
    let checkpoint = &direct["checkpoint"];
    let source_revision: String = connection
        .query_row(
            "SELECT json_extract(metadata_json, '$.source_revision')
             FROM capture_sources
             WHERE provider = 'qwen_code'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let offset = checkpoint["complete_prefix_end"].as_u64().unwrap();
    let proof_length = u32::try_from(offset.min(64 * 1024)).unwrap();
    let mut position = Vec::with_capacity(56);
    position.extend_from_slice(b"CTXJLBP\0");
    position.extend_from_slice(&[1, 1, 0, 0]);
    position.extend_from_slice(&offset.to_be_bytes());
    position.extend_from_slice(&proof_length.to_be_bytes());
    position.extend_from_slice(&[0; 32]);
    let session = &checkpoint["session"];
    let parser_checkpoint = json!({
        "session": {
            "native_session_id": session["native_session_id"],
            "provider_session_id": session["provider_session_id"],
            "parent_provider_session_id": session["parent_provider_session_id"],
            "external_agent_id": session["external_agent_id"],
            "agent_type": session["agent_type"],
            "status": session["status"],
            "started_at": session["started_at"],
            "cwd": session["cwd"],
            "header_anchor": {
                "ordinal": 0,
                "start": 0,
                "end": 0,
                "payload_sha256": vec![0_u8; 32],
            }
        },
        "next_ordinal": checkpoint["next_raw_ordinal"],
        "accepted_captures": checkpoint["accepted_events"],
        "accepted_events": checkpoint["accepted_events"],
        "accepted_file_touches": checkpoint["accepted_file_touches"],
        "rejected_records": checkpoint["rejected_records"],
    });
    let released = json!({
        "v": 1,
        "s": source_revision,
        "p": 4,
        "o": 7,
        "k": "jsonl-byte-boundary-v1",
        "n": BASE64.encode(position),
        "c": BASE64.encode(serde_json::to_vec(&parser_checkpoint).unwrap()),
        "r": checkpoint["rejected_records"],
    });
    connection
        .execute(
            "UPDATE sync_cursors SET cursor = ?1 WHERE stream = ?2",
            params![serde_json::to_string(&released).unwrap(), stream],
        )
        .unwrap();
}

fn write_codex_session_tree(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("explicit-codex-tree");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("session.jsonl"),
        concat!(
            "{\"timestamp\":\"2026-07-18T12:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"explicit-codex-tree-upgrade\",\"timestamp\":\"2026-07-18T12:00:00Z\",\"cwd\":\"/workspace/ctx\"}}\n",
            "{\"timestamp\":\"2026-07-18T12:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"name\":\"exec\",\"call_id\":\"call_tree_revision\",\"input\":\"git commit -m fixture\"}}\n",
            "{\"timestamp\":\"2026-07-18T12:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call_output\",\"call_id\":\"call_tree_revision\",\"output\":\"[fixture db817fa] repaired tree output\"}}\n",
        ),
    )
    .unwrap();
    root
}

#[cfg(target_os = "windows")]
#[test]
fn windows_explicit_codex_file_imports_from_the_local_temp_directory() {
    let temp = tempdir();
    let source = temp.path().join("windows-local-codex-session.jsonl");
    let long_message = format!("{}END_SENTINEL", "x".repeat(20_000));
    let lines = [
        json!({
            "timestamp": "2026-07-23T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "windows-local-codex-session",
                "timestamp": "2026-07-23T12:00:00Z",
                "cwd": r"C:\workspace\ctx"
            }
        }),
        json!({
            "timestamp": "2026-07-23T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": long_message
                }]
            }
        }),
    ];
    fs::write(
        &source,
        lines
            .into_iter()
            .map(|line| serde_json::to_string(&line).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();

    let imported = explicit_import(&temp, "codex", source.to_str().unwrap());

    assert_eq!(imported["totals"]["failed_sources"], 0, "{imported:#}");
    assert_eq!(imported["totals"]["imported_events"], 1, "{imported:#}");
}

fn codex_identity_snapshot(database: &Path) -> (Vec<String>, Vec<String>) {
    let connection = Connection::open(database).unwrap();
    let sessions = connection
        .prepare("SELECT id FROM sessions WHERE provider = 'codex' ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    let cursors = connection
        .prepare(
            "SELECT stream FROM sync_cursors WHERE stream LIKE 'provider:codex:%' ORDER BY stream",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    (sessions, cursors)
}

#[test]
fn codex_automatic_and_explicit_same_spelling_share_ids_and_cursor_streams() {
    let temp = tempdir();
    let fixture = write_codex_session_tree(&temp);
    let codex_home = temp.path().join("codex-home");
    fs::create_dir_all(codex_home.join("identity-hop")).unwrap();
    fs::rename(fixture, codex_home.join("sessions")).unwrap();
    let selected_home = codex_home.join("identity-hop/..");
    let sessions = selected_home.join("sessions");

    let automatic = json_output(ctx(&temp).env("CODEX_HOME", &selected_home).args([
        "import",
        "--provider",
        "codex",
        "--no-daemon",
        "--progress",
        "none",
        "--format=json",
    ]));
    assert_eq!(automatic["totals"]["imported_events"], 1, "{automatic:#}");
    assert_eq!(automatic["totals"]["skipped_events"], 1, "{automatic:#}");
    let database = temp.path().join("work.sqlite");
    let before = codex_identity_snapshot(&database);
    assert!(!before.0.is_empty());
    assert!(!before.1.is_empty());

    let explicit = explicit_import(&temp, "codex", sessions.to_str().unwrap());
    assert_eq!(explicit["totals"]["imported_events"], 0, "{explicit:#}");
    assert_eq!(codex_identity_snapshot(&database), before);
}

#[test]
fn explicit_codex_path_repairs_a_legacy_cursor_once() {
    let temp = tempdir();
    let source = temp.path().join("explicit-codex-upgrade.jsonl");
    fs::write(
        &source,
        concat!(
            "{\"timestamp\":\"2026-07-18T12:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"explicit-codex-upgrade\",\"timestamp\":\"2026-07-18T12:00:00Z\",\"cwd\":\"/workspace/ctx\"}}\n",
            "{\"timestamp\":\"2026-07-18T12:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"name\":\"exec\",\"call_id\":\"call_mKJLNNJYU2VQYQWIubGqDwuR\",\"input\":\"git commit -m fixture\"}}\n",
            "{\"timestamp\":\"2026-07-18T12:00:02Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call_output\",\"call_id\":\"call_mKJLNNJYU2VQYQWIubGqDwuR\",\"output\":[{\"type\":\"input_text\",\"text\":\"Script completed\\nWall time 0.1 seconds\\nOutput:\\n\"},{\"type\":\"input_text\",\"text\":\"[fixture db817fa] repaired output\\n\"}]}}\n",
        ),
    )
    .unwrap();
    let source = source.to_str().unwrap();
    let first = explicit_import(&temp, "codex", source);
    assert_eq!(first["totals"]["imported_events"], 1, "{first:#}");
    assert_eq!(first["totals"]["skipped_events"], 1, "{first:#}");

    let database = temp.path().join("work.sqlite");
    let connection = projection_writer_connection(&database);
    connection
        .execute(
            "DELETE FROM events
             WHERE json_extract(payload_json, '$.body.call_id') = ?1",
            ["call_mKJLNNJYU2VQYQWIubGqDwuR"],
        )
        .unwrap();
    drop(connection);
    let current = downgrade_certified_cursor(&database, "codex", Some(8), Some(4));

    let repaired = explicit_import(&temp, "codex", source);
    assert_eq!(repaired["totals"]["imported_events"], 1, "{repaired:#}");
    assert_eq!(certified_cursor_revisions(&database, "codex"), current);
    let connection = projection_writer_connection(&database);
    let repaired_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM events
             WHERE json_extract(payload_json, '$.body.call_id') = ?1",
            ["call_mKJLNNJYU2VQYQWIubGqDwuR"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(repaired_count, 1);
    drop(connection);

    let idempotent = explicit_import(&temp, "codex", source);
    assert_eq!(idempotent["totals"]["imported_events"], 0, "{idempotent:#}");
    assert_eq!(certified_cursor_revisions(&database, "codex"), current);
}

#[test]
fn explicit_codex_tree_keeps_catalog_noop_and_revision_repair() {
    let temp = tempdir();
    let source = write_codex_session_tree(&temp);
    let source = source.to_str().unwrap();
    let first = explicit_import(&temp, "codex", source);
    assert_eq!(first["totals"]["imported_events"], 1, "{first:#}");
    assert_eq!(first["totals"]["skipped_events"], 1, "{first:#}");

    let database = temp.path().join("work.sqlite");
    let connection = Connection::open(&database).unwrap();
    let source_import_files: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE provider = 'codex'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_import_files, 0);
    drop(connection);

    let idempotent = explicit_import(&temp, "codex", source);
    assert_eq!(idempotent["totals"]["imported_events"], 0, "{idempotent:#}");

    let connection = projection_writer_connection(&database);
    connection
        .execute(
            "DELETE FROM events
             WHERE json_extract(payload_json, '$.body.call_id') = ?1",
            ["call_tree_revision"],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE catalog_sessions
             SET metadata_json = json_set(
                 metadata_json,
                 '$.normalization_capture_revision', 3,
                 '$.normalization_policy_revision', 2
             )
             WHERE provider = 'codex'",
            [],
        )
        .unwrap();
    drop(connection);
    let current = downgrade_certified_cursor(&database, "codex", Some(8), Some(4));

    let repaired = explicit_import(&temp, "codex", source);
    assert_eq!(repaired["totals"]["imported_events"], 1, "{repaired:#}");
    assert_eq!(certified_cursor_revisions(&database, "codex"), current);
    let connection = Connection::open(&database).unwrap();
    let repaired_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM events
             WHERE json_extract(payload_json, '$.body.call_id') = ?1",
            ["call_tree_revision"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(repaired_count, 1);
    let source_import_files: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM source_import_files WHERE provider = 'codex'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_import_files, 0);
}

#[test]
fn explicit_sqlite_path_reaches_the_same_revision_gate() {
    let temp = tempdir();
    let source = write_native_kilo_fixture(&temp, "explicit sqlite revision oracle");
    let first = explicit_import(&temp, "kilo", &source);
    assert_eq!(first["totals"]["imported_events"], 1, "{first:#}");

    let database = temp.path().join("work.sqlite");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "DELETE FROM events WHERE id = (
                SELECT events.id
                FROM events
                JOIN sessions ON sessions.id = events.session_id
                WHERE sessions.provider = 'kilo'
                LIMIT 1
            )",
            [],
        )
        .unwrap();
    drop(connection);
    let current = downgrade_certified_cursor(&database, "kilo", None, None);

    let repaired = explicit_import(&temp, "kilo", &source);
    assert_eq!(repaired["totals"]["imported_events"], 1, "{repaired:#}");
    assert_eq!(certified_cursor_revisions(&database, "kilo"), current);
    let connection = Connection::open(&database).unwrap();
    let repaired_count: i64 = connection
        .query_row(
            "SELECT COUNT(*)
             FROM events
             JOIN sessions ON sessions.id = events.session_id
             WHERE sessions.provider = 'kilo'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(repaired_count, 1);
    drop(connection);

    let idempotent = explicit_import(&temp, "kilo", &source);
    assert_eq!(idempotent["totals"]["imported_events"], 0, "{idempotent:#}");
    assert_eq!(certified_cursor_revisions(&database, "kilo"), current);
}

#[test]
fn explicit_qwen_unchanged_released_cursor_is_atomically_upgraded_once() {
    let temp = tempdir();
    let source = write_native_qwen_fixture(&temp, "qwen released cursor upgrade");
    let first = explicit_import(&temp, "qwen-code", &source);
    assert_eq!(first["totals"]["imported_events"], 2, "{first:#}");

    let database = temp.path().join("work.sqlite");
    replace_qwen_native_cursor_with_released_cursor(&database);
    let migrated = explicit_import(&temp, "qwen-code", &source);
    assert_eq!(migrated["totals"]["failed_sources"], 0, "{migrated:#}");
    assert_eq!(migrated["totals"]["imported_events"], 0, "{migrated:#}");

    let connection = Connection::open(&database).unwrap();
    let upgraded: String = connection
        .query_row(
            "SELECT cursor FROM sync_cursors WHERE stream LIKE 'provider:qwen_code:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let envelope: Value = serde_json::from_str(&upgraded).unwrap();
    let provider_cursor: Value =
        serde_json::from_str(envelope["provider_cursor"].as_str().unwrap()).unwrap();
    assert_eq!(provider_cursor["kind"], "direct-native-jsonl");
    assert_eq!(provider_cursor["checkpoint"]["terminal"], true);
    drop(connection);

    let idempotent = explicit_import(&temp, "qwen-code", &source);
    assert_eq!(idempotent["totals"]["failed_sources"], 0, "{idempotent:#}");
    assert_eq!(idempotent["totals"]["imported_events"], 0, "{idempotent:#}");
    let connection = Connection::open(&database).unwrap();
    let after: String = connection
        .query_row(
            "SELECT cursor FROM sync_cursors WHERE stream LIKE 'provider:qwen_code:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, upgraded);
}
