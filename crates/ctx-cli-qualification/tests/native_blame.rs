#[path = "../../ctx-cli-contract-tests/tests/contracts/support/mod.rs"]
mod support;

use ctx_attribution::protocol::{
    core_record_sha256, BlameAttribution, BlameResultFreshness, BlameTarget, EvidenceCitation,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_history_snapshot_reader::{CoreSnapshot, SnapshotContract};
use support::*;

#[test]
fn authored_commit_has_identical_native_cli_mcp_results_citations_and_one_terminal_per_call() {
    let temp = daemon_test_root();
    let root = data_root(&temp);
    fs::create_dir_all(&root).unwrap();
    ctx_history_platform::platform_security::restrict_private_directory(&root).unwrap();
    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"manual\"\n[sources]\nautomatic = false\n",
    )
    .unwrap();
    let seed = ctx_attribution::test_fixtures::authored_commit_fixture(
        &temp.path().join("authored-repository"),
    )
    .unwrap();
    let mut writer = GenerationWriter::open(root.join("search/lexical"), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(seed.record.source.clone()).unwrap();
    writer.add_core_record(seed.record.clone()).unwrap();
    writer.certify_source(seed.certificate).unwrap();
    let generation = writer.commit(|_| true).unwrap().generation_id;
    let snapshot =
        CoreSnapshot::open(&root, &generation, &SnapshotContract::current().unwrap()).unwrap();
    ctx_attribution::catch_up(&root, &snapshot, &|| false).unwrap();
    let target = BlameTarget::Commit {
        oid: seed.oid.clone(),
        repository: None,
    };
    let result = ctx_attribution::query(&root, &target, 8, None).unwrap();
    assert_eq!(result.freshness, BlameResultFreshness::Current);
    assert_eq!(
        result.result.outcome.attribution,
        BlameAttribution::Possible
    );
    assert!(!result.result.matches.is_empty());
    let citation = EvidenceCitation {
        core_generation_id: generation,
        source: seed.record.source.clone(),
        session_id: seed.record.session_id,
        event_id: seed.record.event_id,
        event_sequence: 1,
        byte_range: None,
        evidence_sha256: Some(core_record_sha256(&seed.record).unwrap()),
    };
    assert!(result
        .result
        .evidence
        .iter()
        .any(|evidence| evidence.citation == citation));
    let previews = ctx_attribution::hydrate_evidence_previews(&root, &result.result);
    let (expected, expected_mcp_text) =
        ctx_attribution::presentation::mcp_text::render_blame_tool(&result, Some(&previews));
    let sink = temp.path().join("never-uploaded.jsonl");
    let endpoint = file_url(&sink);
    let state = temp.path().join("telemetry-state");
    let launch_hint =
        expected_device_path(temp.path(), &state).with_file_name("analytics-launch-v1.json");
    fs::create_dir_all(launch_hint.parent().unwrap()).unwrap();
    // Defer only the delivery child so foreground queue assertions cannot race a drain.
    fs::write(&launch_hint, br#"{"schema_version":2,"next_allowed_at":0}"#).unwrap();
    for args in [
        vec![
            "blame",
            seed.oid.as_str(),
            "--type=commit",
            "--limit=8",
            "--format=json",
        ],
        vec![
            "blame",
            "commit",
            seed.oid.as_str(),
            "--limit=8",
            "--format=json",
        ],
    ] {
        let response = json_output(
            ctx(&temp)
                .args(args)
                .env("XDG_STATE_HOME", &state)
                .env("LOCALAPPDATA", &state)
                .env("CTX_ANALYTICS_ENABLED", "true")
                .env("CTX_ANALYTICS_ENDPOINT", &endpoint),
        );
        assert_eq!(response, expected);
    }
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"authored-test","version":"0"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"blame","arguments":{"target":{"kind":"commit","oid":seed.oid},"limit":8}}}),
        ],
        &[
            ("XDG_STATE_HOME", state.to_str().unwrap()),
            ("LOCALAPPDATA", state.to_str().unwrap()),
            ("CTX_ANALYTICS_ENABLED", "true"),
            ("CTX_ANALYTICS_ENDPOINT", endpoint.as_str()),
        ],
    );
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[1]["result"]["structuredContent"], expected);
    assert_eq!(
        responses[1]["result"]["content"][0]["text"],
        expected_mcp_text
    );
    // With automatic child delivery deferred, a sink write is a foreground regression.
    assert!(!sink.exists(), "Blame foreground must not upload");
    let outbox = launch_hint.with_file_name("analytics-outbox-v1.json");
    let queued: Value = serde_json::from_slice(&fs::read(outbox).unwrap()).unwrap();
    for entry in queued["entries"].as_array().unwrap() {
        assert_eq!(entry["attempts"], 0, "foreground upload");
    }

    let events = read_queued_analytics_events(temp.path())
        .into_iter()
        .flat_map(|payload| payload["events"].as_array().unwrap().clone())
        .filter(|event| {
            event["event_name"] == "operation_completed" && event["operation"] == "blame"
        })
        .collect::<Vec<_>>();
    assert_eq!(
        events.len(),
        3,
        "one ordinary terminal per actual call: {events:#?}"
    );
    let surface_counts = ["cli", "mcp"].map(|surface| {
        events
            .iter()
            .filter(|event| event["surface"] == surface)
            .count()
    });
    assert_eq!(surface_counts, [2, 1]);
    for event in events {
        assert_eq!(event["outcome"], "success");
        let properties = event["properties"].as_object().unwrap();
        if event["surface"] == "cli" {
            assert_analytics_properties_are_allowlisted(properties);
        }
        for (key, value) in [
            ("blame_target_kind", "commit"),
            ("blame_request_kind", "first_request"),
            ("blame_result_state", "possible"),
            ("blame_freshness", "current"),
        ] {
            assert_eq!(properties[key], value, "{event:#}");
        }
        assert_eq!(properties["blame_output_served"], true);
        assert_eq!(properties["blame_has_more"], expected["next"].is_object());
        let encoded = serde_json::to_string(properties).unwrap();
        assert!(!encoded.contains(seed.repository.to_str().unwrap()));
        assert!(!encoded.contains(&citation.core_generation_id));
    }
}

// Authored transcripts exercise import, retained Core, materialization, and CLI
// blame against actual Git output. They are not native provider captures.
fn imported_commit_case(provider: &str, case: &str) {
    let temp = daemon_test_root();
    let session_id = if provider == "codex" {
        "019fb000-0000-7000-8000-0000000000b1"
    } else {
        "primary"
    };
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&repository)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env(
                "SystemRoot",
                std::env::var_os("SystemRoot").unwrap_or_default(),
            )
            .env("HOME", temp.path())
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", temp.path().join("absent-gitconfig"))
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=absent-hooks",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    git(&["init", "-q", "--initial-branch=main"]);
    git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/example/producer-fixture.git",
    ]);
    fs::write(repository.join("witness.txt"), "commit authority witness\n").unwrap();
    git(&["add", "witness.txt"]);
    let receipt = git(&["commit", "-m", "authored commit"]);
    let oid = git(&["rev-parse", "HEAD"]).trim().to_owned();
    let output = format!("{receipt}{oid}\n");
    let command = if case == "replayed_output" {
        "cat earlier-commit.log"
    } else {
        "git commit -m 'authored commit' && git rev-parse HEAD"
    };
    let result_id = if case == "unlinked" {
        "other-call"
    } else {
        "commit-call"
    };
    let timestamp = "2026-01-01T00:00:00Z";
    let source = temp.path().join("transcripts");
    let (path, mut rows) = if provider == "claude" {
        let project = source.join("projects").join("fixture-project");
        let path = if case == "child" {
            project.join("primary/subagents/agent-child.jsonl")
        } else {
            project.join("primary.jsonl")
        };
        (
            path,
            vec![
                json!({"type":"assistant", "uuid":"call-event", "sessionId":"primary",
                "cwd":repository, "timestamp":timestamp, "gitBranch":"main", "version":"2.0.0",
                "message":{"role":"assistant", "content":[{"type":"tool_use",
                    "id":"commit-call", "name":"Bash", "input":{"command":command}}]}}),
                json!({"type":"user", "uuid":"result-event", "sessionId":"primary",
                "cwd":repository, "timestamp":timestamp, "gitBranch":"main", "version":"2.0.0",
                "message":{"role":"user", "content":[{"type":"tool_result",
                    "tool_use_id":result_id, "is_error":case == "failed", "content":output}]}}),
            ],
        )
    } else if provider == "codex" {
        let arguments = json!({"cmd":command, "workdir":repository});
        (
            source.join("rollout.jsonl"),
            vec![
                json!({"type":"session_meta", "timestamp":timestamp, "payload":{
                    "id":session_id, "cwd":repository, "originator":"codex_exec", "source":"cli"}}),
                json!({"type":"response_item", "timestamp":timestamp, "payload":{
                    "type":"custom_tool_call", "name":"exec", "call_id":"commit-call",
                    "input":format!("const r = await tools.exec_command({arguments}); text(r.output);")}}),
                json!({"type":"response_item", "timestamp":timestamp, "payload":{
                "type":"custom_tool_call_output", "call_id":result_id, "output":[
                    {"type":"input_text", "text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},
                    {"type":"input_text", "text":output}
                ]}}),
            ],
        )
    } else if provider == "custom" {
        let present = |value| json!({"capture_status":"present", "value":value});
        (
            source.join("history.jsonl"),
            vec![
                json!({"record_type":"manifest", "schema_version":"ctx-history-jsonl-v2"}),
                json!({"record_type":"source", "source_id":"export", "provider_key":"copied-history",
                "source_format":"transcript-copy", "trust":"unknown", "fidelity":"imported"}),
                json!({"record_type":"session", "source_id":"export", "provider_session_id":"copy",
                "started_at":timestamp, "cwd":repository}),
                json!({"record_type":"event", "source_id":"export", "provider_session_id":"copy",
                "event_index":0, "occurred_at":timestamp, "event_type":"tool_call",
                "payload":{"text":"Imported history", "activity":{"revision":1,
                    "provider_call_id":{"Utf8":"commit-call"},
                    "invocation":{"tool":"bash", "arguments":present(json!({"command":command}))},
                    "facts":[{"kind":"command", "value":command}]}}}),
                json!({"record_type":"event", "source_id":"export", "provider_session_id":"copy",
                "event_index":1, "occurred_at":timestamp, "event_type":"tool_output",
                "payload":{"text":"Imported history", "activity":{"revision":1,
                    "provider_call_id":{"Utf8":"commit-call"}, "result":{"status":"success",
                        "text":present(json!(output)), "structured_content":{"capture_status":"absent"}}}}}),
            ],
        )
    } else {
        let mut header = json!({"type":"session", "version":3, "id":"primary",
            "cwd":repository, "timestamp":timestamp});
        if case == "child" {
            header["parentSession"] = json!("parent-session");
        }
        if case == "fork_custom_path" {
            let parent = temp.path().join("original.jsonl");
            fs::write(
                &parent,
                format!(
                    "{}\n",
                    json!({"type":"session", "version":3,
                "id":"original", "cwd":repository, "timestamp":timestamp})
                ),
            )
            .unwrap();
            header["parentSession"] = json!(parent);
        }
        let call = json!({"type":"toolCall", "id":"commit-call", "name":"bash",
            "arguments":{"command":command}});
        let message = if case.ends_with("flat") {
            json!({"role":"assistant", "toolCallId":"commit-call", "toolName":"bash",
                "arguments":{"command":command}})
        } else if case.ends_with("parallel") {
            json!({"role":"assistant", "content":[
                {"type":"toolCall", "id":"status-call", "name":"bash",
                    "arguments":{"command":"git status --short"}}, call]})
        } else {
            json!({"role":"assistant", "content":[call]})
        };
        (
            source.join("primary.jsonl"),
            vec![
                header,
                json!({"type":"message", "id":"call-event", "timestamp":timestamp,
                "message":message}),
                json!({"type":"message", "id":"result-event", "timestamp":timestamp,
                "message":{"role":"toolResult", "toolCallId":result_id, "toolName":"bash",
                    "isError":case.starts_with("failed"), "content":[{"type":"text", "text":output}]}}),
            ],
        )
    };
    if provider == "claude" {
        for (index, row) in rows.iter_mut().enumerate() {
            if matches!(case, "sidechain" | "sidechain_call" | "sidechain_result")
                && (case == "sidechain" || (case == "sidechain_call") == (index == 0))
            {
                row["isSidechain"] = json!(true);
                row["agentId"] = json!("child");
            }
            if case == "agent_id" {
                row["agentId"] = json!("child");
            }
            if case == "sidechain_only" {
                row["isSidechain"] = json!(true);
            }
            if case == "primary_explicit_false" {
                row["isSidechain"] = json!(false);
            }
        }
    }
    let sidechain_oid = if case == "primary_mixed_sidechain" {
        let child_receipt = git(&["commit", "--allow-empty", "-m", "child commit"]);
        let child_oid = git(&["rev-parse", "HEAD"]).trim().to_owned();
        let mut child_rows = rows.clone();
        child_rows[0]["uuid"] = json!("child-call-event");
        child_rows[0]["message"]["content"][0]["id"] = json!("child-call");
        child_rows[0]["message"]["content"][0]["input"]["command"] =
            json!("git commit --allow-empty -m 'child commit' && git rev-parse HEAD");
        child_rows[1]["uuid"] = json!("child-result-event");
        child_rows[1]["message"]["content"][0]["tool_use_id"] = json!("child-call");
        child_rows[1]["message"]["content"][0]["content"] =
            json!(format!("{child_receipt}{child_oid}\n"));
        for row in &mut child_rows {
            row["isSidechain"] = json!(true);
            row["agentId"] = json!("child");
        }
        child_rows.extend(rows);
        rows = child_rows;
        Some(child_oid)
    } else {
        None
    };
    if case == "fork_custom_path" {
        let mut original = rows.clone();
        original[0]["id"] = json!("original");
        original[0].as_object_mut().unwrap().remove("parentSession");
        fs::write(
            temp.path().join("original.jsonl"),
            original
                .iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>(),
        )
        .unwrap();
    }
    if case == "failed_large_details" {
        rows.last_mut().unwrap()["message"]["details"] =
            json!({"diagnostic":"x".repeat(9 * 1024 * 1024)});
    }
    if case == "failed_large_output" {
        rows.last_mut().unwrap()["message"]["content"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"text", "text":"x".repeat(9 * 1024 * 1024)}));
    }
    if case == "primary_no_error_flag" {
        let message = &mut rows.last_mut().unwrap()["message"];
        if provider == "claude" {
            message["content"][0]
                .as_object_mut()
                .unwrap()
                .remove("is_error");
        } else {
            message.as_object_mut().unwrap().remove("isError");
        }
    }
    if case == "missing_result" {
        rows.pop();
    }
    if case.ends_with("parallel") {
        // The commit result arrives before the first call's result.
        rows.push(
            json!({"type":"message", "id":"status-result", "timestamp":timestamp,
            "message":{"role":"toolResult", "toolCallId":"status-call", "toolName":"bash",
                "isError":false, "content":[{"type":"text", "text":"clean worktree"}]}}),
        );
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    if provider == "pi" {
        // Non-session sidecars must not discard the valid file's header evidence.
        fs::write(source.join("sidecar.jsonl"), "not a session\n").unwrap();
    }
    fs::write(
        &path,
        rows.iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let root = data_root(&temp);
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"manual\"\n[sources]\nautomatic = false\n",
    )
    .unwrap();
    let import_root = if provider == "claude" {
        source.join("projects")
    } else {
        source
    };
    let mut import = ctx(&temp);
    import.arg("import");
    if provider == "custom" {
        import
            .args(["--input-format", "ctx-history-jsonl-v2", "--path"])
            .arg(&path);
    } else {
        import
            .args(["--provider", provider, "--path"])
            .arg(&import_root);
    }
    let imported = import.args(["--progress", "none"]).assert();
    if provider == "pi" {
        // The sidecar reports one file-local failure; the session still imports.
        imported.code(1).stderr(predicate::str::contains(
            "1 import source failed; first failure: Pi source has no valid session header",
        ));
    } else {
        imported.success();
    }
    if matches!(
        case,
        "failed_large_details" | "failed_large_output" | "fork_custom_path"
    ) {
        let generation = ctx_history_snapshot_reader::load_active_generation_id(&root)
            .unwrap()
            .unwrap();
        let snapshot =
            CoreSnapshot::open(&root, &generation, &SnapshotContract::current().unwrap()).unwrap();
        let sources = snapshot.source_manifest_page(None, 8).unwrap();
        assert!(sources.terminal);
        let records = snapshot
            .records_with_default_bounds(sources.items[0].source.clone())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let result = records
            .iter()
            .find(|record| {
                record
                    .content
                    .activity
                    .as_ref()
                    .is_some_and(|activity| activity.result.is_some())
            })
            .unwrap();
        if case.starts_with("failed_large") {
            use ctx_attribution::protocol::{ActivityJsonCapture, ActivityTextCapture};
            let activity = result
                .content
                .activity
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap();
            assert_eq!(activity.structured_content, ActivityJsonCapture::Absent);
            assert_eq!(activity.status.as_deref(), Some("error"));
            if case == "failed_large_details" {
                // A single retained envelope now fits; the original 9 MiB
                // diagnostic must still never become a successful result.
                assert!(
                    matches!(&activity.text, ActivityTextCapture::Present { value } if value == &output)
                );
                assert_eq!(
                    result.content.structured_content.as_ref().unwrap()["entry"]["message"]
                        ["isError"],
                    true
                );
            } else {
                assert!(result.content.structured_content.is_none());
                assert!(
                    matches!(&activity.text, ActivityTextCapture::Omitted { reason, .. } if reason == "size_limit")
                );
                assert!(result
                    .content
                    .normalized_body
                    .as_ref()
                    .unwrap()
                    .starts_with(&output));
            }
        } else {
            assert!(result.parent_session_id.is_none());
            assert_eq!(
                result.content.structured_content.as_ref().unwrap()["session_parent_claim_present"],
                true
            );
        }
    }
    if let Some(child_oid) = sidechain_oid {
        let failed = ctx(&temp)
            .current_dir(&repository)
            .args(["blame", "commit", &child_oid, "--format", "json"])
            .assert()
            .failure()
            .get_output()
            .clone();
        let result: Value = serde_json::from_slice(&failed.stderr).unwrap();
        assert_eq!(result["reason"], "target_not_indexed", "{result:#}");
    }
    let status = json_output(ctx(&temp).args(["status", "--format", "json"]));
    assert_eq!(
        status["attribution"]["currentness"], "current",
        "{status:#}"
    );
    let count = &status["attribution"]["coverage"]["exact_commit_evidence_events"];
    let mut blame = ctx(&temp);
    blame
        .current_dir(&repository)
        .args(["blame", "commit", &oid, "--format", "json"]);
    if case.starts_with("primary") {
        assert_eq!(status["attribution"]["materialized_coverage"], "complete");
        assert_eq!(*count, 1, "{provider}: {status:#}");
        assert_eq!(status["attribution"]["availability"]["commit_blame"], true);
        let result = json_output(&mut blame);
        assert_eq!(result["outcome"]["attribution"], "possible", "{result:#}");
        assert_eq!(result["matches"].as_array().unwrap().len(), 1, "{result:#}");
        assert_eq!(
            result["matches"][0]["value"]["predicate"], "possibly_produced_by",
            "{result:#}"
        );
        let citation = &result["evidence"][0]["citation"];
        assert_eq!(
            result["matches"][0]["value"]["direct_actor"]["display"],
            citation["session_id"]["uuid"]
        );
        let event = json_output(ctx(&temp).args([
            "show",
            "event",
            citation["event_id"]["uuid"].as_str().unwrap(),
            "--format",
            "json",
        ]));
        assert_eq!(
            event["event"]["provider_session_id"], session_id,
            "{event:#}"
        );
    } else {
        assert_eq!(*count, 0, "{provider} {case}: {status:#}");
        assert_eq!(status["attribution"]["availability"]["commit_blame"], false);
        let failed = blame.assert().failure().get_output().clone();
        let result: Value = serde_json::from_slice(&failed.stderr)
            .unwrap_or_else(|error| panic!("{error}: {failed:?}"));
        assert_eq!(
            result["reason"], "target_not_indexed",
            "{provider} {case}: {result:#}"
        );
    }
}

#[test]
fn claude_primary_commit_survives_producer_authority_gate() {
    imported_commit_case("claude", "primary");
}

#[test]
fn pi_primary_commit_survives_producer_authority_gate() {
    imported_commit_case("pi", "primary");
}

#[test]
fn pi_flat_and_parallel_commits_survive_producer_authority_gate() {
    for case in ["primary_flat", "primary_parallel"] {
        imported_commit_case("pi", case);
    }
}

#[test]
fn codex_code_mode_commit_survives_import_and_producer_authority_gate() {
    imported_commit_case("codex", "primary");
}

#[test]
fn claude_child_and_nonproducing_results_stay_unattributed() {
    for case in [
        "child",
        "replayed_output",
        "unlinked",
        "failed",
        "missing_result",
    ] {
        imported_commit_case("claude", case);
    }
}

#[test]
fn pi_fork_and_nonproducing_results_stay_unattributed() {
    for case in [
        "child",
        "replayed_output",
        "unlinked",
        "failed",
        "failed_parallel",
        "missing_result",
    ] {
        imported_commit_case("pi", case);
    }
}

#[test]
fn unknown_origin_import_stays_unattributed() {
    imported_commit_case("custom", "unknown_origin");
}

#[test]
fn pi_unresolved_parent_stays_unattributed() {
    imported_commit_case("pi", "fork_custom_path");
}

#[test]
fn pi_omitted_failed_envelope_stays_unattributed() {
    imported_commit_case("pi", "failed_large_details");
    imported_commit_case("pi", "failed_large_output");
}

#[test]
fn complete_native_results_with_no_error_field_still_attribute() {
    for provider in ["claude", "pi"] {
        imported_commit_case(provider, "primary_no_error_flag");
    }
}

#[test]
fn claude_inline_sidechain_records_stay_unattributed() {
    for case in [
        "sidechain",
        "sidechain_call",
        "sidechain_result",
        "sidechain_only",
        "agent_id",
    ] {
        imported_commit_case("claude", case);
    }
}

#[test]
fn claude_primary_records_in_a_file_with_sidechain_records_still_attribute() {
    imported_commit_case("claude", "primary_mixed_sidechain");
    imported_commit_case("claude", "primary_explicit_false");
}

#[cfg(unix)]
mod daemon_teardown {
    use super::*;
    use std::io::Read;
    use std::process::{Child, Command as StdCommand, Stdio};

    struct ReleasedChild(Child);

    impl Drop for ReleasedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "subprocess fixture for released daemon teardown"]
    fn released_child() {
        std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
    }

    fn fixture() -> (TempDir, PathBuf, PathBuf, ReleasedChild) {
        let temp = tempdir();
        let root = data_root(&temp);
        let binary = std::env::current_exe().unwrap();
        let child = ReleasedChild(
            StdCommand::new(&binary)
                .args(["--exact", "daemon_teardown::released_child", "--ignored"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        fs::create_dir_all(root.join("daemon")).unwrap();
        fs::write(
            root.join("daemon/daemon.lock"),
            json!({"released": true, "pid": child.0.id(), "owner_id": "test-owner",
                "data_root": root, "binary": binary})
            .to_string(),
        )
        .unwrap();
        fs::write(root.join("daemon/query-endpoint.json"), "{}").unwrap();
        (temp, root, binary, child)
    }

    #[test]
    fn waits_for_released_process_before_removing_artifacts() {
        for replace_lock in [false, true] {
            let (temp, root, binary, mut child) = fixture();
            std::thread::scope(|scope| {
                let (tx, rx) = std::sync::mpsc::channel();
                let (temp, root, binary) = (&temp, &root, &binary);
                scope.spawn(move || {
                    tx.send(stop_test_owned_daemon_for_binary(temp, root, binary, None))
                        .unwrap();
                });
                // Hold the released process alive until cleanup has had a
                // chance to wait. No sleep in the child controls this race.
                let early = rx.recv_timeout(Duration::from_millis(150));
                assert!(child.0.try_wait().unwrap().is_none());
                let lock = root.join("daemon/daemon.lock");
                assert!(lock.exists());
                assert!(root.join("daemon/query-endpoint.json").exists());
                let replacement = b"{\"owner_id\":\"replacement\",\"released\":false}";
                if replace_lock {
                    fs::write(&lock, replacement).unwrap();
                }
                drop(child.0.stdin.take());
                // Reap before waiting for cleanup, including on systems where
                // kill(pid, 0) succeeds for zombies.
                assert!(child.0.wait().unwrap().success());
                assert!(
                    matches!(early, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
                    "cleanup returned while the released process was alive: {early:?}"
                );
                let result = rx.recv_timeout(Duration::from_secs(10)).unwrap();
                if replace_lock {
                    assert!(result.unwrap_err().contains("lock changed during teardown"));
                    assert_eq!(fs::read(&lock).unwrap(), replacement);
                    assert!(root.join("daemon/query-endpoint.json").exists());
                } else {
                    result.unwrap();
                    assert!(!lock.exists());
                    assert!(!root.join("daemon/query-endpoint.json").exists());
                }
            });
        }
    }

    #[test]
    fn live_released_process_times_out_without_removing_artifacts() {
        let (temp, root, binary, mut child) = fixture();
        let lock = root.join("daemon/daemon.lock");
        let before = fs::read(&lock).unwrap();
        let started = Instant::now();
        let error = stop_test_owned_daemon_for_binary(&temp, &root, &binary, None).unwrap_err();
        assert!(error.contains("still identifies a live process"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "cleanup signaled the child"
        );
        assert_eq!(fs::read(&lock).unwrap(), before);
        assert!(root.join("daemon/query-endpoint.json").exists());
    }

    #[test]
    fn released_lock_identity_mismatches_preserve_artifacts() {
        let (temp, root, binary, mut child) = fixture();
        let lock = root.join("daemon/daemon.lock");
        let original: Value = serde_json::from_slice(&fs::read(&lock).unwrap()).unwrap();
        for (field, wrong, message) in [
            (
                "data_root",
                temp.path().to_path_buf(),
                "does not match test root",
            ),
            ("binary", lock.clone(), "is not the test copy"),
        ] {
            let mut value = original.clone();
            value[field] = json!(wrong);
            let bytes = serde_json::to_vec(&value).unwrap();
            fs::write(&lock, &bytes).unwrap();
            let error = stop_test_owned_daemon_for_binary(&temp, &root, &binary, None).unwrap_err();
            assert!(error.contains(message), "{error}");
            assert_eq!(fs::read(&lock).unwrap(), bytes);
            assert!(root.join("daemon/query-endpoint.json").exists());
            assert!(child.0.try_wait().unwrap().is_none());
        }
    }
}
