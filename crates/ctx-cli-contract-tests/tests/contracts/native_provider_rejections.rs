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

#[test]
fn partial_source_import_keeps_cli_classification_and_searchable_records() {
    let temp = daemon_test_root();
    let session = temp.path().join("partial-source.jsonl");
    fs::write(
        &session,
        concat!(
            r#"{"timestamp":"2026-07-13T12:00:00.000Z","type":"session_meta","payload":{"id":"partial-source","timestamp":"2026-07-13T12:00:00.000Z","cwd":"/repo","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-13T12:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"partial source classification oracle"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-07-13T12:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":["#,
            "\n",
        ),
    )
    .unwrap();

    let report = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        session.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    let source = assert_source_backed_publication(&report, "codex", "codex_session_jsonl", 1);
    assert_eq!(source["current_source_count"], 1, "{report:#}");
    assert_eq!(source["current_sources_with_rejections"], 1, "{report:#}");
    assert_eq!(source["current_indexed_documents"], 1, "{report:#}");
    let diagnostics = source["rejection_diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("missing rejection diagnostics in {report:#}"));
    assert_eq!(diagnostics.len(), 1, "{report:#}");
    assert!(diagnostics[0]["line"].is_u64(), "{report:#}");
    assert!(diagnostics[0]["class"].is_string(), "{report:#}");
    assert!(
        diagnostics[0]["detail"]
            .as_str()
            .is_some_and(|detail| !detail.is_empty()),
        "{report:#}"
    );

    let search = json_output(ctx(&temp).args([
        "search",
        "partial source classification oracle",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(
        &search,
        "codex",
        "partial source classification oracle",
        1,
        "message",
    );
}

#[test]
fn all_invalid_source_keeps_cli_classification() {
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
fn failed_refresh_preserves_last_good_generation() {
    let temp = daemon_test_root();
    let session = temp.path().join("retained-generation.jsonl");
    let marker = "retained generation classification oracle";
    let valid = concat!(
        r#"{"timestamp":"2026-07-13T12:00:00Z","type":"session_meta","payload":{"id":"retained-generation","timestamp":"2026-07-13T12:00:00Z","cwd":"/repo","originator":"codex-cli"}}"#,
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
        r#"{"timestamp":"2026-07-13T12:00:00Z","type":"session_meta","payload":{"id":"retained-generation","timestamp":"2026-07-13T12:00:00Z","cwd":"/repo","originator":"codex-cli"}}"#,
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
}

#[test]
fn missing_explicit_source_keeps_cli_classification() {
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

#[test]
fn firebender_mixed_rows_publish_and_replay_stably() {
    let temp = daemon_test_root();
    let project = write_native_firebender_fixture(&temp, "Firebender mixed parser oracle");
    let database = Path::new(&project)
        .join(".idea")
        .join("firebender")
        .join("chat_history.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "insert into chat_sessions
             (id, name, created_at, updated_at, messages_json, metadata_json)
             values (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "firebender-malformed",
                "malformed",
                9_i64,
                10_i64,
                "{",
                "{}"
            ],
        )
        .unwrap();
    drop(connection);

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "firebender",
        "--path",
        &project,
        "--format=json",
        "--progress",
        "none",
    ]));
    let first_source =
        assert_source_backed_publication(&first, "firebender", "firebender_chat_history_sqlite", 1);
    assert_eq!(
        first_source["current_sources_with_rejections"], 1,
        "{first:#}"
    );
    assert_eq!(first_source["current_indexed_documents"], 3, "{first:#}");
    let first_generation = first_source["published_generation"].clone();

    let replay = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "firebender",
        "--path",
        &project,
        "--resume",
        "--format=json",
        "--progress",
        "none",
    ]));
    let replay_source = assert_source_backed_publication(
        &replay,
        "firebender",
        "firebender_chat_history_sqlite",
        1,
    );
    assert_eq!(replay_source["change"], "no_op", "{replay:#}");
    assert_eq!(replay_source["generation_changed"], false, "{replay:#}");
    assert_eq!(
        replay_source["published_generation"], first_generation,
        "{replay:#}"
    );
    assert_eq!(replay_source["current_indexed_documents"], 3, "{replay:#}");
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
fn warp_database_failures_keep_cli_classification() {
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
fn unsafe_default_source_is_rejected_before_inventory() {
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
