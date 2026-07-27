mod support;

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
