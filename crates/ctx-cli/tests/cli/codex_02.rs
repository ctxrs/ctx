#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn fresh_home_search_mvp_flow() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");

    ctx(&temp)
        .arg("setup")
        .assert()
        .success()
        .stdout(predicate::str::contains("no local history was indexed"));

    let setup_json = json_output(ctx(&temp).args(["setup", "--json"]));
    assert_eq!(setup_json["schema_version"], 1);
    assert_eq!(setup_json["network_required"], false);
    assert_eq!(setup_json["repo_writes"], false);
    assert_eq!(setup_json["import"]["ran"], true);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    assert_eq!(sources["schema_version"], 1);
    assert!(sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source["provider"] == "codex"));

    let import = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
    ]));
    assert_eq!(import["schema_version"], 1);
    assert!(import["totals"]["imported_sessions"].as_u64().unwrap() > 0);
    assert!(import["totals"]["source_files"].as_u64().unwrap() > 0);
    assert!(import["totals"]["source_bytes"].as_u64().unwrap() > 0);

    let search =
        json_output(ctx(&temp).args(["search", "onboarding", "--provider", "codex", "--json"]));
    assert_eq!(search["schema_version"], 1);
    assert_eq!(search["share_safe"], false);
    assert_omits_keys(
        &search,
        &[
            "record_id",
            "history_record_id",
            "raw_source_path",
            "kind",
            "external_session_id",
        ],
    );
    let first_result = &search["results"][0];
    assert_eq!(first_result["item_type"], "session_result");
    assert_eq!(first_result["result_scope"], "session");
    let ctx_event_id = first_result["ctx_event_id"].as_str().unwrap().to_owned();
    let ctx_session_id = first_result["ctx_session_id"].as_str().unwrap().to_owned();
    assert!(first_result["provider_session_id"].is_string());
    assert!(first_result["source_path"].is_string());
    assert!(first_result["cursor"].is_string());
    assert_session_suggested_next_commands(first_result);
    assert!(first_result["citations"][0]["ctx_event_id"].is_string());
    assert!(first_result["citations"][0]["ctx_session_id"].is_string());

    let term_search = json_output(ctx(&temp).args([
        "search",
        "zzzz-no-match",
        "--term",
        "onboarding",
        "--provider",
        "codex",
        "--json",
    ]));
    assert_eq!(term_search["query"], "zzzz-no-match OR onboarding");
    assert!(!term_search["results"].as_array().unwrap().is_empty());
    assert!(term_search["results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|result| { result["suggested_next_commands"].as_array().unwrap().iter() })
        .all(|command| !command.as_str().unwrap().starts_with("ctx search ")));

    let event_search = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--events",
        "--json",
    ]));
    assert_event_search_provider_oracle(&event_search, "codex", "onboarding", 1, "message");

    let session_events = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--session",
        &ctx_session_id,
        "--json",
    ]));
    assert_event_search_provider_oracle(&session_events, "codex", "onboarding", 1, "message");
    assert_eq!(session_events["filters"]["session"], ctx_session_id);
    assert!(session_events["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|result| result["ctx_session_id"] == ctx_session_id));

    let session_prefix = &ctx_session_id[..8];
    let prefixed_session_events = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--session",
        session_prefix,
        "--json",
    ]));
    assert_event_search_provider_oracle(
        &prefixed_session_events,
        "codex",
        "onboarding",
        1,
        "message",
    );
    assert_eq!(
        prefixed_session_events["filters"]["session"],
        ctx_session_id
    );

    let human_search = ctx(&temp)
        .args(["search", "onboarding"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_search = String::from_utf8(human_search).unwrap();
    assert!(human_search.contains("1. "));
    assert!(human_search.contains("importance"));
    assert!(human_search.contains("session "));
    assert!(human_search.contains("event "));
    assert!(human_search.contains("inspect: ctx show event"));
    assert!(!human_search.contains("ctx_event_id"));
    assert!(!human_search.contains("provider_session_id"));
    assert!(!human_search.contains("next:"));
    assert!(!human_search.contains("work_record"));
    assert!(!human_search.contains("history_record"));

    let verbose_search = ctx(&temp)
        .args(["search", "onboarding", "--verbose"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let verbose_search = String::from_utf8(verbose_search).unwrap();
    assert!(verbose_search.contains("ctx_event_id"));
    assert!(verbose_search.contains("ctx_session_id"));
    assert!(verbose_search.contains("provider_session_id"));
    assert!(verbose_search.contains("session_importance"));
    assert!(verbose_search.contains("next: ctx show session"));
    assert!(verbose_search.contains("next: ctx show event"));
    assert!(verbose_search.contains("next: ctx search onboarding --session"));
    assert!(!human_search.contains("work_record"));
    assert!(!human_search.contains("history_record"));

    let file_search =
        json_output(ctx(&temp).args(["search", "--file", "crates/foo/src/lib.rs", "--json"]));
    assert_eq!(file_search["query"], "");
    assert!(file_search["results"].is_array());

    let show_event = json_output(ctx(&temp).args([
        "show",
        "event",
        &ctx_event_id,
        "--window",
        "2",
        "--format",
        "json",
    ]));
    assert_eq!(show_event["schema_version"], 1);
    assert_eq!(show_event["item_type"], "event_window");
    assert_eq!(show_event["event"]["ctx_event_id"], ctx_event_id);
    assert_eq!(show_event["event"]["ctx_session_id"], ctx_session_id);
    assert_omits_keys(
        &show_event,
        &[
            "record_id",
            "history_record_id",
            "kind",
            "payload",
            "payload_blob_id",
            "dedupe_key",
            "capture_source_id",
        ],
    );
    assert!(show_event["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["ctx_event_id"].is_string()
            && event["ctx_session_id"].is_string()
            && event["preview"].is_string()));

    let show_event_prefix = json_output(ctx(&temp).args([
        "show",
        "event",
        &ctx_event_id[..8],
        "--window",
        "1",
        "--format",
        "json",
    ]));
    assert_eq!(show_event_prefix["event"]["ctx_event_id"], ctx_event_id);

    let oversized_after = failure_stderr(ctx(&temp).args([
        "show",
        "event",
        &ctx_event_id,
        "--after",
        "18446744073709551615",
    ]));
    assert!(
        oversized_after.contains("event window must be between 0 and 50"),
        "{oversized_after}"
    );

    let oversized_window = failure_stderr(ctx(&temp).args([
        "show",
        "event",
        &ctx_event_id,
        "--window",
        "18446744073709551615",
    ]));
    assert!(
        oversized_window.contains("event window must be between 0 and 50"),
        "{oversized_window}"
    );

    let show_session =
        json_output(ctx(&temp).args(["show", "session", &ctx_session_id, "--format", "json"]));
    assert_eq!(show_session["schema_version"], 1);
    assert_eq!(show_session["item_type"], "session_transcript");
    assert_eq!(show_session["session"]["item_type"], "session");
    assert_eq!(show_session["session"]["item_id"], ctx_session_id);
    assert_eq!(show_session["mode"], "lite");

    let show_session_prefix =
        json_output(ctx(&temp).args(["show", "session", &ctx_session_id[..8], "--format", "json"]));
    assert_eq!(show_session_prefix["session"]["item_id"], ctx_session_id);

    let show_session_full = json_output(ctx(&temp).args([
        "show",
        "session",
        &ctx_session_id,
        "--mode",
        "full",
        "--format",
        "json",
    ]));
    assert_eq!(show_session_full["schema_version"], 1);
    assert_eq!(show_session_full["item_type"], "session_transcript");
    assert_eq!(show_session_full["session"]["item_id"], ctx_session_id);
    assert_eq!(show_session_full["mode"], "full");

    let locate_event = json_output(ctx(&temp).args(["locate", "event", &ctx_event_id, "--json"]));
    assert_eq!(locate_event["schema_version"], 1);
    assert_eq!(locate_event["item_type"], "event_location");
    assert_eq!(locate_event["ctx_event_id"], ctx_event_id);
    assert_eq!(locate_event["ctx_session_id"], ctx_session_id);
    assert_eq!(locate_event["provider"], "codex");
    assert!(locate_event["provider_session_id"].is_string());
    assert!(locate_event["source"]["path"].is_string());
    assert!(locate_event["cursor"].is_string());

    let export_path = temp.path().join("transcript.md");
    ctx(&temp)
        .args([
            "show",
            "session",
            &ctx_session_id,
            "--format",
            "markdown",
            "--out",
            export_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(
        export_path.exists(),
        "show session --out should write the requested artifact path"
    );
    let exported = fs::read_to_string(&export_path).unwrap();
    assert!(
        exported.contains("- mode: `lite`"),
        "show session --out should default to lite transcript mode"
    );

    let full_export_path = temp.path().join("transcript-full.md");
    ctx(&temp)
        .args([
            "show",
            "session",
            &ctx_session_id,
            "--mode",
            "full",
            "--format",
            "markdown",
            "--out",
            full_export_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let exported_full = fs::read_to_string(&full_export_path).unwrap();
    assert!(
        exported_full.contains("- mode: `full`"),
        "show session --mode full --out should remain explicit"
    );

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["schema_version"], 1);
    assert!(status["indexed_items"].as_u64().unwrap() > 0);

    let doctor = json_output(ctx(&temp).args(["doctor", "--json"]));
    assert_eq!(doctor["schema_version"], 1);
    assert_eq!(doctor["ok"], true);
    assert_eq!(doctor["progress"], "auto");

    let doctor_progress = ctx(&temp)
        .args(["doctor", "--json", "--progress", "json"])
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    let doctor_progress = String::from_utf8(doctor_progress).unwrap();
    assert!(doctor_progress.contains(r#""operation":"doctor""#));
    assert!(doctor_progress.contains(r#""phase":"checking""#));
}

#[test]
pub(crate) fn mcp_search_and_show_tools_return_structured_json_without_refresh() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert!(imported["totals"]["imported_events"].as_u64().unwrap() > 0);

    let search_responses = mcp_roundtrip(
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
                "id": "search",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "onboarding",
                        "provider": "codex",
                        "limit": 5
                    }
                }
            }),
        ],
    );
    let search = &search_responses[1]["result"]["structuredContent"];
    assert_eq!(search["schema_version"], 1);
    assert_eq!(search["query"], "onboarding");
    assert_eq!(search["freshness"]["mode"], "off");
    assert_eq!(search["freshness"]["status"], "skipped");
    assert_eq!(search["share_safe"], false);
    assert_eq!(
        search_responses[1]["result"]["content"][0]["text"],
        "ctx returned structured JSON in structuredContent. Treat it as private local history."
    );
    let first_result = &search["results"][0];
    let ctx_session_id = first_result["ctx_session_id"].as_str().unwrap();
    let ctx_event_id = first_result["ctx_event_id"].as_str().unwrap();

    let show_responses = mcp_roundtrip(
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
                "id": "session",
                "method": "tools/call",
                "params": {
                    "name": "show_session",
                    "arguments": {
                        "ctx_session_id": ctx_session_id,
                        "mode": "lite"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "event",
                "method": "tools/call",
                "params": {
                    "name": "show_event",
                    "arguments": {
                        "ctx_event_id": ctx_event_id,
                        "window": 1
                    }
                }
            }),
        ],
    );

    let session = &show_responses[1]["result"]["structuredContent"];
    assert_eq!(session["item_type"], "session_transcript");
    assert_eq!(session["ctx_session_id"], ctx_session_id);
    assert_eq!(session["mode"], "lite");
    assert!(session["events"].as_array().unwrap().iter().all(|event| {
        event["ctx_session_id"] == ctx_session_id && event["ctx_event_id"].is_string()
    }));

    let event = &show_responses[2]["result"]["structuredContent"];
    assert_eq!(event["item_type"], "event_window");
    assert_eq!(event["ctx_event_id"], ctx_event_id);
    assert_eq!(event["ctx_session_id"], ctx_session_id);
    assert!(!event["events"].as_array().unwrap().is_empty());
}

#[test]
pub(crate) fn mcp_search_excludes_active_codex_session_by_default_when_available() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));

    let excluded = mcp_roundtrip_with_env(
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
                "id": "search",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "onboarding",
                        "provider": "codex",
                        "limit": 5
                    }
                }
            }),
        ],
        &[("CODEX_THREAD_ID", "codex-session-root")],
    );
    let excluded_search = &excluded[1]["result"]["structuredContent"];
    assert_eq!(excluded_search["results"].as_array().unwrap().len(), 0);
    assert_eq!(
        excluded_search["filters"]["exclude_provider_session"]["provider"],
        "codex"
    );

    let included = mcp_roundtrip_with_env(
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
                "id": "search",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "onboarding",
                        "provider": "codex",
                        "limit": 5,
                        "include_current_session": true
                    }
                }
            }),
        ],
        &[("CODEX_THREAD_ID", "codex-session-root")],
    );
    let included_search = &included[1]["result"]["structuredContent"];
    assert_eq!(included_search["results"].as_array().unwrap().len(), 1);
    assert!(included_search["filters"]["exclude_provider_session"].is_null());
}
