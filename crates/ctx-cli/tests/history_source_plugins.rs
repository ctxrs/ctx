mod support;

use support::*;

fn write_durable_plugin(temp: &TempDir) -> (PathBuf, PathBuf) {
    let provider_root = temp.path().join("provider-owned");
    fs::create_dir_all(&provider_root).unwrap();
    let source_path = provider_root.join("history.jsonl");
    let records = [
        json!({"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}),
        json!({
            "record_type":"source",
            "source_id":"archive",
            "provider_key":"example-agent",
            "source_format":"example-agent-v1"
        }),
        json!({
            "record_type":"session",
            "source_id":"archive",
            "session_id":"session-1",
            "started_at":"2026-07-30T04:00:00Z",
            "agent_type":"primary",
            "is_primary":true,
            "status":"completed"
        }),
        json!({
            "record_type":"event",
            "source_id":"archive",
            "session_id":"session-1",
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
fn durable_plugin_path_indexes_in_place_and_hydrates_exact_content() {
    let temp = tempdir();
    let (manifest_dir, source_path) = write_durable_plugin(&temp);
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
    assert_eq!(imported["outcome"], "success", "{imported:#}");
    assert_eq!(imported["sources"][0]["path"], json!(source_path));
    assert_eq!(imported["sources"][0]["provider_source_authority"], true);
    assert!(imported["totals"]["current_indexed_documents"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    for forbidden in [
        "imported_sessions",
        "imported_events",
        "rejected_records",
        "rejections",
    ] {
        assert!(
            imported["totals"].get(forbidden).is_none(),
            "{forbidden} was fabricated in {imported:#}"
        );
    }
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
    let event_id = result["ctx_event_id"].as_str().unwrap();
    let shown = json_output(ctx(&temp).args([
        "show",
        "event",
        event_id,
        "--content",
        "complete",
        "--format=json",
    ]));
    assert_eq!(
        shown["event"]["text"],
        "provider-owned durable plugin oracle"
    );
    assert_eq!(shown["event"]["content"]["origin"], "provider_source");
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
