mod support;

use support::*;

const COMPLETE_MESSAGE_PREFIX_CHARS: usize = 16_000;

fn import_and_find_long_message(
    temp: &TempDir,
    provider: &str,
    path: &Path,
    query: &str,
) -> (String, Value) {
    let imported = json_output(ctx(temp).args([
        "import",
        "--provider",
        provider,
        "--path",
        path.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["totals"]["rejected_records"], 0, "{imported:#}");
    let search = json_output(ctx(temp).args([
        "search",
        query,
        "--provider",
        provider,
        "--events",
        "--refresh",
        "off",
        "--json",
    ]));
    let result = search["results"]
        .as_array()
        .and_then(|results| results.first())
        .unwrap_or_else(|| panic!("missing {provider} long-message result: {search:#}"));
    (
        result["ctx_event_id"].as_str().unwrap().to_owned(),
        imported,
    )
}

fn assert_complete_show(temp: &TempDir, event_id: &str, expected: &str) {
    let shown = json_output(ctx(temp).args([
        "show",
        "event",
        event_id,
        "--content",
        "complete",
        "--format",
        "json",
    ]));
    assert_eq!(shown["event"]["text"], expected);
    assert_eq!(shown["event"]["content"]["complete"], true);
    assert_eq!(shown["event"]["content"]["origin"], "provider_source");
    assert_eq!(shown["event"]["content"]["source_verified"], true);
}

fn assert_complete_show_error(
    temp: &TempDir,
    event_id: &str,
    source: &Path,
    private_tail: &str,
    expected_code: &str,
) {
    let output = ctx(temp)
        .args([
            "show",
            "event",
            event_id,
            "--content",
            "complete",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error_code"], expected_code, "{error:#}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains(private_tail));
    assert!(!stderr.contains(source.to_str().unwrap()));
}

#[test]
fn codex_canonical_truncation_hydrates_and_fails_closed() {
    let temp = tempdir();
    let query = "codex-complete-message-oracle";
    let end = "CTX_HYDRATE_SENTINEL_END";
    let complete = format!(
        "{query}-{}-{end}",
        "x".repeat(COMPLETE_MESSAGE_PREFIX_CHARS)
    );
    let transcript = temp.path().join("codex-long-session.jsonl");
    let header = json!({
        "timestamp": "2026-07-22T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": "codex-complete-session",
            "timestamp": "2026-07-22T12:00:00Z",
            "cwd": "/workspace/codex",
            "originator": "codex-cli",
        },
    });
    let message = json!({
        "timestamp": "2026-07-22T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "phase": "final_answer",
            "content": [{"type": "output_text", "text": complete}],
        },
    });
    let original = format!(
        "{}\n{}\n",
        serde_json::to_string(&header).unwrap(),
        serde_json::to_string(&message).unwrap()
    );
    fs::write(&transcript, &original).unwrap();
    let (event_id, _) = import_and_find_long_message(&temp, "codex", &transcript, query);

    let indexed = json_output(ctx(&temp).args([
        "show",
        "event",
        &event_id,
        "--content",
        "indexed",
        "--format",
        "json",
    ]));
    assert_eq!(indexed["event"]["content"]["complete"], false);
    assert_eq!(indexed["event"]["content"]["stored_truncated"], true);
    assert_eq!(
        indexed["event"]["text"].as_str().unwrap().chars().count(),
        COMPLETE_MESSAGE_PREFIX_CHARS
    );
    assert!(!indexed["event"]["text"].as_str().unwrap().contains(end));

    assert_complete_show(&temp, &event_id, &complete);
    assert!(
        json_output(ctx(&temp).args(["locate", "event", &event_id, "--format", "json"]))
            ["complete_content"]["available"]
            .as_bool()
            .unwrap()
    );
    let text_output = ctx(&temp)
        .args(["show", "event", &event_id, "--content", "complete"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8(text_output).unwrap().contains(end));

    let changed = original.replacen(query, "zodex-complete-message-oracle", 1);
    assert_eq!(changed.len(), original.len());
    fs::write(&transcript, changed).unwrap();
    let output = ctx(&temp)
        .args([
            "show",
            "event",
            &event_id,
            "--content",
            "complete",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error_code"], "source_changed", "{error:#}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains(transcript.to_str().unwrap()));

    fs::remove_file(&transcript).unwrap();
    let output = ctx(&temp)
        .args([
            "show",
            "event",
            &event_id,
            "--content",
            "complete",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error_code"], "source_missing", "{error:#}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains(transcript.to_str().unwrap()));
}

#[test]
fn pi_long_message_reopens_after_append_without_storing_the_tail() {
    let temp = tempdir();
    let query = "pi-complete-message-oracle";
    let tail = "pi-private-unindexed-tail";
    let complete = format!(
        "{query} {}{tail}",
        "x".repeat(COMPLETE_MESSAGE_PREFIX_CHARS)
    );
    let transcript = temp.path().join("pi-long-session.jsonl");
    let header = json!({
        "type": "session",
        "id": "pi-complete-session",
        "timestamp": "2026-07-22T12:00:00Z",
        "cwd": "/workspace/pi",
    });
    let message = json!({
        "type": "message",
        "id": "pi-complete-message",
        "parentId": null,
        "timestamp": "2026-07-22T12:00:01Z",
        "message": {"role": "user", "content": [{"type": "text", "text": complete}]},
    });
    let original = format!(
        "{}\n{}\n",
        serde_json::to_string(&header).unwrap(),
        serde_json::to_string(&message).unwrap()
    );
    fs::write(&transcript, &original).unwrap();
    let (event_id, _) = import_and_find_long_message(&temp, "pi", &transcript, query);

    let database = Connection::open(temp.path().join("work.sqlite")).unwrap();
    let (payload, metadata): (String, String) = database
        .query_row(
            "select payload_json, metadata_json from events where id = ?1",
            [&event_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(!payload.contains(tail));
    assert!(!metadata.contains(tail));
    assert!(!metadata.contains(transcript.to_str().unwrap()));
    assert!(metadata.contains("pi-jsonl.message-body.v1"));
    assert!(metadata.contains("pi-complete-message"));
    drop(database);

    let appended = json!({
        "type": "custom",
        "id": "pi-later-notice",
        "timestamp": "2026-07-22T12:00:02Z",
        "customType": "appended-after-import",
    });
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(&appended).unwrap()).unwrap();
    drop(file);

    assert_complete_show(&temp, &event_id, &complete);

    let appended_source = format!("{original}{}\n", serde_json::to_string(&appended).unwrap());
    let rewritten = appended_source.replacen(query, "qi-complete-message-oracle", 1);
    assert_eq!(rewritten.len(), appended_source.len());
    fs::write(&transcript, rewritten).unwrap();
    assert_complete_show_error(&temp, &event_id, &transcript, tail, "source_changed");

    fs::remove_file(&transcript).unwrap();
    assert_complete_show_error(&temp, &event_id, &transcript, tail, "source_missing");
}

#[test]
fn claude_long_message_reopens_and_rejects_historical_rewrite() {
    let temp = tempdir();
    let query = "claude-complete-message-oracle";
    let tail = "claude-private-unindexed-tail";
    let complete = format!(
        "{query} {}{tail}",
        "y".repeat(COMPLETE_MESSAGE_PREFIX_CHARS)
    );
    let root = temp.path().join("claude-projects");
    fs::create_dir_all(&root).unwrap();
    let transcript = root.join("claude-complete-session.jsonl");
    let message = json!({
        "type": "user",
        "sessionId": "claude-complete-session",
        "timestamp": "2026-07-22T12:00:01Z",
        "cwd": "/workspace/claude",
        "message": {
            "id": "claude-complete-message-id",
            "role": "user",
            "content": [{"type": "text", "text": complete}]
        },
    });
    let original = format!("{}\n", serde_json::to_string(&message).unwrap());
    fs::write(&transcript, &original).unwrap();
    let (event_id, _) = import_and_find_long_message(&temp, "claude", &root, query);

    let database = Connection::open(temp.path().join("work.sqlite")).unwrap();
    let (payload, metadata): (String, String) = database
        .query_row(
            "select payload_json, metadata_json from events where id = ?1",
            [&event_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(!payload.contains(tail));
    assert!(!metadata.contains(tail));
    assert!(!metadata.contains(transcript.to_str().unwrap()));
    assert!(payload.contains("\"truncated\":true"));
    assert!(metadata.contains("claude-jsonl.message-body.v1"));
    assert!(metadata.contains("claude-complete-message-id"));
    drop(database);

    let indexed = json_output(ctx(&temp).args([
        "show",
        "event",
        &event_id,
        "--content",
        "indexed",
        "--format",
        "json",
    ]));
    assert_eq!(indexed["event"]["content"]["complete"], false);
    assert_eq!(indexed["event"]["content"]["stored_truncated"], true);
    assert_eq!(
        indexed["event"]["text"].as_str().unwrap().chars().count(),
        COMPLETE_MESSAGE_PREFIX_CHARS
    );
    assert!(!indexed["event"]["text"].as_str().unwrap().contains(tail));

    let tail_search = json_output(ctx(&temp).args([
        "search",
        tail,
        "--provider",
        "claude",
        "--events",
        "--refresh",
        "off",
        "--json",
    ]));
    assert!(
        tail_search["results"].as_array().unwrap().is_empty(),
        "{tail_search:#}"
    );

    assert_complete_show(&temp, &event_id, &complete);

    let changed = original.replacen(query, "claudz-complete-message-oracle", 1);
    assert_eq!(changed.len(), original.len());
    fs::write(&transcript, changed).unwrap();
    let output = ctx(&temp)
        .args([
            "show",
            "event",
            &event_id,
            "--content",
            "complete",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error_code"], "source_changed", "{error:#}");
}

#[test]
fn codebuddy_cli_jsonl_imports_and_searches_through_public_cli() {
    let temp = tempdir();
    let query = "codebuddy-cli-real-shape-oracle";
    let path = write_native_codebuddy_cli_jsonl_fixture(&temp, query);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codebuddy",
        "--path",
        &path,
        "--json",
    ]));
    assert_eq!(imported["schema_version"], 2);
    assert_eq!(imported["sources"][0]["provider"], "codebuddy");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "codebuddy_history_json"
    );
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "codebuddy",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "codebuddy", query, 1, "message");
}

#[test]
fn codebuddy_cli_complete_content_hydrates_through_cli_and_mcp() {
    let temp = tempdir();
    let query = "codebuddy-complete-content-oracle";
    let end = "CODEBUDDY_COMPLETE_SENTINEL";
    let complete = format!(
        "{query}-{}-{end}",
        "x".repeat(COMPLETE_MESSAGE_PREFIX_CHARS)
    );
    let root = temp
        .path()
        .join("codebuddy-complete/.codebuddy/projects/project-hash");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("complete-session.jsonl");
    fs::write(
        &path,
        format!(
            "{}\n",
            json!({
                "id": "codebuddy-complete-message",
                "timestamp": 1783170001000i64,
                "type": "message",
                "role": "assistant",
                "content": complete,
                "sessionId": "complete-session",
                "cwd": "/workspace/codebuddy",
            })
        ),
    )
    .unwrap();
    let source_root = temp.path().join("codebuddy-complete/.codebuddy");
    let (event_id, _) = import_and_find_long_message(&temp, "codebuddy", &source_root, query);

    assert_complete_show(&temp, &event_id, &complete);
    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "show",
                "method": "tools/call",
                "params": {
                    "name": "show_event",
                    "arguments": {
                        "ctx_event_id": event_id,
                        "content": "complete"
                    }
                }
            }),
        ],
    );
    let shown = &responses[1]["result"]["structuredContent"];
    assert_eq!(shown["content_policy"], "complete");
    assert_eq!(shown["event"]["text"], complete);
    assert_eq!(shown["event"]["content"]["complete"], true);
    assert_eq!(shown["event"]["content"]["source_verified"], true);
    assert!(shown["event"]["text"].as_str().unwrap().contains(end));
}

#[test]
fn codebuddy_extension_complete_content_uses_only_the_locator_message_path() {
    let temp = tempdir();
    let query = "codebuddy-extension-complete-oracle";
    let complete = format!(
        "{query}-{}-CODEBUDDY_EXTENSION_SENTINEL",
        "x".repeat(COMPLETE_MESSAGE_PREFIX_CHARS)
    );
    let root = temp.path().join("codebuddy-extension");
    let project = root.join("history/project-hash");
    let session = project.join("extension-session");
    let messages = session.join("messages");
    fs::create_dir_all(&messages).unwrap();
    fs::write(
        project.join("index.json"),
        json!({
            "conversations": [{
                "id": "extension-session",
                "name": "Complete extension session",
                "createdAt": "2026-07-25T10:00:00Z"
            }]
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        session.join("index.json"),
        json!({
            "messages": [{"id": "message-1", "role": "assistant", "type": "message"}]
        })
        .to_string(),
    )
    .unwrap();
    let message_path = messages.join("message-1.json");
    let original = json!({
        "id": "message-1",
        "role": "assistant",
        "content": complete,
        "createdAt": "2026-07-25T10:00:01Z"
    })
    .to_string();
    fs::write(&message_path, &original).unwrap();
    fs::write(session.join("message-1.backup.json"), &original).unwrap();
    let oversized = messages.join("unreferenced-oversized.json");
    fs::File::create(&oversized)
        .unwrap()
        .set_len(65 * 1024 * 1024)
        .unwrap();

    let (event_id, _) = import_and_find_long_message(&temp, "codebuddy", &root, query);
    assert_complete_show(&temp, &event_id, &complete);

    fs::write(
        &message_path,
        original.replacen(query, "codebuddy-extension-mutated-oracle", 1),
    )
    .unwrap();
    let output = ctx(&temp)
        .args([
            "show",
            "event",
            &event_id,
            "--content",
            "complete",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error_code"], "source_changed", "{error:#}");
}

#[test]
fn nanoclaw_import_preserves_text_timestamp_millis_and_integer_trigger() {
    let temp = tempdir();
    let query = "nanoclaw-real-text-timestamp-oracle";
    let path = write_native_nanoclaw_fixture(&temp, query);

    let central = Connection::open(Path::new(&path).join("data/v2.db")).unwrap();
    central
        .execute_batch(
            "update sessions
             set created_at = '2026-07-10T03:18:34.491Z',
                 last_active = '2026-07-10 03:19:51'",
        )
        .unwrap();
    let inbound = Connection::open(
        Path::new(&path)
            .join("data/v2-sessions/ag-1/session-1")
            .join("inbound.db"),
    )
    .unwrap();
    inbound
        .execute(
            "update messages_in set timestamp = ?1, trigger = 1 where id = 'in-1'",
            ["2026-07-10T03:18:34.491Z"],
        )
        .unwrap();
    let outbound = Connection::open(
        Path::new(&path)
            .join("data/v2-sessions/ag-1/session-1")
            .join("outbound.db"),
    )
    .unwrap();
    outbound
        .execute(
            "update messages_out set timestamp = ?1 where id = 'out-1'",
            ["2026-07-10 03:19:51"],
        )
        .unwrap();

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "nanoclaw",
        "--path",
        &path,
        "--json",
    ]));
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let store = Connection::open(temp.path().join("work.sqlite")).unwrap();
    let (occurred_at_ms, payload_json): (i64, String) = store
        .query_row(
            "select occurred_at_ms, payload_json
             from events
             where json_extract(metadata_json, '$.metadata.message_id') = 'in-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(occurred_at_ms, 1_783_653_514_491);
    let payload: Value = serde_json::from_str(&payload_json).unwrap();
    let body: Value =
        serde_json::from_str(payload["body"]["body"]["json"].as_str().unwrap()).unwrap();
    assert_eq!(body["trigger"], "1");

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "nanoclaw",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "nanoclaw", query, 1, "message");
}

#[test]
fn nanoclaw_complete_content_reopens_the_brokered_compound_snapshot() {
    let temp = tempdir();
    let needle = "nanoclaw-complete-content-broker-oracle";
    let complete_text = format!("{needle}-{}", "x".repeat(17_000));
    let path = write_native_nanoclaw_fixture(&temp, &complete_text);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "nanoclaw",
        "--path",
        &path,
        "--json",
    ]));
    assert_eq!(imported["totals"]["rejected_records"], 0);

    let search = json_output(ctx(&temp).args([
        "search",
        needle,
        "--provider",
        "nanoclaw",
        "--refresh",
        "off",
        "--json",
    ]));
    let event_id = search["results"][0]["ctx_event_id"].as_str().unwrap();
    let shown = json_output(ctx(&temp).args([
        "show",
        "event",
        event_id,
        "--content",
        "complete",
        "--json",
    ]));

    assert_eq!(
        shown["event"]["text"],
        json!({"text": complete_text}).to_string()
    );
    assert_eq!(shown["event"]["content"]["origin"], "provider_source");
    assert_eq!(shown["event"]["content"]["source_verified"], true);
    assert_eq!(shown["event"]["content"]["complete"], true);

    let inbound = Path::new(&path)
        .join("data/v2-sessions/ag-1/session-1")
        .join("inbound.db");
    Connection::open(&inbound)
        .unwrap()
        .execute(
            "update messages_in set content = replace(content, ?1, ?2) where id = 'in-1'",
            [needle, "nanoclaw-complete-content-mutated-oracle"],
        )
        .unwrap();
    assert_complete_show_error(
        &temp,
        event_id,
        Path::new(&path),
        &complete_text,
        "content_verification_failed",
    );
}
