#[path = "../support/mod.rs"]
mod support;

use support::*;

fn write_durable_plugin(temp: &TempDir) -> (PathBuf, PathBuf) {
    let provider_root = temp.path().join("provider-owned");
    fs::create_dir_all(&provider_root).unwrap();
    let source_path = provider_root.join("history.jsonl");
    let records = [
        json!({"record_type":"manifest","schema_version":"ctx-history-jsonl-v2"}),
        json!({
            "record_type":"source",
            "source_id":"other",
            "provider_key":"other-agent",
            "source_format":"other-agent-v1"
        }),
        json!({
            "record_type":"source",
            "source_id":"archive",
            "provider_key":"example-agent",
            "source_format":"example-agent-v1"
        }),
        json!({
            "record_type":"session",
            "source_id":"archive",
            "provider_session_id":"session-1",
            "started_at":"2026-07-30T04:00:00Z",
            "agent_scope":"primary",
            "status":"completed"
        }),
        json!({
            "record_type":"event",
            "source_id":"archive",
            "provider_session_id":"session-1",
            "event_index":0,
            "event_id":"event-1",
            "event_type":"message",
            "role":"assistant",
            "occurred_at":"2026-07-30T04:00:01Z",
            "payload":{"text":"provider-owned durable plugin oracle"}
        }),
    ];
    let body = records
        .iter()
        .map(|record| format!("{record}\n"))
        .collect::<String>();
    fs::write(&source_path, body).unwrap();

    let manifest_dir = temp.path().join("history-plugins/example-agent");
    fs::create_dir_all(&manifest_dir).unwrap();
    fs::write(
        manifest_dir.join("ctx-history-plugin.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version":1,
            "name":"example-plugin",
            "history_sources":[{
                "id":"primary",
                "provider_key":"example-agent",
                "source_id":"archive",
                "source_format":"example-agent-v1",
                "path":source_path
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    (manifest_dir, source_path)
}

#[test]
fn command_only_plugin_is_discoverable_but_typed_unsupported_and_never_runs() {
    let temp = tempdir();
    let plugin =
        write_history_source_plugin_with_refresh(&temp, "hermes", true, Some("auto"), None);

    let human = ctx(&temp)
        .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
        .args(["sources"])
        .output()
        .unwrap();
    assert!(human.status.success(), "{human:#?}");
    let human_stdout = String::from_utf8(human.stdout).unwrap();
    assert!(
        human_stdout.starts_with("! No importable history sources found\n"),
        "{human_stdout}"
    );
    assert!(human_stdout.contains("unsupported"), "{human_stdout}");
    assert!(
        human_stdout.contains("no durable provider path"),
        "{human_stdout}"
    );
    assert!(
        human_stdout.contains("command stdout is not a provider-owned durable source"),
        "{human_stdout}"
    );
    assert!(
        !human_stdout.contains("history source is ready"),
        "{human_stdout}"
    );

    let sources = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args(["sources", "--format=json"]),
    );
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["history_source"] == "hermes/default")
        .unwrap();
    assert_eq!(source["status"], "unsupported", "{source:#}");
    assert_eq!(source["importable"], false, "{source:#}");
    assert_eq!(source["provider_source_authority"], false, "{source:#}");
    assert!(source["unsupported_reason"]
        .as_str()
        .unwrap()
        .contains("command stdout is not a provider-owned durable source"));

    let error = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "hermes/default",
                "--no-daemon",
                "--progress",
                "none",
            ]),
    );
    assert!(error.contains("command-only history source plugins are unsupported"));
    assert!(!plugin.run_marker.exists());
    assert!(!data_root(&temp)
        .join("history-source-plugin-sources")
        .exists());
}

#[test]
fn durable_plugin_path_indexes_in_place_and_renders_complete_core_content() {
    let temp = tempdir();
    let (manifest_dir, source_path) = write_durable_plugin(&temp);
    let sources = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &manifest_dir)
            .args(["sources", "--format=json"]),
    );
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["history_source"] == "example-agent/archive")
        .unwrap();
    assert_eq!(source["status"], "available", "{source:#}");
    assert_eq!(source["importable"], true, "{source:#}");
    assert_eq!(source["path"], json!(source_path), "{source:#}");
    assert_eq!(source["provider_source_authority"], true, "{source:#}");

    let state = temp.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let _daemon = start_source_refresh_daemon(&temp, &data_root(&temp), temp.path(), &state);

    let imported = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &manifest_dir)
            .args([
                "import",
                "--history-source",
                "example-plugin/primary",
                "--no-daemon",
                "--progress",
                "none",
                "--format=json",
            ]),
    );
    let receipt = assert_explicit_source_publication(&imported, "custom", "example-agent-v1");
    assert_eq!(receipt["route_source_format"], "ctx_history_jsonl_v2");
    assert_eq!(imported["sources"][0]["path"], json!(source_path));
    assert_eq!(imported["sources"][0]["provider_source_authority"], true);
    assert!(imported["totals"]["current_indexed_documents"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    assert!(!data_root(&temp)
        .join("history-source-plugin-sources")
        .exists());

    let search = json_output(ctx(&temp).args([
        "search",
        "provider-owned durable plugin oracle",
        "--history-source",
        "example-agent/archive",
        "--refresh",
        "off",
        "--format=json",
    ]));
    let result = &search["results"].as_array().unwrap()[0];
    assert_eq!(result["provider_session_id"], "session-1", "{result:#}");
    let event_id = result["ctx_event_id"].as_str().unwrap();
    let shown = json_output(ctx(&temp).args(["show", "event", event_id, "--format=json"]));
    assert_eq!(
        shown["event"]["text"],
        "provider-owned durable plugin oracle"
    );
    assert_eq!(shown["event"]["content"]["policy_status"], "selected");
}

#[test]
fn all_rejected_plugin_fails_even_when_unrelated_history_is_retained() {
    let temp = tempdir();
    let (manifest_dir, source_path) = write_durable_plugin(&temp);
    let source = fs::read_to_string(&source_path).unwrap();
    let malformed = source.replace(r#""event_index":0"#, r#""event_index":"invalid""#);
    assert_ne!(malformed, source);
    fs::write(&source_path, malformed).unwrap();

    let state = temp.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let _daemon = start_source_refresh_daemon(&temp, &data_root(&temp), temp.path(), &state);
    let codex_path = temp.path().join("unrelated-codex.jsonl");
    fs::write(
        &codex_path,
        concat!(
            r#"{"timestamp":"2026-07-30T03:00:00Z","type":"session_meta","payload":{"id":"unrelated-codex","timestamp":"2026-07-30T03:00:00Z","cwd":"/workspace/unrelated","originator":"codex-cli"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-30T03:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"unrelated retained oracle"}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    let retained = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        codex_path.to_str().unwrap(),
        "--no-daemon",
        "--progress",
        "none",
        "--format=json",
    ]));
    assert_eq!(retained["outcome"], "success", "{retained:#}");
    let retained_records = retained["totals"]["current_retained_records"]
        .as_u64()
        .filter(|count| *count > 0)
        .expect("retained Codex fixture records");

    let rejected = failure_json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &manifest_dir)
            .args([
                "import",
                "--history-source",
                "example-plugin/primary",
                "--no-daemon",
                "--progress",
                "none",
                "--format=json",
            ]),
    );
    assert_eq!(rejected["outcome"], "failure", "{rejected:#}");
    assert_eq!(rejected["failure_scope"], "record", "{rejected:#}");
    assert_eq!(rejected["failure_type"], "record_rejection", "{rejected:#}");
    assert_eq!(rejected["sources"][0]["status"], "partial", "{rejected:#}");
    assert_eq!(
        rejected["totals"]["current_retained_records"], retained_records,
        "generation-wide retained history must not mask this failed plugin request: {rejected:#}"
    );
}

#[test]
fn durable_plugin_manifest_rejects_command_runtime_options() {
    let temp = tempdir();
    let (manifest_dir, _) = write_durable_plugin(&temp);
    let manifest = manifest_dir.join("ctx-history-plugin.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["history_sources"][0]["command"] = json!(["export-history"]);
    fs::write(&manifest, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

    let sources = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &manifest_dir)
            .args(["sources", "--format=json"]),
    );
    let invalid = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["status"] == "invalid")
        .unwrap();
    assert!(invalid["error"]
        .as_str()
        .unwrap()
        .contains("either a durable path or a command, not both"));
}
