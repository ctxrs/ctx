mod support;

use support::*;

fn assert_source_backed_publication<'a>(
    report: &'a Value,
    provider: &str,
    source_format: &str,
    rejected_records: u64,
) -> &'a Value {
    let source = if rejected_records == 0 {
        assert_explicit_source_publication(report, provider, source_format)
    } else {
        assert_eq!(report["schema_version"], 2, "{report:#}");
        let sources = report["sources"]
            .as_array()
            .unwrap_or_else(|| panic!("missing explicit source receipt in {report:#}"));
        assert_eq!(sources.len(), 1, "{report:#}");
        let source = &sources[0];
        assert_eq!(source["provider"], provider, "{report:#}");
        assert_eq!(source["source_format"], source_format, "{report:#}");
        source
    };
    assert_eq!(
        source["current_rejected_records"], rejected_records,
        "{report:#}"
    );
    assert_eq!(
        report["totals"]["current_rejected_records"], rejected_records,
        "{report:#}"
    );
    if rejected_records == 0 {
        assert_eq!(source["status"], "published", "{report:#}");
    } else {
        assert_eq!(report["outcome"], "completed_with_rejections", "{report:#}");
        assert_eq!(source["status"], "partial", "{report:#}");
        assert_eq!(source["failure_scope"], "record", "{report:#}");
        assert_eq!(source["failure_type"], "record_rejection", "{report:#}");
        assert_eq!(
            source["rejected_record_total"], rejected_records,
            "{report:#}"
        );
    }
    source
}

fn source_refresh_failure(command: &mut Command) -> String {
    let output = command.assert().failure().get_output().clone();
    assert!(
        output.stdout.is_empty(),
        "source-backed refresh failure synthesized unsupported JSON: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stderr).unwrap()
}

fn write_codex_lineage_rollout(
    root: &Path,
    native_session_id: &str,
    parent_native_session_id: Option<&str>,
    advisory_session_id: Option<&str>,
    marker: &str,
) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let mut payload = serde_json::json!({
        "id": native_session_id,
        "timestamp": "2026-08-07T12:00:00Z",
        "cwd": "/private/root-conflict/workspace",
        "originator": "codex_cli_rs",
        "cli_version": "1.0.0",
        "source": "cli",
        "model_provider": "openai"
    });
    if let Some(parent) = parent_native_session_id {
        payload["forked_from_id"] = serde_json::json!(parent);
    }
    if let Some(advisory) = advisory_session_id {
        payload["session_id"] = serde_json::json!(advisory);
    }
    let session_meta = serde_json::json!({
        "timestamp": "2026-08-07T12:00:00Z",
        "type": "session_meta",
        "payload": payload,
    });
    let message = serde_json::json!({
        "timestamp": "2026-08-07T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": marker}]
        }
    });
    let path = root.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(&path, format!("{session_meta}\n{message}\n")).unwrap();
    path
}

#[test]
fn antigravity_cli_import_skips_malformed_file_among_valid_files() {
    let temp = finite_daemon_test_root();
    let brain = write_antigravity_valid_and_malformed_file_tree(&temp);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        brain.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    let source = assert_source_backed_publication(
        &imported,
        "antigravity",
        "antigravity_cli_transcript_jsonl_tree",
        0,
    );
    assert_eq!(source["source_files"], 2, "{imported:#}");
    assert_eq!(source["current_source_count"], 1, "{imported:#}");
    assert_eq!(source["current_indexed_documents"], 3, "{imported:#}");
    assert!(
        source["current_certified_source_bytes"].as_u64() < source["source_bytes"].as_u64(),
        "{imported:#}"
    );

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["history_epoch"]["status"], "ready", "{status:#}");
    assert_eq!(status["lexical"]["indexed_documents"], 3, "{status:#}");

    let search = json_output(ctx(&temp).args([
        "search",
        "write_to_file",
        "--provider",
        "antigravity",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "antigravity", "write_to_file", 1, "tool_call");
}

#[test]
fn mixed_source_replay_remains_stable_after_malformed_file_is_skipped() {
    let temp = finite_daemon_test_root();
    let brain = write_antigravity_valid_and_malformed_file_tree(&temp);
    fs::create_dir_all(temp.path().join(".gemini/antigravity-cli/brain")).unwrap();
    fs::create_dir_all(temp.path().join(".gemini/antigravity-ide/brain")).unwrap();
    let path = brain.to_str().unwrap();

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        path,
        "--format=json",
        "--progress",
        "none",
    ]));
    let first_source = assert_source_backed_publication(
        &first,
        "antigravity",
        "antigravity_cli_transcript_jsonl_tree",
        0,
    );
    let generation = first_source["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();

    let replay = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        path,
        "--resume",
        "--format=json",
        "--progress",
        "none",
    ]));
    let replay_source = assert_source_backed_publication(
        &replay,
        "antigravity",
        "antigravity_cli_transcript_jsonl_tree",
        0,
    );
    assert_eq!(replay_source["change"], "no_op", "{replay:#}");
    assert_eq!(replay_source["generation_changed"], false, "{replay:#}");
    assert_eq!(
        replay_source["published_generation"], generation,
        "{replay:#}"
    );
}

#[test]
fn firebender_replay_preserves_mixed_and_all_invalid_outcomes() {
    let mixed_temp = finite_daemon_test_root();
    let mixed_project =
        write_native_firebender_fixture(&mixed_temp, "firebender mixed rejection replay oracle");
    let mixed_database = Path::new(&mixed_project)
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    let conn = Connection::open(&mixed_database).unwrap();
    conn.execute(
        "update chat_sessions set updated_at = 20 where id = 'firebender-fixture-session'",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into chat_sessions
         (id, name, created_at, updated_at, messages_json, metadata_json)
         values (?1, ?2, ?3, ?4, ?5, ?6)",
        params!["firebender-invalid", "invalid", 9_i64, 10_i64, "{", "{}"],
    )
    .unwrap();
    drop(conn);

    let mut mixed_generation = None;
    for resume in [false, true] {
        let mut command = ctx(&mixed_temp);
        command.args([
            "import",
            "--provider",
            "firebender",
            "--path",
            &mixed_project,
            "--format=json",
            "--progress",
            "none",
        ]);
        if resume {
            command.arg("--resume");
        }
        let report = json_output(&mut command);
        let source = assert_source_backed_publication(
            &report,
            "firebender",
            "firebender_chat_history_sqlite",
            1,
        );
        assert_eq!(source["current_sources_with_rejections"], 1, "{report:#}");
        assert_eq!(source["current_indexed_documents"], 3, "{report:#}");
        let published_generation = source["published_generation"]
            .as_str()
            .expect("published Firebender generation");
        if resume {
            assert_eq!(source["change"], "no_op", "{report:#}");
            assert_eq!(source["generation_changed"], false, "{report:#}");
            assert_eq!(
                Some(published_generation),
                mixed_generation.as_deref(),
                "{report:#}"
            );
        } else {
            let change = source["change"].as_str().expect("change classification");
            assert!(matches!(change, "changed" | "no_op"), "{report:#}");
            assert_eq!(
                source["generation_changed"],
                change == "changed",
                "{report:#}"
            );
            if change == "no_op" {
                assert_eq!(
                    source["previous_generation"], source["published_generation"],
                    "{report:#}"
                );
            }
            mixed_generation = Some(published_generation.to_owned());
        }
    }

    let invalid_temp = finite_daemon_test_root();
    let invalid_project =
        write_native_firebender_fixture(&invalid_temp, "unused all-invalid oracle");
    let invalid_database = Path::new(&invalid_project)
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    let conn = Connection::open(&invalid_database).unwrap();
    conn.execute("update chat_sessions set messages_json = '{'", [])
        .unwrap();
    drop(conn);

    for resume in [false, true] {
        let mut command = ctx(&invalid_temp);
        command.args([
            "import",
            "--provider",
            "firebender",
            "--path",
            &invalid_project,
            "--format=json",
            "--progress",
            "none",
        ]);
        if resume {
            command.arg("--resume");
        }
        let report = json_output(&mut command);
        let source = assert_source_backed_publication(
            &report,
            "firebender",
            "firebender_chat_history_sqlite",
            1,
        );
        assert_eq!(source["current_source_count"], 1, "{report:#}");
        assert_eq!(source["current_indexed_documents"], 0, "{report:#}");
        assert_eq!(source["current_retained_records"], 0, "{report:#}");
    }

    assert!(
        !invalid_temp.path().join("work.sqlite").exists(),
        "an all-invalid provider source must not create the previous-epoch Store"
    );
    assert!(
        provider_core_records(&data_root(&invalid_temp), "firebender").is_empty(),
        "an all-invalid source must publish no Core records"
    );
    assert!(!data_root(&invalid_temp).join("relational.sqlite").exists());
}

#[test]
fn codex_mixed_session_replay_preserves_source_backed_rejection_counts() {
    let temp = finite_daemon_test_root();
    let session = temp.path().join("codex-mixed-replay.jsonl");
    fs::write(
        &session,
        concat!(
            r#"{"timestamp":"2026-07-13T12:00:00.000Z","type":"session_meta","payload":{"id":"codex-mixed-replay","timestamp":"2026-07-13T12:00:00.000Z","cwd":"/repo","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-13T12:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"codex mixed replay oracle"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-07-13T12:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":["#,
            "\n",
        ),
    )
    .unwrap();
    let path = session.to_str().unwrap();

    for resume in [false, true] {
        let mut command = ctx(&temp);
        command.args([
            "import",
            "--provider",
            "codex",
            "--path",
            path,
            "--format=json",
            "--progress",
            "none",
        ]);
        if resume {
            command.arg("--resume");
        }
        let report = json_output(&mut command);
        let source = assert_source_backed_publication(&report, "codex", "codex_session_jsonl", 1);
        assert_eq!(source["current_sources_with_rejections"], 1, "{report:#}");
        assert_eq!(source["current_indexed_documents"], 1, "{report:#}");
        if resume {
            assert_eq!(source["change"], "no_op", "{report:#}");
        } else {
            assert!(
                matches!(source["change"].as_str(), Some("changed" | "no_op")),
                "the persistent daemon may publish the first generation before import: {report:#}"
            );
        }
    }

    let search = json_output(ctx(&temp).args([
        "search",
        "codex mixed replay oracle",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "codex", "codex mixed replay oracle", 1, "message");
}

#[test]
fn codex_root_conflict_cli_reports_path_safe_source_failure_and_publishes_peer() {
    let temp = finite_daemon_test_root();
    let sessions = temp.path().join("private-codex-root-conflict-sessions");
    let root_a = "019fc000-0000-7000-8000-0000000032a0";
    let child_a = "019fc000-0000-7000-8000-0000000032a1";
    let root_b = "019fc000-0000-7000-8000-0000000032b0";
    let private_root_marker = "privaterootaclicanary328";
    let private_child_marker = "privatechildaclicanary328";
    let valid_marker = "validrootbclicanary328";
    write_codex_lineage_rollout(&sessions, root_a, None, None, private_root_marker);
    write_codex_lineage_rollout(
        &sessions,
        child_a,
        Some(root_a),
        Some(root_b),
        private_child_marker,
    );
    write_codex_lineage_rollout(&sessions, root_b, None, None, valid_marker);

    let report = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        sessions.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(
        report["outcome"], "completed_with_source_failures",
        "{report:#}"
    );
    assert_eq!(report["failure_scope"], "source", "{report:#}");
    let [source] = report["sources"].as_array().unwrap().as_slice() else {
        panic!("one explicit Codex receipt expected: {report:#}");
    };
    assert_eq!(source["status"], "partial", "{report:#}");
    assert_eq!(source["failure_scope"], "source", "{report:#}");
    assert_eq!(source["route_source_failure_total"], 2, "{report:#}");
    assert_eq!(source["current_source_count"], 1, "{report:#}");
    assert_eq!(source["current_rejected_records"], 0, "{report:#}");
    assert!(source["rejection_diagnostics"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(source["source_selector"]
        .as_str()
        .is_some_and(|selector| selector.starts_with("logical-source:")));
    let detail = source["detail"].as_str().unwrap();
    for expected in [
        format!("computed_root_native_session_id={root_a}"),
        format!("conflicting_advisory_session_id={root_b}"),
        format!("evidence_source_record=session_meta:{child_a}"),
        format!("computed_root_source_record=session_meta:{root_a}"),
        format!("advisory_source_record=session_meta:{root_b}"),
    ] {
        assert!(detail.contains(&expected), "{detail}");
    }
    assert!(!detail.contains(sessions.to_str().unwrap()));
    assert!(!detail.contains(private_root_marker));
    assert!(!detail.contains(private_child_marker));

    let search = json_output(ctx(&temp).args([
        "search",
        valid_marker,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "codex", valid_marker, 1, "message");
}

#[test]
fn codex_many_root_conflicts_cli_reports_exact_total_and_bounded_omission() {
    const CONFLICTING_CHILDREN: usize = 65;
    const REJECTED_SOURCES: usize = CONFLICTING_CHILDREN + 1;

    let temp = finite_daemon_test_root();
    let sessions = temp.path().join(".codex/sessions/2026/08/07");
    let root_a = "019fc000-0000-7000-8000-000000003400";
    let root_b = "019fc000-0000-7000-8000-0000000034b0";
    let evidence_child = "019fc000-0000-7000-8001-000000000000";
    let private_marker = "private bounded CLI root conflict content 328";
    let valid_marker = "bounded CLI root conflict valid peer 328";
    write_codex_lineage_rollout(&sessions, root_a, None, None, private_marker);
    for index in 0..CONFLICTING_CHILDREN {
        let child = format!("019fc000-0000-7000-8001-{index:012x}");
        let advisory = if index == 0 { root_b } else { root_a };
        write_codex_lineage_rollout(
            &sessions,
            &child,
            Some(root_a),
            Some(advisory),
            private_marker,
        );
    }
    write_codex_lineage_rollout(&sessions, root_b, None, None, valid_marker);

    let report =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_eq!(
        report["outcome"], "completed_with_source_failures",
        "{report:#}"
    );
    assert_eq!(report["failure_scope"], "source", "{report:#}");
    assert_eq!(report["totals"]["current_source_count"], 1, "{report:#}");
    assert_eq!(
        report["totals"]["current_rejected_records"], 0,
        "{report:#}"
    );
    let sources = report["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 4, "summary plus three bounded diagnostics");
    let summary = &sources[0];
    assert_eq!(summary["status"], "partial", "{report:#}");
    assert_eq!(summary["failure_scope"], "source", "{report:#}");
    assert_eq!(summary["failure_type"], "source_failure", "{report:#}");
    assert_eq!(summary["source_failure_total"], REJECTED_SOURCES);
    assert_eq!(summary["source_failures_omitted"], REJECTED_SOURCES - 3);
    assert_eq!(summary["rejected_record_total"], 0);
    assert_eq!(summary["current_rejected_records"], 0);
    for source in &sources[1..] {
        assert_eq!(source["failure_scope"], "source", "{source:#}");
        assert_eq!(source["failure_type"], "other", "{source:#}");
        assert!(source["source_selector"]
            .as_str()
            .is_some_and(|selector| selector.starts_with("logical-source:")));
        let detail = source["detail"].as_str().unwrap();
        for expected in [
            format!("computed_root_native_session_id={root_a}"),
            format!("conflicting_advisory_session_id={root_b}"),
            format!("evidence_source_record=session_meta:{evidence_child}"),
            format!("computed_root_source_record=session_meta:{root_a}"),
            format!("advisory_source_record=session_meta:{root_b}"),
        ] {
            assert!(detail.contains(&expected), "{detail}");
        }
        assert!(!detail.contains(sessions.to_str().unwrap()));
        assert!(!detail.contains(private_marker));
    }

    let search = json_output(ctx(&temp).args([
        "search",
        valid_marker,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "codex", valid_marker, 1, "message");
}

#[test]
fn codex_all_invalid_root_conflict_cli_failure_includes_typed_proof() {
    let temp = finite_daemon_test_root();
    let sessions = temp.path().join("private-codex-root-conflict-sessions");
    let root = "019fc000-0000-7000-8000-0000000032c0";
    let child = "019fc000-0000-7000-8000-0000000032c1";
    let missing_advisory = "019fc000-0000-7000-8000-0000000032ff";
    let private_marker = "private all-invalid CLI message content";
    write_codex_lineage_rollout(&sessions, root, None, None, private_marker);
    write_codex_lineage_rollout(
        &sessions,
        child,
        Some(root),
        Some(missing_advisory),
        private_marker,
    );

    let stderr = source_refresh_failure(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        sessions.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    for expected in [
        format!("computed_root_native_session_id={root}"),
        format!("conflicting_advisory_session_id={missing_advisory}"),
        format!("evidence_source_record=session_meta:{child}"),
        format!("computed_root_source_record=session_meta:{root}"),
        "advisory_source_record=unavailable".to_owned(),
    ] {
        assert!(stderr.contains(&expected), "{stderr}");
    }
    let diagnostic = stderr
        .split_once("first rejection diagnostic:")
        .and_then(|(_, diagnostic)| diagnostic.lines().next())
        .expect("all-invalid Codex failure must identify its projected diagnostic");
    assert!(!diagnostic.contains(sessions.to_str().unwrap()), "{stderr}");
    assert!(!stderr.contains(private_marker), "{stderr}");
}

#[test]
fn junie_all_unknown_session_is_published_with_ignored_accounting() {
    let temp = finite_daemon_test_root();
    let sessions = temp.path().join("junie-all-unknown");
    let session_id = "session-unknown-247";
    let session = sessions.join(session_id);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        sessions.join("index.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "sessionId": session_id,
                "createdAt": 1_786_000_000_000_i64,
                "projectDir": "/workspace/junie-unknown"
            })
        ),
    )
    .unwrap();
    fs::write(
        session.join("events.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::json!({"kind": "FutureTopLevelEvent", "value": 1}),
            serde_json::json!({
                "kind": "SessionA2uxEvent",
                "event": {"agentEvent": {"kind": "FutureAgentEvent", "value": 2}}
            })
        ),
    )
    .unwrap();

    let report = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "junie",
        "--path",
        sessions.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    let source =
        assert_source_backed_publication(&report, "junie", "junie_session_events_jsonl_tree", 0);
    assert_eq!(source["current_source_count"], 1, "{report:#}");
    assert_eq!(source["current_indexed_documents"], 0, "{report:#}");
    assert_eq!(source["current_complete_records"], 2, "{report:#}");
    assert_eq!(source["current_retained_records"], 0, "{report:#}");
    assert_eq!(source["current_ignored_records"], 2, "{report:#}");
}

#[test]
fn auggie_unknown_node_is_published_with_ignored_accounting() {
    let temp = finite_daemon_test_root();
    let session = temp.path().join("auggie-unknown-node.json");
    let unknown_body = "auggie-unknown-node-body-must-not-be-indexed";
    fs::write(
        &session,
        serde_json::to_vec(&serde_json::json!({
            "sessionId": "auggie-unknown-node-247",
            "created": "2026-07-04T20:00:00Z",
            "chatHistory": [{
                "exchange": {
                    "request_nodes": [{
                        "type": 71,
                        "text_node": {"content": unknown_body}
                    }]
                }
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let report = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "auggie",
        "--path",
        session.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    let source = assert_source_backed_publication(&report, "auggie", "auggie_session_json", 0);
    assert_eq!(source["current_source_count"], 1, "{report:#}");
    assert_eq!(source["current_indexed_documents"], 0, "{report:#}");
    assert_eq!(source["current_complete_records"], 1, "{report:#}");
    assert_eq!(source["current_retained_records"], 0, "{report:#}");
    assert_eq!(source["current_ignored_records"], 1, "{report:#}");
    assert!(provider_core_records(&data_root(&temp), "auggie").is_empty());
}

#[test]
fn codex_unknown_result_like_events_preserve_neighbors_on_fresh_and_warm_import() {
    let temp = finite_daemon_test_root();
    let session = temp.path().join("codex-unknown-result-like.jsonl");
    let binary_body = "iVBORw0KGgo=issue-247-cli-body";
    let future_body = "future-cli-result-body-must-not-be-indexed";
    fs::write(
        &session,
        format!(
            concat!(
                r#"{{"timestamp":"2026-05-31T20:33:07.000Z","type":"session_meta","payload":{{"id":"codex-unknown-result-like","timestamp":"2026-05-31T20:33:07.000Z","cwd":"/tmp/repro","originator":"codex_cli_rs","cli_version":"0.0.0","source":"cli","model_provider":"openai"}}}}"#,
                "\n",
                r#"{{"timestamp":"2026-05-31T20:33:08.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"issue 247 valid before unknown"}}]}}}}"#,
                "\n",
                r#"{{"timestamp":"2026-05-31T20:33:10.000Z","type":"event_msg","payload":{{"type":"image_generation_end","call_id":"ig_repro","status":"generating","revised_prompt":"a red square","result":"{binary_body}","saved_path":"/tmp/repro/img.png"}}}}"#,
                "\n",
                r#"{{"timestamp":"2026-05-31T20:33:11.000Z","type":"response_item","payload":{{"type":"future_tool_result","result":"{future_body}"}}}}"#,
                "\n",
                r#"{{"timestamp":"2026-05-31T20:33:12.000Z","type":"event_msg","payload":{{"type":"future_tool_response","output":"{future_body}"}}}}"#,
                "\n",
                r#"{{"timestamp":"2026-05-31T20:33:13.000Z","type":"event_msg","payload":{{"type":"future_tool_end","result":"{future_body}"}}}}"#,
                "\n",
                r#"{{"timestamp":"2026-05-31T20:33:14.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"issue 247 valid after unknown"}}]}}}}"#,
                "\n"
            ),
            binary_body = binary_body,
            future_body = future_body,
        ),
    )
    .unwrap();
    let path = session.to_str().unwrap();

    let mut generation = None;
    for resume in [false, true] {
        let mut command = ctx(&temp);
        command.args([
            "import",
            "--provider",
            "codex",
            "--path",
            path,
            "--format=json",
            "--progress",
            "none",
        ]);
        if resume {
            command.arg("--resume");
        }
        let report = json_output(&mut command);
        let source = assert_source_backed_publication(&report, "codex", "codex_session_jsonl", 0);
        assert_eq!(source["current_indexed_documents"], 2, "{report:#}");
        assert_eq!(source["current_complete_records"], 7, "{report:#}");
        assert_eq!(source["current_retained_records"], 2, "{report:#}");
        assert_eq!(source["current_ignored_records"], 5, "{report:#}");
        if let Some(generation) = generation.as_ref() {
            assert_eq!(source["change"], "no_op", "{report:#}");
            assert_eq!(source["generation_changed"], false, "{report:#}");
            assert_eq!(source["published_generation"], *generation, "{report:#}");
        } else {
            generation = source["published_generation"].as_str().map(str::to_owned);
        }
    }

    for query in [
        "issue 247 valid before unknown",
        "issue 247 valid after unknown",
    ] {
        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            "codex",
            "--refresh",
            "off",
            "--format=json",
        ]));
        assert_search_provider_oracle(&search, "codex", query, 1, "message");
    }
    let records = provider_core_records(&data_root(&temp), "codex");
    let encoded = serde_json::to_string(&records).unwrap();
    assert!(!encoded.contains(binary_body));
    assert!(!encoded.contains(future_body));
}

#[test]
fn corrected_manifested_file_retries_rejected_row_idempotently() {
    let temp = finite_daemon_test_root();
    let project = temp.path().join("claude-project");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(temp.path().join(".claude/projects")).unwrap();
    let session = project.join("manifest-retry.jsonl");
    let valid_user = r#"{"sessionId":"manifest-retry","timestamp":"2026-07-13T12:00:00Z","cwd":"/repo","version":"test","type":"user","message":{"role":"user","content":[{"type":"text","text":"manifest retry valid row"}]},"uuid":"manifest-retry-1"}"#;
    fs::write(&session, format!("{valid_user}\n{{\"type\":\n")).unwrap();
    let path = project.to_str().unwrap();

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "claude",
        "--path",
        path,
        "--format=json",
        "--progress",
        "none",
    ]));
    let first_source =
        assert_source_backed_publication(&first, "claude", "claude_projects_jsonl_tree", 1);
    assert_eq!(first_source["current_indexed_documents"], 1, "{first:#}");
    assert_eq!(first_source["current_ignored_records"], 0, "{first:#}");
    let first_generation = first_source["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();

    let valid_assistant = r#"{"sessionId":"manifest-retry","timestamp":"2026-07-13T12:00:01Z","cwd":"/repo","version":"test","type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"manifest retry corrected row"}]},"uuid":"manifest-retry-2"}"#;
    fs::write(&session, format!("{valid_user}\n{valid_assistant}\n")).unwrap();
    let corrected = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "claude",
        "--path",
        path,
        "--format=json",
        "--progress",
        "none",
    ]));
    let corrected_source =
        assert_source_backed_publication(&corrected, "claude", "claude_projects_jsonl_tree", 0);
    assert_ne!(
        corrected_source["published_generation"], first_generation,
        "{corrected:#}"
    );
    assert!(
        corrected_source["current_indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "{corrected:#}"
    );

    let search = json_output(ctx(&temp).args([
        "search",
        "manifest retry corrected row",
        "--provider",
        "claude",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(
        &search,
        "claude",
        "manifest retry corrected row",
        1,
        "message",
    );
}

#[test]
fn all_invalid_source_reports_daemon_owned_failure_and_exits_nonzero() {
    let temp = finite_daemon_test_root();
    let brain = temp.path().join("brain");
    let bad_logs = brain.join("agy-bad").join(".system_generated").join("logs");
    fs::create_dir_all(&bad_logs).unwrap();
    fs::write(bad_logs.join("transcript_full.jsonl"), "{\"step_index\":\n").unwrap();

    let stderr = source_refresh_failure(ctx(&temp).args([
        "import",
        "--provider",
        "antigravity",
        "--path",
        brain.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert!(
        stderr.contains("daemon-owned source-backed refresh failed"),
        "{stderr}"
    );
    assert!(stderr.contains("antigravity"), "{stderr}");
    assert!(stderr.contains("rejected 1 records"), "{stderr}");
}

#[test]
fn complete_oversize_only_codex_session_reports_source_backed_rejection() {
    let temp = finite_daemon_test_root();
    let session = temp.path().join("codex-all-rejected.jsonl");
    let mut source = concat!(
        r#"{"timestamp":"2026-07-13T12:00:00Z","type":"session_meta","payload":{"id":"codex-all-rejected","timestamp":"2026-07-13T12:00:00Z","cwd":"/repo","originator":"codex-cli"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-13T12:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":""#,
    )
    .to_owned();
    source.push_str(&"x".repeat(16 * 1024 * 1024));
    source.push_str("\"}]}}\n");
    fs::write(&session, source).unwrap();

    for resume in [false, true] {
        let mut command = ctx(&temp);
        command.args([
            "import",
            "--provider",
            "codex",
            "--path",
            session.to_str().unwrap(),
            "--format=json",
            "--progress",
            "none",
        ]);
        if resume {
            command.arg("--resume");
        }
        let report = json_output(&mut command);
        let source = assert_source_backed_publication(&report, "codex", "codex_session_jsonl", 1);
        assert_eq!(source["current_indexed_documents"], 0, "{report:#}");
        assert_eq!(source["current_retained_records"], 0, "{report:#}");
        assert_eq!(source["current_sources_with_rejections"], 1, "{report:#}");
    }

    assert!(
        !temp.path().join("work.sqlite").exists(),
        "an all-rejected provider source must not create the previous-epoch Store"
    );
    assert!(
        provider_core_records(&data_root(&temp), "codex").is_empty(),
        "an all-rejected source must publish no Core records"
    );
    assert!(!data_root(&temp).join("relational.sqlite").exists());
}

#[test]
fn missing_explicit_provider_source_keeps_not_found_classification() {
    let temp = finite_daemon_test_root();
    let missing = temp.path().join("missing-history.jsonl");
    let stderr = source_refresh_failure(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        missing.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert!(
        stderr.contains("not found") || stderr.contains("No such file"),
        "{stderr}"
    );
}

fn failed_warp_report(temp: &TempDir, path: &Path) -> String {
    source_refresh_failure(ctx(temp).args([
        "import",
        "--provider",
        "warp",
        "--path",
        path.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]))
}

#[test]
fn warp_native_source_failures_keep_cli_classification() {
    let corrupt_temp = finite_daemon_test_root();
    let corrupt_path = corrupt_temp.path().join("corrupt-warp.sqlite");
    fs::write(&corrupt_path, b"not a SQLite database").unwrap();
    let corrupt = failed_warp_report(&corrupt_temp, &corrupt_path);
    assert!(
        corrupt.contains("file is not a database") || corrupt.contains("source database"),
        "{corrupt}"
    );

    let schema_temp = finite_daemon_test_root();
    let schema_path = schema_temp.path().join("schema-warp.sqlite");
    drop(Connection::open(&schema_path).unwrap());
    let schema = failed_warp_report(&schema_temp, &schema_path);
    assert!(schema.contains("missing required"), "{schema}");
    assert!(schema.contains("agent_conversations"), "{schema}");
}

#[cfg(unix)]
#[test]
fn symlinked_default_source_is_not_admitted_beside_a_valid_source() {
    let temp = finite_daemon_test_root();
    write_codex_inventory_oracle(&temp);
    write_symlinked_claude_inventory_source(&temp);

    let report =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));

    assert_eq!(report["outcome"], "success", "{report:#}");
    assert_eq!(report["totals"]["current_source_count"], 1, "{report:#}");
    assert_eq!(
        report["totals"]["current_indexed_documents"], 1,
        "{report:#}"
    );
    assert_eq!(report["sources"][0]["current_rejected_records"], 0);
    assert!(!report["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source["provider"] == "claude"));
}

#[cfg(unix)]
#[test]
fn symlinked_default_source_is_excluded_before_inventory() {
    let temp = finite_daemon_test_root();
    write_symlinked_claude_inventory_source(&temp);

    let report =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_eq!(report["outcome"], "success", "{report:#}");
    assert_eq!(report["totals"]["current_source_count"], 0, "{report:#}");
    assert_eq!(
        report["totals"]["current_indexed_documents"], 0,
        "{report:#}"
    );

    let sources =
        json_output(ctx(&temp).args(["sources", "--provider", "claude", "--format=json"]));
    assert_eq!(sources["sources"], json!([]), "{sources:#}");
}

#[test]
fn mixed_import_analytics_reports_only_coarse_rejection_outcome() {
    let temp = tempdir();
    let brain = write_antigravity_valid_and_malformed_file_tree(&temp);
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    fs::create_dir_all(&home).unwrap();
    bind_test_ctx_binary(&temp);
    let _daemon = start_source_refresh_daemon(&temp, &data_root, &home, &state);

    let output = ctx(&temp)
        .args([
            "import",
            "--provider",
            "antigravity",
            "--path",
            brain.to_str().unwrap(),
            "--format=json",
            "--progress",
            "none",
        ])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENABLED", "1")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success()
        .get_output()
        .clone();

    assert!(
        events_path.exists(),
        "analytics event was not written; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let event = read_analytics_events(&events_path).remove(0);
    assert_operation_event(&event, "import", "success");
    let properties = analytics_event_properties(&event);
    assert_eq!(properties["import_outcome"], "success");
    assert_eq!(properties["import_failure_scope"], "none");
    assert_eq!(properties["import_failure_type"], "none");
    assert!(
        properties.get("rejected_records").is_none(),
        "{properties:#?}"
    );
    assert_analytics_properties_are_allowlisted(properties);
    let refresh = event["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_name"] == "provider_refresh_completed")
        .unwrap_or_else(|| panic!("analytics batch has no provider refresh event: {event:#}"));
    assert_eq!(refresh["properties"]["refresh_result"], "complete");
    assert_eq!(refresh["properties"]["failure_scope"], "none");
    assert_eq!(refresh["properties"]["failure_type"], "none");
    let encoded = serde_json::to_string(&event).unwrap();
    assert!(!encoded.contains(brain.to_str().unwrap()), "{encoded}");
    assert!(!encoded.contains("agy-bad"), "{encoded}");
}

fn write_antigravity_valid_and_malformed_file_tree(temp: &TempDir) -> PathBuf {
    let brain = temp.path().join("brain");
    write_antigravity_valid_and_malformed_file_tree_at(&brain);
    brain
}

fn write_antigravity_valid_and_malformed_file_tree_at(brain: &Path) {
    let source_fixture = PathBuf::from(provider_history_fixture("antigravity/v1/brain"));
    let valid_logs = brain
        .join("agy-success")
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&valid_logs).unwrap();
    fs::copy(
        source_fixture
            .join("agy-success")
            .join(".system_generated")
            .join("logs")
            .join("transcript_full.jsonl"),
        valid_logs.join("transcript_full.jsonl"),
    )
    .unwrap();

    let bad_logs = brain.join("agy-bad").join(".system_generated").join("logs");
    fs::create_dir_all(&bad_logs).unwrap();
    fs::write(bad_logs.join("transcript_full.jsonl"), "{\"step_index\":\n").unwrap();
}

#[cfg(unix)]
fn write_codex_inventory_oracle(temp: &TempDir) {
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/07/13");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-inventory-oracle.jsonl"),
        concat!(
            r#"{"timestamp":"2026-07-13T12:00:00.000Z","type":"session_meta","payload":{"id":"inventory-oracle","timestamp":"2026-07-13T12:00:00.000Z","cwd":"/repo","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-13T12:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"inventory isolation oracle"}]}}"#,
            "\n",
        ),
    )
    .unwrap();
}

#[cfg(unix)]
fn write_symlinked_claude_inventory_source(temp: &TempDir) {
    let target = temp.path().join("claude-projects-target");
    fs::create_dir_all(&target).unwrap();
    fs::write(
        target.join("symlinked-session.jsonl"),
        r#"{"sessionId":"symlinked","type":"user","message":{"role":"user","content":"inventory failure"}}"#,
    )
    .unwrap();
    let claude = temp.path().join(".claude");
    fs::create_dir_all(&claude).unwrap();
    std::os::unix::fs::symlink(target, claude.join("projects")).unwrap();
}
