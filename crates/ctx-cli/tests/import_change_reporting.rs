mod support;

use ctx_history_core::compute_payload_hash;
use ctx_history_store::Store;
use support::*;

fn codex_source(message: &str) -> String {
    [
        json!({
            "timestamp": "2026-07-23T01:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "import-change-reporting",
                "timestamp": "2026-07-23T01:00:00Z",
                "cwd": "/workspace/project",
                "originator": "codex-cli"
            }
        }),
        json!({
            "timestamp": "2026-07-23T01:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "id": "message-one",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": message
                }]
            }
        }),
    ]
    .into_iter()
    .map(|record| format!("{}\n", serde_json::to_string(&record).unwrap()))
    .collect()
}

fn codex_prompt_history_source(message: &str) -> String {
    format!(
        "{}\n",
        json!({
            "session_id": "prompt-history-routing",
            "ts": 1_784_371_200,
            "text": message,
        })
    )
}

fn append_codex_message(path: &Path, message: &str) {
    let record = json!({
        "timestamp": "2026-07-23T01:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "id": "message-two",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": message
            }],
            "phase": "final_answer"
        }
    });
    let mut source = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(source, "{}", serde_json::to_string(&record).unwrap()).unwrap();
    source.sync_all().unwrap();
}

fn import_codex(temp: &TempDir, source: &Path) -> Value {
    let events_path = temp.path().join("analytics.jsonl");
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    json_output(
        ctx(temp)
            .arg("import")
            .args(["--provider", "codex", "--path"])
            .arg(source)
            .args(["--no-daemon", "--json", "--progress", "none"])
            .env("CTX_DATA_ROOT", &data_root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &state)
            .env("LOCALAPPDATA", &state)
            .env_remove("CTX_ANALYTICS_ENABLED")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
            .env("CTX_UPGRADE_AUTO", "off"),
    )
}

fn assert_change(report: &Value, expected: &str) {
    assert_eq!(report["outcome"], "success", "{report:#}");
    assert_eq!(report["totals"]["change"], expected, "{report:#}");
    assert_eq!(report["sources"][0]["change"], expected, "{report:#}");
}

fn rewrite_codex_event_as_legacy_fallback(data_root: &Path) -> (Value, String) {
    let connection = Connection::open(data_root.join("work.sqlite")).unwrap();
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
    let (payload_json, metadata_json, dedupe_key): (String, String, String) = connection
        .query_row(
            "SELECT payload_json, metadata_json, dedupe_key FROM events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let mut payload: Value = serde_json::from_str(&payload_json).unwrap();
    let expected_body = payload["body"].clone();
    let legacy_body = json!({"legacy_normalization": expected_body});
    let legacy_hash = compute_payload_hash(&legacy_body).unwrap();
    payload["body"] = legacy_body;
    payload["provider_event_hash"] = Value::String(legacy_hash.clone());

    let mut metadata: Value = serde_json::from_str(&metadata_json).unwrap();
    metadata
        .as_object_mut()
        .unwrap()
        .remove("provider_event_hash_authority");
    metadata["provider_event_hash"] = Value::String(legacy_hash.clone());
    let legacy_dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&dedupe_key, &legacy_hash).unwrap();

    assert_eq!(
        connection
            .execute(
                "UPDATE events
                 SET payload_json = ?1, metadata_json = ?2, dedupe_key = ?3",
                params![
                    serde_json::to_string(&payload).unwrap(),
                    serde_json::to_string(&metadata).unwrap(),
                    legacy_dedupe_key,
                ],
            )
            .unwrap(),
        1
    );
    (payload["body"]["legacy_normalization"].clone(), dedupe_key)
}

#[test]
fn import_reports_initial_noop_append_and_replacement_truthfully() {
    let temp = tempdir();
    fs::create_dir_all(temp.path().join("home")).unwrap();
    let source = temp
        .path()
        .join(".codex/sessions/2026/07/23/import-change-reporting.jsonl");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, codex_source("replacement-before")).unwrap();

    let initial = import_codex(&temp, &source);
    assert_change(&initial, "changed");
    assert_eq!(initial["totals"]["imported_events"], 1);

    let no_op = import_codex(&temp, &source);
    assert_change(&no_op, "no_op");
    assert_eq!(no_op["totals"]["imported_events"], 0);
    assert_eq!(no_op["totals"]["skipped_events"], 1);

    append_codex_message(&source, "appended-message");
    let append = import_codex(&temp, &source);
    assert_change(&append, "changed");
    assert_eq!(append["totals"]["imported_events"], 1);

    let before_replacement = fs::read_to_string(&source).unwrap();
    let after_replacement =
        before_replacement.replacen("replacement-before", "replacement-after!", 1);
    assert_ne!(before_replacement, after_replacement);
    assert_eq!(before_replacement.len(), after_replacement.len());
    fs::write(&source, after_replacement).unwrap();

    let replacement = import_codex(&temp, &source);
    assert_change(&replacement, "changed");
    assert_eq!(replacement["totals"]["imported_events"], 0);
    assert!(replacement["totals"]["skipped_events"].as_u64().unwrap() > 0);

    let stored = json_output(
        ctx(&temp)
            .args([
                "sql",
                "SELECT SUM(payload_json LIKE '%replacement-before%') AS old_hits, \
         SUM(payload_json LIKE '%replacement-after!%') AS new_hits FROM ctx_events",
                "--json",
            ])
            .env("CTX_DATA_ROOT", temp.path().join("ctx-data")),
    );
    assert_eq!(stored["rows"], json!([[0, 1]]));

    let analytics = read_analytics_events(&temp.path().join("analytics.jsonl"));
    assert_eq!(analytics.len(), 4);
    for (payload, expected) in analytics
        .iter()
        .zip(["changed", "no_op", "changed", "changed"])
    {
        assert_eq!(
            payload["events"][0]["event_name"],
            "provider_refresh_completed"
        );
        assert_eq!(payload["events"][0]["properties"]["change"], expected);
        if expected == "no_op" {
            assert_eq!(payload["events"][0]["properties"]["work_kind"], "no_op");
        } else {
            assert!(
                payload["events"][0]["properties"]
                    .get("work_kind")
                    .is_none(),
                "changed work must stay unclassified until NativePath returns an authoritative kind"
            );
        }
        assert!(payload["events"][0]["properties"]["cpu_duration_bucket"].is_string());
        assert!(payload["events"][0]["properties"]["observed_process_peak_rss_bucket"].is_string());
        assert_no_json_string_contains(payload, &[source.to_str().unwrap(), "replacement-after!"]);
    }

    let human = ctx(&temp)
        .arg("import")
        .args(["--provider", "codex", "--path"])
        .arg(&source)
        .args(["--no-daemon", "--progress", "none"])
        .env("CTX_DATA_ROOT", temp.path().join("ctx-data"))
        .output()
        .unwrap();
    assert!(human.status.success());
    let stdout = String::from_utf8(human.stdout).unwrap();
    assert!(stdout.contains("change=no_op"), "{stdout}");
    assert!(stdout.contains("change: no_op"), "{stdout}");
}

#[test]
fn codex_reimport_reconciles_pre_authority_fallback_row() {
    let temp = tempdir();
    fs::create_dir_all(temp.path().join("home")).unwrap();
    let source = temp
        .path()
        .join(".codex/sessions/2026/07/23/legacy-fallback.jsonl");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, codex_source("legacy fallback reconciliation")).unwrap();

    let initial = import_codex(&temp, &source);
    assert_change(&initial, "changed");

    let data_root = temp.path().join("ctx-data");
    let (expected_body, expected_dedupe_key) = rewrite_codex_event_as_legacy_fallback(&data_root);
    let rewritten_source = fs::read_to_string(&source).unwrap().replace(
        "\"originator\":\"codex-cli\"",
        "\"originator\":\"codex-v1\"",
    );
    fs::write(&source, rewritten_source).unwrap();

    let replay = import_codex(&temp, &source);
    assert_eq!(replay["outcome"], "success", "{replay:#}");
    assert_eq!(replay["totals"]["rejected_records"], 0, "{replay:#}");

    let connection = Connection::open(data_root.join("work.sqlite")).unwrap();
    let (payload_json, metadata_json, dedupe_key): (String, String, String) = connection
        .query_row(
            "SELECT payload_json, metadata_json, dedupe_key FROM events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let payload: Value = serde_json::from_str(&payload_json).unwrap();
    let metadata: Value = serde_json::from_str(&metadata_json).unwrap();
    assert_eq!(payload["body"], expected_body);
    assert_eq!(dedupe_key, expected_dedupe_key);
    assert_eq!(
        metadata["provider_event_hash_authority"],
        "normalized_payload_fallback"
    );
}

#[test]
fn explicit_codex_dispatch_uses_admitted_schema_for_renames_and_trees() {
    let prompt_temp = tempdir();
    fs::create_dir_all(prompt_temp.path().join("home")).unwrap();
    let prompt_named_rollout = prompt_temp.path().join("rollout-renamed.jsonl");
    fs::write(
        &prompt_named_rollout,
        codex_prompt_history_source("renamed prompt-history dispatch"),
    )
    .unwrap();
    let prompt = import_codex(&prompt_temp, &prompt_named_rollout);
    assert_eq!(prompt["outcome"], "success", "{prompt:#}");
    assert_eq!(prompt["sources"][0]["source_format"], "codex_history_jsonl");
    assert_eq!(prompt["totals"]["imported_sessions"], 1);
    assert_eq!(prompt["totals"]["imported_events"], 1);

    let rollout_temp = tempdir();
    fs::create_dir_all(rollout_temp.path().join("home")).unwrap();
    let rollout_named_history = rollout_temp.path().join("history.jsonl");
    fs::write(
        &rollout_named_history,
        codex_source("renamed rollout dispatch"),
    )
    .unwrap();
    let direct = import_codex(&rollout_temp, &rollout_named_history);
    assert_eq!(direct["outcome"], "success", "{direct:#}");
    assert_eq!(direct["sources"][0]["source_format"], "codex_session_jsonl");
    assert_eq!(direct["totals"]["imported_sessions"], 1);
    assert_eq!(direct["totals"]["imported_events"], 1);

    let tree_temp = tempdir();
    fs::create_dir_all(tree_temp.path().join("home")).unwrap();
    let tree = tree_temp.path().join("renamed-session-tree");
    fs::create_dir_all(tree.join("2026/07/23")).unwrap();
    fs::write(
        tree.join("2026/07/23/rollout.jsonl"),
        codex_source("tree dispatch"),
    )
    .unwrap();
    let tree_report = import_codex(&tree_temp, &tree);
    assert_eq!(tree_report["outcome"], "success", "{tree_report:#}");
    assert_eq!(
        tree_report["sources"][0]["source_format"],
        "codex_session_jsonl_tree"
    );
    assert_eq!(
        tree_report["totals"]["imported_sessions"],
        direct["totals"]["imported_sessions"]
    );
    assert_eq!(
        tree_report["totals"]["imported_events"],
        direct["totals"]["imported_events"]
    );
}

#[test]
fn explicit_codex_dispatch_rejects_ambiguous_first_record() {
    let temp = tempdir();
    let source = temp.path().join("ambiguous.jsonl");
    fs::write(
        &source,
        concat!(
            r#"{"session_id":"both","ts":1784371200,"text":"both","timestamp":"2026-07-21T00:00:00Z","type":"session_meta","payload":{}}"#,
            "\n"
        ),
    )
    .unwrap();

    ctx(&temp)
        .arg("import")
        .args(["--provider", "codex", "--path"])
        .arg(&source)
        .args(["--no-daemon", "--json", "--progress", "none"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("schema is ambiguous"));
}
