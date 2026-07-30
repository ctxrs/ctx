#[test]
fn search_refresh_codex_generation_covers_full_source_lifecycle() {
    let temp = tempdir();
    let sessions = temp.path().join(".codex").join("sessions");
    let session = sessions.join("2026/07/29/lifecycle.jsonl");
    let sibling_session = sessions.join("2026/07/29/sibling.jsonl");
    let native_session_id = "019fac90-0000-7000-8000-000000000001";
    let cold_query = "cold-source-lifecycle-oracle";
    write_codex_session(
        &session,
        native_session_id,
        &[("2026-07-29T12:00:01.000Z", "user", cold_query)],
    );
    write_codex_session(
        &sibling_session,
        "019fac90-0000-7000-8000-000000000002",
        &[(
            "2026-07-29T12:00:01.000Z",
            "assistant",
            "certified-deletion-sibling-oracle",
        )],
    );
    let _daemon = start_source_refresh_daemon(&temp);

    let cold = json_output(ctx(&temp).args([
        "search",
        cold_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &cold, "codex", cold_query, 1, "message");
    let cold_generation = assert_published_generation(&cold, "wait");
    assert_eq!(cold["retrieval"]["indexed_documents"], 2, "{cold:#}");
    let cold_status = assert_daemon_publication(&temp, &cold_generation, 1, &["codex", "codex"]);
    assert_eq!(
        cold_status["history_epoch"]["lexical_generation_id"], cold_generation,
        "{cold_status:#}"
    );
    let (cold_manifest, _) = generation_manifest(&temp, &cold_generation);
    assert_eq!(cold_manifest.sources.len(), 2);
    assert!(cold_manifest.removals.is_empty());

    let unchanged = json_output(ctx(&temp).args([
        "search",
        cold_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &unchanged, "codex", cold_query, 1, "message");
    assert_eq!(generation_id(&unchanged), cold_generation, "{unchanged:#}");
    let unchanged_status =
        assert_daemon_publication(&temp, &cold_generation, 1, &["codex", "codex"]);
    assert_eq!(
        unchanged_status["daemon"]["jobs"]["source_backed_refresh"]["generation_changed"], false,
        "{unchanged_status:#}"
    );

    let append_query = "append-source-lifecycle-oracle";
    append_codex_message(
        &session,
        "2026-07-29T12:00:02.000Z",
        "assistant",
        append_query,
    );
    let appended = json_output(ctx(&temp).args([
        "search",
        append_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &appended, "codex", append_query, 1, "message");
    let append_generation = assert_published_generation(&appended, "wait");
    assert_ne!(append_generation, cold_generation);
    assert_eq!(
        appended["retrieval"]["indexed_documents"], 3,
        "{appended:#}"
    );
    let append_status =
        assert_daemon_publication(&temp, &append_generation, 1, &["codex", "codex"]);
    assert_eq!(
        append_status["history_epoch"]["lexical_generation_id"], cold_generation,
        "{append_status:#}"
    );
    let (append_manifest, _) = generation_manifest(&temp, &append_generation);
    assert_eq!(append_manifest.sources.len(), 2);
    let append_source = append_manifest
        .sources
        .iter()
        .find(|source| source.counts().indexed_documents == 2)
        .expect("appended lifecycle source");
    let cold_source = cold_manifest
        .sources
        .iter()
        .find(|source| source.observation().source() == append_source.observation().source())
        .expect("cold lifecycle source");
    assert_ne!(append_source.content_digest(), cold_source.content_digest());
    assert!(
        append_source.counts().certified_bytes > cold_source.counts().certified_bytes,
        "{append_manifest:#?}"
    );
    assert_eq!(append_source.counts().indexed_documents, 2);

    let rewrite_query = "rewrite-source-lifecycle-oracle";
    let rewrite_padding = format!("rewrite-companion-{}", "x".repeat(2_048));
    write_codex_session(
        &session,
        native_session_id,
        &[
            ("2026-07-29T12:00:03.000Z", "user", rewrite_query),
            ("2026-07-29T12:00:04.000Z", "assistant", &rewrite_padding),
        ],
    );
    let rewrite_length = fs::metadata(&session).unwrap().len();
    let rewritten = json_output(ctx(&temp).args([
        "search",
        rewrite_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(
        &temp,
        &rewritten,
        "codex",
        rewrite_query,
        1,
        "message",
    );
    let rewrite_generation = assert_published_generation(&rewritten, "wait");
    assert_ne!(rewrite_generation, append_generation);
    assert_eq!(
        rewritten["retrieval"]["indexed_documents"], 3,
        "{rewritten:#}"
    );
    let rewrite_status =
        assert_daemon_publication(&temp, &rewrite_generation, 1, &["codex", "codex"]);
    assert_eq!(
        rewrite_status["history_epoch"]["lexical_generation_id"], cold_generation,
        "{rewrite_status:#}"
    );
    let (rewrite_manifest, _) = generation_manifest(&temp, &rewrite_generation);
    let rewrite_source = rewrite_manifest
        .sources
        .iter()
        .find(|source| source.observation().source() == append_source.observation().source())
        .expect("rewritten lifecycle source");
    assert_eq!(
        rewrite_source.observation().source(),
        append_source.observation().source()
    );
    assert_ne!(
        rewrite_source.content_digest(),
        append_source.content_digest()
    );
    let replaced = json_output(ctx(&temp).args([
        "search",
        append_query,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        replaced["results"].as_array().unwrap().is_empty(),
        "{replaced:#}"
    );
    assert_eq!(generation_id(&replaced), rewrite_generation);

    let truncate_query = "truncate-source-lifecycle-oracle";
    write_codex_session(
        &session,
        native_session_id,
        &[("2026-07-29T12:00:05.000Z", "user", truncate_query)],
    );
    assert!(
        fs::metadata(&session).unwrap().len() < rewrite_length,
        "truncate lifecycle mutation must reduce the certified source length"
    );
    let truncated = json_output(ctx(&temp).args([
        "search",
        truncate_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(
        &temp,
        &truncated,
        "codex",
        truncate_query,
        1,
        "message",
    );
    let truncate_generation = assert_published_generation(&truncated, "wait");
    assert_ne!(truncate_generation, rewrite_generation);
    assert_eq!(
        truncated["retrieval"]["indexed_documents"], 2,
        "{truncated:#}"
    );
    let truncate_status =
        assert_daemon_publication(&temp, &truncate_generation, 1, &["codex", "codex"]);
    assert_eq!(
        truncate_status["history_epoch"]["lexical_generation_id"], cold_generation,
        "{truncate_status:#}"
    );
    let (truncate_manifest, _) = generation_manifest(&temp, &truncate_generation);
    let truncate_source = truncate_manifest
        .sources
        .iter()
        .find(|source| source.observation().source() == rewrite_source.observation().source())
        .expect("truncated lifecycle source");
    assert_eq!(
        truncate_source.observation().source(),
        rewrite_source.observation().source()
    );
    assert_eq!(truncate_source.counts().indexed_documents, 1);
    let unavailable_event_id = truncated["results"][0]["ctx_event_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let unavailable_sessions = temp.path().join(".codex/sessions-unavailable");
    fs::rename(&sessions, &unavailable_sessions).unwrap();
    let unavailable = failure_stderr(ctx(&temp).args([
        "search",
        truncate_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert!(
        unavailable.contains("all_provider_terminal_coverage_unavailable"),
        "{unavailable}"
    );
    let unavailable_status = assert_daemon_refresh_failure(&temp, 0, Some(&truncate_generation));
    assert_eq!(
        unavailable_status["daemon"]["jobs"]["source_backed_refresh"]["error_code"],
        "all_provider_terminal_coverage_unavailable",
        "{unavailable_status:#}"
    );
    assert_eq!(
        unavailable_status["lexical"]["generation_id"], truncate_generation,
        "{unavailable_status:#}"
    );
    assert_eq!(
        unavailable_status["lexical"]["status"], "unavailable",
        "{unavailable_status:#}"
    );
    assert_eq!(
        unavailable_status["lexical"]["reason"], "source_refresh_failed",
        "{unavailable_status:#}"
    );
    let (retained_manifest, _) = generation_manifest(&temp, &truncate_generation);
    assert_eq!(retained_manifest.sources, truncate_manifest.sources);
    assert!(retained_manifest.removals.is_empty());
    let unavailable_search = failure_stderr(ctx(&temp).args([
        "search",
        truncate_query,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        unavailable_search.contains("generation-bound source"),
        "{unavailable_search}"
    );
    ctx(&temp)
        .args(["show", "event", &unavailable_event_id, "--format", "json"])
        .assert()
        .failure();

    fs::rename(&unavailable_sessions, &sessions).unwrap();
    let restored = json_output(ctx(&temp).args([
        "search",
        truncate_query,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(
        &temp,
        &restored,
        "codex",
        truncate_query,
        1,
        "message",
    );
    assert_eq!(generation_id(&restored), truncate_generation);

    fs::remove_file(&session).unwrap();
    let deleted = json_output(ctx(&temp).args([
        "search",
        truncate_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    let deletion_generation = assert_published_generation(&deleted, "wait");
    assert_ne!(deletion_generation, truncate_generation);
    assert_eq!(deleted["retrieval"]["indexed_documents"], 1, "{deleted:#}");
    assert!(
        deleted["results"].as_array().unwrap().is_empty(),
        "{deleted:#}"
    );
    let deletion_status = assert_daemon_publication(&temp, &deletion_generation, 1, &["codex"]);
    assert_eq!(
        deletion_status["history_epoch"]["lexical_generation_id"], cold_generation,
        "{deletion_status:#}"
    );
    let (deletion_manifest, _) = generation_manifest(&temp, &deletion_generation);
    assert_eq!(deletion_manifest.sources.len(), 1);
    assert_eq!(deletion_manifest.removals.len(), 1);
    assert_eq!(
        deletion_manifest.removals[0].source(),
        truncate_source.observation().source()
    );
    let deleted_show = failure_stderr(ctx(&temp).args([
        "show",
        "event",
        &unavailable_event_id,
        "--format",
        "json",
    ]));
    assert!(
        deleted_show.contains("was not found in the source-backed Core generation"),
        "{deleted_show}"
    );
}

#[test]
fn two_provider_mutation_fails_closed_when_retained_provider_is_temporarily_omitted() {
    let temp = tempdir();
    let codex_history = temp.path().join(".codex/history.jsonl");
    fs::create_dir_all(codex_history.parent().unwrap()).unwrap();
    let initial_codex_body = concat!(
        r#"{"session_id":"terminal-coverage-codex","ts":1785326400,"text":"terminal coverage codex retained"}"#,
        "\n"
    );
    fs::write(&codex_history, initial_codex_body).unwrap();
    let claude_query = "terminal coverage claude retained";
    install_default_claude_fixture(&temp, claude_query);
    let _daemon = start_source_refresh_daemon(&temp);

    let initial = json_output(ctx(&temp).args([
        "search",
        "terminal coverage codex retained",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    let retained_generation = assert_published_generation(&initial, "wait");
    let (retained_manifest, _) = generation_manifest(&temp, &retained_generation);
    assert_eq!(retained_manifest.sources.len(), 2, "{retained_manifest:#?}");
    let published_manifests = generation_manifest_paths(&temp);

    let claude_root = temp.path().join(".claude/projects");
    let unavailable_claude_root = temp.path().join(".claude/projects-unavailable");
    fs::rename(&claude_root, &unavailable_claude_root).unwrap();
    let retained_codex_history = temp.path().join("retained-codex-history.jsonl");
    fs::rename(&codex_history, &retained_codex_history).unwrap();
    fs::copy(&retained_codex_history, &codex_history).unwrap();
    let mut history = fs::OpenOptions::new()
        .append(true)
        .open(&codex_history)
        .unwrap();
    writeln!(
        history,
        r#"{{"session_id":"terminal-coverage-codex","ts":1785326401,"text":"terminal coverage codex mutation"}}"#
    )
    .unwrap();
    drop(history);

    let failure = failure_stderr(ctx(&temp).args([
        "search",
        "terminal coverage codex mutation",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert!(
        failure.contains("all_provider_terminal_coverage_unavailable"),
        "{failure}"
    );
    let failed = assert_daemon_refresh_failure(&temp, 0, Some(&retained_generation));
    let job = &failed["daemon"]["jobs"]["source_backed_refresh"];
    assert_eq!(
        job["error_code"], "all_provider_terminal_coverage_unavailable",
        "{failed:#}"
    );
    assert_eq!(
        job["reason"], "provider_terminal_coverage_unavailable",
        "{failed:#}"
    );
    assert_eq!(job["retryable"], true, "{failed:#}");
    assert!(job["retry_not_before_at_ms"].is_number(), "{failed:#}");
    assert_eq!(
        generation_manifest_paths(&temp),
        published_manifests,
        "a partial provider refresh must not publish a mixed generation"
    );
    let (still_retained, _) = generation_manifest(&temp, &retained_generation);
    assert_eq!(still_retained.sources, retained_manifest.sources);

    let codex = failure_stderr(ctx(&temp).args([
        "search",
        "terminal coverage codex retained",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        codex.contains("source_hydration_failed/")
            && !codex.contains("no registered provider route owns")
            && !codex.contains("resolver_generation_unavailable"),
        "{codex}"
    );
    let claude = failure_stderr(ctx(&temp).args([
        "search",
        claude_query,
        "--provider",
        "claude",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        claude.contains("source_hydration_failed/")
            && !claude.contains("no registered provider route owns")
            && !claude.contains("resolver_generation_unavailable"),
        "{claude}"
    );
}

#[test]
fn search_refresh_publishes_discovered_top_provider_sources() {
    for (cli_provider, stored_provider, install_fixture) in [
        (
            "claude",
            "claude",
            install_default_claude_fixture as fn(&TempDir, &str),
        ),
        ("pi", "pi", install_default_pi_fixture),
        ("hermes", "hermes", install_default_hermes_fixture),
        ("kilo", "kilo", install_default_kilo_fixture),
        ("astrbot", "astrbot", install_default_astrbot_fixture),
        ("continue", "continue", install_default_continue_fixture),
        ("openhands", "openhands", install_default_openhands_fixture),
        ("rovodev", "rovodev", install_default_rovodev_fixture),
        ("lingma", "lingma", install_default_lingma_fixture),
        ("qoder", "qoder", install_default_qoder_fixture),
        ("junie", "junie", install_default_junie_fixture),
        ("cursor", "cursor", install_default_cursor_fixture),
    ] {
        let temp = tempdir();
        let query = format!("{stored_provider}-default-refresh-oracle");
        install_fixture(&temp, &query);
        let _daemon = start_source_refresh_daemon(&temp);

        let search = json_output(ctx(&temp).args([
            "search",
            &query,
            "--provider",
            cli_provider,
            "--refresh",
            "wait",
            "--format=json",
        ]));
        assert_source_backed_search_show_oracle(
            &temp,
            &search,
            stored_provider,
            &query,
            1,
            "message",
        );
        assert_eq!(search["freshness"]["source_count"], 1);
        let generation = assert_published_generation(&search, "wait");
        let status = assert_daemon_publication(&temp, &generation, 1, &[stored_provider]);
        assert_eq!(
            status["lexical"]["certified_sources"], 1,
            "{cli_provider} did not publish source inventory: {status:#}"
        );

        let unchanged = json_output(ctx(&temp).args([
            "search",
            &query,
            "--provider",
            cli_provider,
            "--refresh",
            "wait",
            "--format=json",
        ]));
        assert_source_backed_search_show_oracle(
            &temp,
            &unchanged,
            stored_provider,
            &query,
            1,
            "message",
        );
        assert_eq!(generation_id(&unchanged), generation, "{unchanged:#}");
        let unchanged_status = assert_daemon_publication(&temp, &generation, 1, &[stored_provider]);
        assert_eq!(
            unchanged_status["daemon"]["jobs"]["source_backed_refresh"]["generation_changed"],
            false,
            "{cli_provider} republished an unchanged source: {unchanged_status:#}"
        );
    }
}

#[test]
fn search_refresh_hermes_generation_detects_wal_only_append() {
    let temp = tempdir();
    let initial = "hermes-root-inventory-initial-oracle";
    let appended = "hermes-root-inventory-appended-oracle";
    install_default_hermes_fixture(&temp, initial);
    let source = temp.path().join(".hermes/state.db");
    let writer = Connection::open(&source).unwrap();
    let journal_mode: String = writer
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
    writer
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .unwrap();
    let _daemon = start_source_refresh_daemon(&temp);

    let first = json_output(ctx(&temp).args([
        "search",
        initial,
        "--provider",
        "hermes",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &first, "hermes", initial, 1, "message");
    let first_generation = assert_published_generation(&first, "wait");
    let first_documents = first["retrieval"]["indexed_documents"].as_u64().unwrap();

    let unchanged = json_output(ctx(&temp).args([
        "search",
        initial,
        "--provider",
        "hermes",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_eq!(generation_id(&unchanged), first_generation, "{unchanged:#}");
    let unchanged_status = assert_daemon_publication(&temp, &first_generation, 1, &["hermes"]);
    assert_eq!(
        unchanged_status["daemon"]["jobs"]["source_backed_refresh"]["generation_changed"], false,
        "{unchanged_status:#}"
    );

    let main_before = fs::metadata(&source).unwrap();
    writer
        .execute(
            "INSERT INTO messages (session_id, role, content, timestamp)
             VALUES (?1, 'user', ?2, 1782259203.0)",
            ["hermes-cli-native", appended],
        )
        .unwrap();
    assert!(source.with_extension("db-wal").is_file());
    let main_after = fs::metadata(&source).unwrap();
    assert_eq!(main_after.len(), main_before.len());
    assert_eq!(
        main_after.modified().unwrap(),
        main_before.modified().unwrap()
    );

    let refreshed = json_output(ctx(&temp).args([
        "search",
        appended,
        "--provider",
        "hermes",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &refreshed, "hermes", appended, 1, "message");
    let refreshed_generation = assert_published_generation(&refreshed, "wait");
    assert_ne!(refreshed_generation, first_generation);
    assert!(
        refreshed["retrieval"]["indexed_documents"]
            .as_u64()
            .unwrap()
            > first_documents,
        "{refreshed:#}"
    );
    let refreshed_status = assert_daemon_publication(&temp, &refreshed_generation, 1, &["hermes"]);
    assert_eq!(
        refreshed_status["daemon"]["jobs"]["source_backed_refresh"]["generation_changed"], true,
        "{refreshed_status:#}"
    );
    drop(writer);
}

#[test]
fn search_refresh_wait_json_keeps_stderr_clean_and_reports_daemon_progress() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    copy_dir_all(&fixture, &temp.path().join(".codex").join("sessions"));
    let _daemon = start_source_refresh_daemon(&temp);

    let output = ctx(&temp)
        .args([
            "search",
            "onboarding",
            "--provider",
            "codex",
            "--refresh",
            "wait",
            "--format=json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_source_backed_search_show_oracle(&temp, &stdout, "codex", "onboarding", 1, "message");
    let generation = assert_published_generation(&stdout, "wait");
    let status = assert_daemon_publication(&temp, &generation, 1, &["codex", "codex"]);
    let job = &status["daemon"]["jobs"]["source_backed_refresh"];
    assert_eq!(job["progress"]["phase"], "published", "{status:#}");
    assert_eq!(job["progress"]["completed_sources"], 1, "{status:#}");
    assert_eq!(job["progress"]["total_sources"], 1, "{status:#}");
}

#[test]
fn search_refresh_wait_reports_typed_failure_for_empty_source_inventory() {
    let temp = tempdir();
    let _daemon = start_source_refresh_daemon(&temp);
    let output = ctx(&temp)
        .args(["search", "anything", "--refresh", "wait", "--format=json"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("daemon-owned source-backed refresh failed"),
        "{stderr}"
    );
    assert!(
        stderr.contains("no executable source-backed routes were registered"),
        "{stderr}"
    );

    let status = assert_daemon_refresh_failure(&temp, 0, None);
    assert_eq!(
        status["history_epoch"]["status"], "unavailable",
        "{status:#}"
    );
    assert_eq!(
        status["history_epoch"]["reason"], "source_refresh_failed",
        "{status:#}"
    );
    assert_eq!(status["lexical"]["status"], "unavailable", "{status:#}");
    assert_eq!(status["refresh"]["source_count"], 0, "{status:#}");
    assert_eq!(
        status["refresh"]["progress"]["phase"], "failed",
        "{status:#}"
    );
    assert!(status.get("prior_epoch").is_none(), "{status:#}");
    assert!(!search_refresh_data_root(&temp)
        .join("search/lexical/meta.json")
        .exists());
    assert!(generation_manifest_paths(&temp).is_empty());
}
