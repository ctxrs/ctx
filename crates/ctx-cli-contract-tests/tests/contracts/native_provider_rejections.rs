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

fn assert_unusable_source_backed_failure<'a>(
    report: &'a Value,
    provider: &str,
    source_format: &str,
    rejected_records: u64,
) -> &'a Value {
    assert_eq!(report["schema_version"], 2, "{report:#}");
    assert_eq!(report["outcome"], "failure", "{report:#}");
    assert_eq!(
        report["failure_scope"],
        if rejected_records == 0 {
            "none"
        } else {
            "record"
        },
        "{report:#}"
    );
    assert_eq!(
        report["failure_type"],
        if rejected_records == 0 {
            "none"
        } else {
            "record_rejection"
        },
        "{report:#}"
    );
    let sources = report["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("missing explicit source receipt in {report:#}"));
    assert_eq!(sources.len(), 1, "{report:#}");
    let source = &sources[0];
    assert_eq!(source["provider"], provider, "{report:#}");
    assert_eq!(source["source_format"], source_format, "{report:#}");
    assert_eq!(
        source["status"],
        if rejected_records == 0 {
            "published"
        } else {
            "partial"
        },
        "{report:#}"
    );
    assert_eq!(source["current_indexed_documents"], 0, "{report:#}");
    assert_eq!(source["current_retained_records"], 0, "{report:#}");
    assert_eq!(
        report["totals"]["rejected_records"], rejected_records,
        "{report:#}"
    );
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
    let session_meta = codex_lineage_session_meta(
        native_session_id,
        parent_native_session_id,
        advisory_session_id,
    );
    let message = codex_lineage_message(marker);
    let path = root.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(&path, format!("{session_meta}\n{message}\n")).unwrap();
    path
}

fn codex_lineage_session_meta(
    native_session_id: &str,
    parent_native_session_id: Option<&str>,
    advisory_session_id: Option<&str>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "id": native_session_id,
        "timestamp": "2026-08-07T12:00:00Z",
        "cwd": "/private/nested-lineage/workspace",
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
    serde_json::json!({
        "timestamp": "2026-08-07T12:00:00Z",
        "type": "session_meta",
        "payload": payload,
    })
}

fn codex_lineage_message(marker: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-07T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": marker}]
        }
    })
}

fn write_codex_metadata_rollout(
    root: &Path,
    native_session_id: &str,
    metadata: impl IntoIterator<Item = serde_json::Value>,
    marker: &str,
) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let path = root.join(format!("rollout-{native_session_id}.jsonl"));
    let mut body = metadata
        .into_iter()
        .map(|value| format!("{value}\n"))
        .collect::<String>();
    body.push_str(&format!("{}\n", codex_lineage_message(marker)));
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn antigravity_cli_import_skips_malformed_file_among_valid_files() {
    let temp = daemon_test_root();
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
    let temp = daemon_test_root();
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
    let route_identity = first_source["route_identity"].clone();
    let complete_records = first_source["current_complete_records"].clone();

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
        replay_source["published_generation"], replay_source["previous_generation"],
        "{replay:#}"
    );
    assert_eq!(
        replay_source["route_identity"], route_identity,
        "{replay:#}"
    );
    assert_eq!(
        replay_source["current_complete_records"], complete_records,
        "{replay:#}"
    );
}

#[test]
fn firebender_replay_preserves_mixed_rejections_and_distinguishes_all_invalid_from_empty() {
    let mixed_temp = daemon_test_root();
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

    let warm_temp = daemon_test_root();
    let marker = "firebender warm all-invalid carry-forward oracle";
    let warm_project = write_native_firebender_fixture(&warm_temp, marker);
    let warm_database = Path::new(&warm_project)
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");

    let valid = json_output(ctx(&warm_temp).args([
        "import",
        "--provider",
        "firebender",
        "--path",
        &warm_project,
        "--format=json",
        "--progress",
        "none",
    ]));
    let valid_source =
        assert_source_backed_publication(&valid, "firebender", "firebender_chat_history_sqlite", 0);
    assert_eq!(valid_source["current_source_count"], 1, "{valid:#}");
    assert_eq!(valid_source["current_indexed_documents"], 3, "{valid:#}");
    let valid_generation = valid_source["published_generation"]
        .as_str()
        .expect("published valid Firebender generation")
        .to_owned();
    let valid_search = json_output(ctx(&warm_temp).args([
        "search",
        marker,
        "--provider",
        "firebender",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&valid_search, "firebender", marker, 1, "message");

    let conn = Connection::open(&warm_database).unwrap();
    conn.execute(
        "update chat_sessions
         set messages_json = '{', updated_at = updated_at + 100",
        [],
    )
    .unwrap();
    drop(conn);

    let all_rejected = json_output(ctx(&warm_temp).args([
        "import",
        "--provider",
        "firebender",
        "--path",
        &warm_project,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(
        all_rejected["outcome"], "completed_with_source_failures",
        "{all_rejected:#}"
    );
    assert_eq!(all_rejected["failure_scope"], "source", "{all_rejected:#}");
    assert_eq!(
        all_rejected["failure_type"], "source_failure",
        "{all_rejected:#}"
    );
    assert_eq!(
        all_rejected["totals"]["failed_sources"], 1,
        "{all_rejected:#}"
    );
    assert_eq!(
        all_rejected["totals"]["current_indexed_documents"], 3,
        "{all_rejected:#}"
    );
    let rejected_source = &all_rejected["sources"][0];
    assert_eq!(
        rejected_source["provider"], "firebender",
        "{all_rejected:#}"
    );
    assert_eq!(
        rejected_source["source_format"], "firebender_chat_history_sqlite",
        "{all_rejected:#}"
    );
    assert_eq!(rejected_source["status"], "failure", "{all_rejected:#}");
    assert_eq!(
        rejected_source["failure_scope"], "source",
        "{all_rejected:#}"
    );
    assert_eq!(rejected_source["failure_type"], "other", "{all_rejected:#}");
    assert_eq!(
        rejected_source["source_failure_class"], "unreadable",
        "{all_rejected:#}"
    );
    assert_eq!(rejected_source["carried_forward"], true, "{all_rejected:#}");
    assert_eq!(
        rejected_source["source_failure_total"], 1,
        "{all_rejected:#}"
    );
    assert_eq!(
        rejected_source["route_source_failure_total"], 1,
        "{all_rejected:#}"
    );
    assert_eq!(rejected_source["successful_routes"], 0, "{all_rejected:#}");
    assert_eq!(rejected_source["change"], "no_op", "{all_rejected:#}");
    assert_eq!(
        rejected_source["generation_changed"], false,
        "{all_rejected:#}"
    );
    assert_eq!(
        rejected_source["previous_generation"], valid_generation,
        "{all_rejected:#}"
    );
    assert_eq!(
        rejected_source["published_generation"], valid_generation,
        "{all_rejected:#}"
    );
    assert_eq!(
        rejected_source["current_source_count"], 1,
        "{all_rejected:#}"
    );
    assert_eq!(
        rejected_source["current_indexed_documents"], 3,
        "{all_rejected:#}"
    );
    assert_eq!(
        rejected_source["current_retained_records"], 3,
        "{all_rejected:#}"
    );
    assert_eq!(
        rejected_source["rejected_record_total"], 0,
        "{all_rejected:#}"
    );
    assert!(rejected_source["rejection_diagnostics"]
        .as_array()
        .is_some_and(Vec::is_empty));

    let retained_search = json_output(ctx(&warm_temp).args([
        "search",
        marker,
        "--provider",
        "firebender",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&retained_search, "firebender", marker, 1, "message");
    assert_eq!(
        retained_search["retrieval"]["generation_id"], valid_generation,
        "{retained_search:#}"
    );

    let conn = Connection::open(&warm_database).unwrap();
    conn.execute("delete from chat_sessions", []).unwrap();
    drop(conn);

    let empty = json_output(ctx(&warm_temp).args([
        "import",
        "--provider",
        "firebender",
        "--path",
        &warm_project,
        "--format=json",
        "--progress",
        "none",
    ]));
    let empty_source =
        assert_source_backed_publication(&empty, "firebender", "firebender_chat_history_sqlite", 0);
    assert_eq!(empty_source["current_source_count"], 1, "{empty:#}");
    assert_eq!(empty_source["current_indexed_documents"], 0, "{empty:#}");
    assert_eq!(empty_source["current_complete_records"], 0, "{empty:#}");
    assert_eq!(empty_source["current_retained_records"], 0, "{empty:#}");
    assert_eq!(empty_source["current_rejected_records"], 0, "{empty:#}");
    assert_ne!(
        empty_source["published_generation"], valid_generation,
        "{empty:#}"
    );

    let removed_search = json_output(ctx(&warm_temp).args([
        "search",
        marker,
        "--provider",
        "firebender",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        removed_search["results"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "{removed_search:#}"
    );
    assert_eq!(
        removed_search["retrieval"]["generation_id"], empty_source["published_generation"],
        "{removed_search:#}"
    );

    assert!(
        !warm_temp.path().join("work.sqlite").exists(),
        "Firebender refreshes must not create the previous-epoch Store"
    );
    assert!(
        provider_core_records(&data_root(&warm_temp), "firebender").is_empty(),
        "a successful empty Firebender replacement must clear prior Core records"
    );
    assert!(!data_root(&warm_temp).join("relational.sqlite").exists());
}

#[test]
fn codex_mixed_session_replay_preserves_source_backed_rejection_counts() {
    let temp = daemon_test_root();
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

    fs::write(
        &session,
        concat!(
            r#"{"timestamp":"2026-07-13T12:00:00.000Z","type":"session_meta","payload":{"id":"codex-mixed-replay","timestamp":"2026-07-13T12:00:00.000Z","cwd":"/repo","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-13T12:00:03.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":{"different_failed_attempt":"#,
            "\n",
        ),
    )
    .unwrap();
    let carried = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        path,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(
        carried["outcome"], "completed_with_rejections_and_source_failures",
        "{carried:#}"
    );
    let carried_source = &carried["sources"][0];
    assert_eq!(carried_source["status"], "partial", "{carried:#}");
    assert_eq!(carried_source["carried_forward"], true, "{carried:#}");
    assert_eq!(carried_source["current_rejected_records"], 1, "{carried:#}");
    assert_eq!(carried_source["rejected_record_total"], 1, "{carried:#}");
    let carried_diagnostics = carried_source["rejection_diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("missing carried rejection diagnostics in {carried:#}"));
    assert_eq!(carried_diagnostics.len(), 1, "{carried:#}");
    assert_eq!(carried_diagnostics[0]["line"], 3, "{carried:#}");
    assert_eq!(
        carried_diagnostics[0]["class"], "malformed_record",
        "{carried:#}"
    );
    assert!(
        carried_diagnostics[0]["detail"]
            .as_str()
            .is_some_and(|detail| !detail.is_empty()),
        "the carried diagnostic must remain committed while the new failed-attempt rejection is suppressed: {carried:#}"
    );

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
fn codex_nested_root_advisory_import_is_searchable_and_showable_without_source_failures() {
    let temp = daemon_test_root();
    let sessions = temp.path().join("codex-nested-lineage-sessions");
    let root = "019fc000-0000-7000-8000-0000000032a0";
    let child = "019fc000-0000-7000-8000-0000000032a1";
    let grandchild = "019fc000-0000-7000-8000-0000000032a2";
    let great_grandchild = "019fc000-0000-7000-8000-0000000032a3";
    let root_marker = "nestedrootclicanary328";
    let child_marker = "nestedchildclicanary328";
    let grandchild_marker = "nestedgrandchildclicanary328";
    let great_grandchild_marker = "nestedgreatgrandchildclicanary328";
    write_codex_lineage_rollout(&sessions, root, None, Some(root), root_marker);
    write_codex_lineage_rollout(&sessions, child, Some(root), Some(root), child_marker);
    write_codex_lineage_rollout(
        &sessions,
        grandchild,
        Some(child),
        Some(root),
        grandchild_marker,
    );
    write_codex_lineage_rollout(
        &sessions,
        great_grandchild,
        Some(grandchild),
        Some(root),
        great_grandchild_marker,
    );

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
    let source = assert_source_backed_publication(&report, "codex", "codex_session_jsonl_tree", 0);
    assert_eq!(source["source_failure_total"], 0, "{report:#}");
    assert_eq!(source["route_source_failure_total"], 0, "{report:#}");
    assert_eq!(source["current_source_count"], 4, "{report:#}");
    assert_eq!(source["current_rejected_records"], 0, "{report:#}");
    assert!(source["rejection_diagnostics"]
        .as_array()
        .unwrap()
        .is_empty());

    let search = json_output(ctx(&temp).args([
        "search",
        great_grandchild_marker,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "codex", great_grandchild_marker, 1, "message");
    let result = &search["results"][0];
    assert_eq!(
        result["provider_session_id"], great_grandchild,
        "{result:#}"
    );
    let event_id = result["ctx_event_id"].as_str().unwrap();
    let shown =
        json_output(ctx(&temp).args(["show", "event", event_id, "--window", "1", "--format=json"]));
    assert_eq!(shown["event"]["provider_session_id"], great_grandchild);
    assert!(shown["event"]["text"]
        .as_str()
        .is_some_and(|text| text.contains(great_grandchild_marker)));
}

#[test]
fn codex_cold_import_keeps_inherited_metadata_and_quarantines_unrelated_owner() {
    let temp = daemon_test_root();
    let sessions = temp.path().join("codex-inherited-metadata-sessions");
    let owner_first = "019fc000-0000-7000-8000-0000000032b0";
    let owner_first_parent = "019fc000-0000-7000-8000-0000000032b1";
    let ancestor_first = "019fc000-0000-7000-8000-0000000032b2";
    let ancestor_first_parent = "019fc000-0000-7000-8000-0000000032b3";
    let malformed_owner = "019fc000-0000-7000-8000-0000000032b4";
    let unrelated_owner = "019fc000-0000-7000-8000-0000000032b5";
    let owner_first_marker = "ownerfirstcoldimportcanary614";
    let ancestor_first_marker = "ancestorfirstcoldimportcanary614";
    let quarantined_marker = "unrelatedownercoldimportcanary614";

    write_codex_metadata_rollout(
        &sessions,
        owner_first,
        [
            codex_lineage_session_meta(owner_first, Some(owner_first_parent), Some(owner_first)),
            codex_lineage_session_meta(owner_first_parent, None, Some(owner_first_parent)),
        ],
        owner_first_marker,
    );
    write_codex_metadata_rollout(
        &sessions,
        ancestor_first,
        [
            codex_lineage_session_meta(ancestor_first_parent, None, Some(ancestor_first_parent)),
            codex_lineage_session_meta(
                ancestor_first,
                Some(ancestor_first_parent),
                Some(ancestor_first),
            ),
        ],
        ancestor_first_marker,
    );
    let malformed_path = write_codex_metadata_rollout(
        &sessions,
        malformed_owner,
        [
            codex_lineage_session_meta(malformed_owner, None, Some(malformed_owner)),
            codex_lineage_session_meta(unrelated_owner, None, Some(unrelated_owner)),
        ],
        quarantined_marker,
    );
    fs::rename(
        malformed_path,
        sessions.join("renamed-corrupt-owner-rollout.jsonl"),
    )
    .unwrap();

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
    assert_eq!(report["schema_version"], 2, "{report:#}");
    assert_eq!(
        report["outcome"], "completed_with_source_failures",
        "{report:#}"
    );
    assert_eq!(report["failure_scope"], "source", "{report:#}");
    assert_eq!(report["failure_type"], "source_failure", "{report:#}");
    let sources = report["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("missing Codex source receipt in {report:#}"));
    assert_eq!(sources.len(), 1, "{report:#}");
    let source = &sources[0];
    assert_eq!(source["provider"], "codex", "{report:#}");
    assert_eq!(
        source["source_format"], "codex_session_jsonl_tree",
        "{report:#}"
    );
    assert_eq!(source["status"], "partial", "{report:#}");
    assert_eq!(source["source_failure_total"], 1, "{report:#}");
    assert_eq!(source["route_source_failure_total"], 1, "{report:#}");
    assert_eq!(source["carried_forward"], false, "{report:#}");
    assert_eq!(source["current_source_count"], 2, "{report:#}");

    for marker in [owner_first_marker, ancestor_first_marker] {
        let search = json_output(ctx(&temp).args([
            "search",
            marker,
            "--provider",
            "codex",
            "--refresh",
            "off",
            "--format=json",
        ]));
        assert_search_provider_oracle(&search, "codex", marker, 1, "message");
    }
    let quarantined = json_output(ctx(&temp).args([
        "search",
        quarantined_marker,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(quarantined["results"].as_array().unwrap().is_empty());
}

#[test]
fn junie_all_unknown_session_fails_with_ignored_accounting() {
    let temp = daemon_test_root();
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

    let report = failure_json_output(ctx(&temp).args([
        "import",
        "--provider",
        "junie",
        "--path",
        sessions.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    let source = assert_unusable_source_backed_failure(
        &report,
        "junie",
        "junie_session_events_jsonl_tree",
        0,
    );
    assert_eq!(source["current_source_count"], 1, "{report:#}");
    assert_eq!(source["current_indexed_documents"], 0, "{report:#}");
    assert_eq!(source["current_complete_records"], 2, "{report:#}");
    assert_eq!(source["current_retained_records"], 0, "{report:#}");
    assert_eq!(source["current_ignored_records"], 2, "{report:#}");
}

#[test]
fn auggie_unknown_node_fails_with_ignored_accounting() {
    let temp = daemon_test_root();
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

    let report = failure_json_output(ctx(&temp).args([
        "import",
        "--provider",
        "auggie",
        "--path",
        session.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    let source = assert_unusable_source_backed_failure(&report, "auggie", "auggie_session_json", 0);
    assert_eq!(source["current_source_count"], 1, "{report:#}");
    assert_eq!(source["current_indexed_documents"], 0, "{report:#}");
    assert_eq!(source["current_complete_records"], 1, "{report:#}");
    assert_eq!(source["current_retained_records"], 0, "{report:#}");
    assert_eq!(source["current_ignored_records"], 1, "{report:#}");
    assert!(provider_core_records(&data_root(&temp), "auggie").is_empty());
}

#[test]
fn codex_unknown_result_like_events_preserve_neighbors_on_fresh_and_warm_import() {
    let temp = daemon_test_root();
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
    let temp = daemon_test_root();
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
    let temp = daemon_test_root();
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
fn complete_oversize_only_codex_refresh_preserves_last_good_generation() {
    let temp = daemon_test_root();
    let session = temp.path().join("codex-all-rejected.jsonl");
    let marker = "codex JSONL all-rejected carry-forward oracle";
    let valid = concat!(
        r#"{"timestamp":"2026-07-13T12:00:00Z","type":"session_meta","payload":{"id":"codex-all-rejected","timestamp":"2026-07-13T12:00:00Z","cwd":"/repo","originator":"codex-cli"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-13T12:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"__MARKER__"}]}}"#,
        "\n"
    )
    .replace("__MARKER__", marker);
    fs::write(&session, valid).unwrap();
    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        session.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    let first_source = assert_source_backed_publication(&first, "codex", "codex_session_jsonl", 0);
    let first_generation = first_source["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();

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
        assert_eq!(
            report["outcome"], "completed_with_source_failures",
            "{report:#}"
        );
        assert_eq!(report["failure_scope"], "source", "{report:#}");
        let failed_source = &report["sources"][0];
        assert_eq!(failed_source["provider"], "codex", "{report:#}");
        assert_eq!(
            failed_source["source_format"], "codex_session_jsonl",
            "{report:#}"
        );
        assert_eq!(failed_source["status"], "partial", "{report:#}");
        assert_eq!(failed_source["carried_forward"], true, "{report:#}");
        assert_eq!(failed_source["change"], "no_op", "{report:#}");
        assert_eq!(
            failed_source["published_generation"], first_generation,
            "{report:#}"
        );
        assert_eq!(failed_source["current_indexed_documents"], 1, "{report:#}");
        assert_eq!(failed_source["current_retained_records"], 1, "{report:#}");
        assert_eq!(failed_source["rejected_record_total"], 0, "{report:#}");
    }

    assert!(
        !temp.path().join("work.sqlite").exists(),
        "Codex refreshes must not create the previous-epoch Store"
    );
    let retained = json_output(ctx(&temp).args([
        "search",
        marker,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&retained, "codex", marker, 1, "message");
    assert_eq!(
        retained["retrieval"]["generation_id"], first_generation,
        "{retained:#}"
    );
    assert!(!data_root(&temp).join("relational.sqlite").exists());
}

#[test]
fn missing_explicit_provider_source_keeps_not_found_classification() {
    let temp = daemon_test_root();
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
    let corrupt_temp = daemon_test_root();
    let corrupt_path = corrupt_temp.path().join("corrupt-warp.sqlite");
    fs::write(&corrupt_path, b"not a SQLite database").unwrap();
    let corrupt = failed_warp_report(&corrupt_temp, &corrupt_path);
    assert!(
        corrupt.contains("file is not a database") || corrupt.contains("source database"),
        "{corrupt}"
    );

    let schema_temp = daemon_test_root();
    let schema_path = schema_temp.path().join("schema-warp.sqlite");
    drop(Connection::open(&schema_path).unwrap());
    let schema = failed_warp_report(&schema_temp, &schema_path);
    assert!(schema.contains("missing required"), "{schema}");
    assert!(schema.contains("agent_conversations"), "{schema}");
}

#[cfg(unix)]
#[test]
fn symlinked_default_source_is_not_admitted_beside_a_valid_source() {
    let temp = daemon_test_root();
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
fn symlinked_default_source_is_rejected_before_inventory() {
    let temp = daemon_test_root();
    write_symlinked_claude_inventory_source(&temp);

    let error = source_refresh_failure(ctx(&temp).args([
        "import",
        "--all",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert!(
        error.contains("code=all_provider_terminal_coverage_unavailable"),
        "{error}"
    );
    assert!(
        error.contains(
            "claude the selected history path uses a symlink component; use a trusted real path with --path"
        ),
        "{error}"
    );
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
