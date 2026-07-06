#[allow(unused_imports)]
use super::*;

pub(crate) fn tempdir() -> TempDir {
    Builder::new().prefix("ctx-search-mvp-").tempdir().unwrap()
}

pub(crate) fn assert_search_provider_oracle(
    packet: &Value,
    provider: &str,
    query: &str,
    expected_results: usize,
    expected_match_reason: &str,
) {
    assert_search_provider_oracle_with_scope(
        packet,
        provider,
        query,
        expected_results,
        expected_match_reason,
        "session_result",
        "session",
    );
}

pub(crate) fn assert_event_search_provider_oracle(
    packet: &Value,
    provider: &str,
    query: &str,
    expected_results: usize,
    expected_match_reason: &str,
) {
    assert_search_provider_oracle_with_scope(
        packet,
        provider,
        query,
        expected_results,
        expected_match_reason,
        "event",
        "event",
    );
}

pub(crate) fn assert_search_provider_oracle_with_scope(
    packet: &Value,
    provider: &str,
    query: &str,
    expected_results: usize,
    expected_match_reason: &str,
    expected_item_type: &str,
    expected_scope: &str,
) {
    assert_eq!(packet["schema_version"], 1);
    assert_eq!(packet["query"], query);
    assert_eq!(packet["filters"]["provider"], provider);
    let results = packet["results"].as_array().unwrap();
    assert_eq!(
        results.len(),
        expected_results,
        "unexpected search result count in {packet:#}"
    );

    for result in results {
        assert_eq!(result["provider"], provider, "provider filter failed");
        assert_eq!(result["source_exists"], true, "source_exists failed");
        assert_eq!(result["item_type"], expected_item_type);
        assert_eq!(result["result_scope"], expected_scope);
        assert!(result["ctx_event_id"].is_string());
        assert!(result["ctx_session_id"].is_string());
        assert!(result["provider_session_id"].is_string());
        assert!(result["source_path"].is_string());
        assert!(result["cursor"].is_string());
        if expected_scope == "session" {
            assert!(result["session_importance"].is_number());
            assert!(result["more_matches_in_session"].is_number());
            assert_session_suggested_next_commands(result);
        } else {
            assert_eq!(result.get("session_importance"), None);
            assert_eq!(result.get("more_matches_in_session"), None);
            assert_event_suggested_next_commands(result);
        }
        assert!(result["why_matched"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == expected_match_reason));
        assert_provider_citations(result, provider);
    }
}

#[test]
pub(crate) fn mcp_search_requires_query_term_or_file_without_opening_store() {
    let temp = tempdir();
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
                "id": "search",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "provider": "codex",
                        "limit": 5
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "search-hidden-provider",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "hidden provider probe",
                        "provider": "not-a-real-provider",
                        "limit": 5
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "search-provider-alias",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "provider alias probe",
                        "provider": "roo_code",
                        "limit": 5
                    }
                }
            }),
        ],
    );

    let result = &responses[1]["result"];
    assert_eq!(result["isError"], true);
    assert!(result["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("search needs a query or file"));
    let hidden_provider = &responses[2]["result"];
    assert_eq!(hidden_provider["isError"], true);
    assert!(hidden_provider["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("provider must be one of"));
    let alias_result = &responses[3]["result"];
    assert_eq!(alias_result["isError"], true);
    assert!(alias_result["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("ctx store is not initialized"));
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "invalid MCP search should fail before opening the ctx store"
    );
}

#[test]
pub(crate) fn search_refresh_strict_times_out_when_plugin_helper_keeps_stdout_open() {
    let temp = tempdir();
    let script = r#"#!/usr/bin/env python3
import json
import os
import subprocess

observed = "2026-07-01T12:00:00Z"
source_id = os.environ["CTX_HISTORY_SOURCE_ID"]
provider_key = os.environ["CTX_HISTORY_PROVIDER_KEY"]
source_format = os.environ["CTX_HISTORY_SOURCE_FORMAT"]
cursor_stream = os.environ["CTX_HISTORY_CURSOR_STREAM"]
records = [
    {"record_type": "manifest", "schema_version": "ctx-history-jsonl-v1"},
    {"record_type": "source", "source_id": source_id, "provider_key": provider_key, "source_format": source_format, "observed_at": observed, "cursor": {"after": {"stream": cursor_stream, "cursor": json.dumps({"seq": 1}), "observed_at": observed}}},
    {"record_type": "session", "source_id": source_id, "session_id": "hanging-session", "started_at": observed, "agent_type": "primary", "is_primary": True, "status": "completed"},
    {"record_type": "event", "source_id": source_id, "session_id": "hanging-session", "event_index": 0, "event_type": "message", "role": "assistant", "occurred_at": observed, "payload": {"text": "hanging plugin marker"}, "preview": "hanging plugin marker"},
]
for record in records:
    print(json.dumps(record, separators=(",", ":")), flush=True)
subprocess.Popen(["sh", "-c", "sleep 5"])
"#;
    let plugin = write_raw_history_source_plugin_with_options_and_timeout(
        &temp,
        "hanging",
        script,
        true,
        Some("auto"),
        1,
    );

    let started = Instant::now();
    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "search",
                "hanging plugin marker",
                "--provider",
                "custom",
                "--refresh",
                "strict",
                "--json",
            ]),
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "plugin timeout did not bound pipe draining: {stderr}"
    );
    assert!(
        stderr.contains("history source plugin hanging/default timed out after 1s"),
        "{stderr}"
    );
}

#[test]
pub(crate) fn search_refresh_strict_fails_when_no_supported_refresh_source_exists() {
    let temp = tempdir();
    ctx(&temp)
        .args(["search", "anything", "--refresh", "strict", "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "strict search refresh found no supported",
        ));
}

#[test]
pub(crate) fn search_rejects_unbounded_limit() {
    let temp = tempdir();
    ctx(&temp)
        .args(["search", "anything", "--limit", "201"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
pub(crate) fn human_search_reports_no_results() {
    let temp = tempdir();
    let fresh = ctx(&temp)
        .args(["search", "definitely-no-results-here"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let fresh = String::from_utf8(fresh).unwrap();
    assert!(fresh.contains("no results for definitely-no-results-here"));
    assert!(fresh.contains("next: ctx import --all"));

    let fixture = provider_history_fixture("codex-sessions");
    ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--progress",
            "none",
        ])
        .assert()
        .success();
    let indexed = ctx(&temp)
        .args(["search", "definitely-no-results-here"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let indexed = String::from_utf8(indexed).unwrap();
    assert!(indexed.contains("no results for definitely-no-results-here"));
    assert!(indexed.contains("next: try broader terms with ctx search --term \"<term>\""));

    let term_only = ctx(&temp)
        .args(["search", "--term", "term-only-no-results"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let term_only = String::from_utf8(term_only).unwrap();
    assert!(term_only.contains("no results for --term term-only-no-results"));
}

#[test]
pub(crate) fn search_refresh_off_requires_existing_store_without_creating_one() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["search", "anything", "--refresh", "off"]));

    assert!(stderr.contains("ctx store is not initialized"), "{stderr}");
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "refresh-off search should not create the ctx store"
    );
}

#[test]
pub(crate) fn search_normalizes_whitespace_only_filters() {
    let temp = tempdir();
    let no_file = json_output(ctx(&temp).args(["search", "test", "--file", " ", "--json"]));
    assert!(
        !no_file["filters"].as_object().unwrap().contains_key("file"),
        "expected no \"file\" key in filters, got: {}",
        no_file["filters"],
    );

    let no_workspace =
        json_output(ctx(&temp).args(["search", "test", "--workspace", " ", "--json"]));
    assert!(
        !no_workspace["filters"]
            .as_object()
            .unwrap()
            .contains_key("workspace"),
        "expected no \"workspace\" key in filters, got: {}",
        no_workspace["filters"],
    );
}

#[test]
pub(crate) fn skill_install_refreshes_stale_bundled_copy() {
    let temp = tempdir();
    let skill_dir = temp
        .path()
        .join(".agents")
        .join("skills")
        .join("ctx-agent-history-search");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "old instructions\n").unwrap();
    let old_hash = format!("sha256:{:x}", Sha256::digest(b"old instructions\n"));
    fs::write(
        skill_dir.join(".ctx-skill.json"),
        json!({
            "schema_version": 1,
            "installer": "ctx-cli",
            "skill_name": "ctx-agent-history-search",
            "skill_hash": old_hash,
            "ctx_cli_version": "0.0.0",
            "installed_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let stale = json_output(ctx(&temp).args(["skill", "status", "--agent", "universal", "--json"]));
    assert_eq!(stale["results"][0]["status"], "stale");

    let install =
        json_output(ctx(&temp).args(["skill", "install", "--agent", "universal", "--json"]));
    assert_eq!(install["results"][0]["previous_status"], "stale");
    assert_eq!(install["results"][0]["updated"], true);
    assert!(fs::read_to_string(skill_dir.join("SKILL.md"))
        .unwrap()
        .contains("ctx Agent History Search"));
}

#[test]
pub(crate) fn skill_install_preserves_modified_copy_unless_forced() {
    let temp = tempdir();
    let skill_dir = temp
        .path()
        .join(".agents")
        .join("skills")
        .join("ctx-agent-history-search");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "local custom instructions\n").unwrap();

    let output = ctx(&temp)
        .args(["skill", "install", "--agent", "universal", "--json"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["results"][0]["success"], false);
    assert_eq!(json["results"][0]["previous_status"], "modified");
    assert_eq!(json["results"][0]["status"], "modified");
    assert!(json["results"][0]["error"]
        .as_str()
        .unwrap()
        .contains("--force"));
    assert_eq!(
        fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        "local custom instructions\n"
    );

    let forced = json_output(ctx(&temp).args([
        "skill",
        "install",
        "--agent",
        "universal",
        "--force",
        "--json",
    ]));
    assert_eq!(forced["results"][0]["success"], true);
    assert_eq!(forced["results"][0]["previous_status"], "modified");
    assert_eq!(forced["results"][0]["status"], "current");
    assert!(fs::read_to_string(skill_dir.join("SKILL.md"))
        .unwrap()
        .contains("ctx Agent History Search"));
}
