#[test]
fn fresh_home_search_mvp_flow() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let obsolete_relational = data_root(&temp).join("relational.sqlite");
    let obsolete_relational_bytes = b"obsolete relational projection must remain inert";
    fs::create_dir_all(data_root(&temp)).unwrap();
    fs::write(&obsolete_relational, obsolete_relational_bytes).unwrap();

    let setup_stdout = ctx(&temp)
        .arg("setup")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let setup_stdout = String::from_utf8(setup_stdout).unwrap();
    assert!(
        setup_stdout.contains("History is ready to search")
            || setup_stdout.contains("History indexing is queued"),
        "{setup_stdout}"
    );
    assert!(
        !data_root(&temp).join("config.toml").exists(),
        "setup should not write config.toml for implicit defaults"
    );

    let setup_json = json_output(ctx(&temp).args(["setup", "--format=json"]));
    assert_eq!(setup_json["schema_version"], 2);
    assert_eq!(setup_json["network_required"], false);
    assert_eq!(setup_json["repo_writes"], false);
    assert!(
        matches!(setup_json["mode"].as_str(), Some("pending" | "ready")),
        "{setup_json:#}"
    );
    assert_eq!(setup_json["daemon_autostart"]["status"], "degraded");
    assert_eq!(setup_json["daemon_autostart"]["requested"], true);
    assert!(setup_json.get("background_indexing").is_none());
    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
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
        "--format=json",
    ]));
    assert_explicit_source_publication(&import, "codex", "codex_session_jsonl_tree");
    assert!(import["totals"]["current_source_count"].as_u64().unwrap() > 0);
    assert!(
        import["totals"]["current_certified_source_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(import["sources"][0]["source_files"].as_u64().unwrap() > 0);
    assert!(import["sources"][0]["source_bytes"].as_u64().unwrap() > 0);

    let search = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--format=json",
    ]));
    assert_eq!(search["schema_version"], 1);
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
    assert_eq!(first_result["result_type"], "session_result");
    assert_eq!(first_result["result_scope"], "session");
    let ctx_event_id = first_result["ctx_event_id"].as_str().unwrap().to_owned();
    let ctx_session_id = first_result["ctx_session_id"].as_str().unwrap().to_owned();
    let provider_session_id = first_result["provider_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(first_result.get("source_path").is_none());
    assert_session_suggested_next_commands(first_result);
    assert!(first_result["citations"][0]["ctx_event_id"].is_string());
    assert!(first_result["citations"][0]["ctx_session_id"].is_string());

    let shown_by_provider_id = json_output(ctx(&temp).args([
        "show",
        "session",
        "--provider",
        "codex",
        "--provider-session",
        &provider_session_id,
        "--format=json",
    ]));
    assert_eq!(shown_by_provider_id["ctx_session_id"], ctx_session_id);
    assert_eq!(
        shown_by_provider_id["provider_session_id"],
        provider_session_id
    );

    let located_by_provider_id = json_output(ctx(&temp).args([
        "locate",
        "session",
        "--provider",
        "codex",
        "--provider-session",
        &provider_session_id,
        "--format=json",
    ]));
    assert_eq!(located_by_provider_id["ctx_session_id"], ctx_session_id);
    assert_eq!(
        located_by_provider_id["provider_session_id"],
        provider_session_id
    );
    assert_eq!(
        located_by_provider_id["source"]["source_format"],
        "codex_session_jsonl"
    );
    let located_session_human = ctx(&temp)
        .args(["locate", "session", &ctx_session_id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let located_session_human = String::from_utf8(located_session_human).unwrap();
    assert!(
        located_session_human.contains(&format!(
            "First event       {}",
            located_by_provider_id["started_at"].as_str().unwrap()
        )),
        "{located_session_human}"
    );
    assert!(!located_session_human.contains("Started"));

    let located_event =
        json_output(ctx(&temp).args(["locate", "event", &ctx_event_id, "--format=json"]));
    assert_eq!(located_event["ctx_event_id"], ctx_event_id);
    assert_eq!(located_event["ctx_session_id"], ctx_session_id);
    let located_event_human = ctx(&temp)
        .args(["locate", "event", &ctx_event_id])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let located_event_human = String::from_utf8(located_event_human).unwrap();
    assert!(
        located_event_human.contains(&format!("Session           {ctx_session_id}")),
        "{located_event_human}"
    );
    assert!(
        located_event_human.contains(&format!(
            "Time              {}",
            located_event["occurred_at"].as_str().unwrap()
        )),
        "{located_event_human}"
    );
    assert!(
        located_event_human.contains(&format!(
            "Sequence          {}",
            located_event["sequence"].as_u64().unwrap()
        )),
        "{located_event_human}"
    );
    assert!(!located_event_human.contains("Role"));
    assert!(!located_event_human.contains("Type"));

    let term_search = json_output(ctx(&temp).args([
        "search",
        "zzzz-no-match",
        "--term",
        "onboarding",
        "--provider",
        "codex",
        "--format=json",
    ]));
    assert_eq!(term_search["query"], "zzzz-no-match OR onboarding");
    assert!(!term_search["results"].as_array().unwrap().is_empty());
    for result in term_search["results"].as_array().unwrap() {
        assert_session_suggested_next_commands(result);
    }

    let event_search = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--events",
        "--format=json",
    ]));
    assert_event_search_provider_oracle(&event_search, "codex", "onboarding", 2, "message");

    let session_events = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--session",
        &ctx_session_id,
        "--format=json",
    ]));
    assert_event_search_provider_oracle(&session_events, "codex", "onboarding", 2, "message");
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
        "--format=json",
    ]));
    assert_event_search_provider_oracle(
        &prefixed_session_events,
        "codex",
        "onboarding",
        2,
        "message",
    );
    assert_eq!(
        prefixed_session_events["filters"]["session"],
        session_prefix
    );

    let human_search = ctx(&temp)
        .args(["search", "onboarding"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human_search = String::from_utf8(human_search).unwrap();
    assert!(
        human_search.starts_with("1 result · relevance order · primary sessions\n\n"),
        "{human_search}"
    );
    assert!(human_search.contains("1. "));
    assert!(human_search.contains("Session  codex · "), "{human_search}");
    assert!(
        human_search.contains("Event    44ce421b · 2026-06-23T15:00:02.000Z"),
        "{human_search}"
    );
    assert!(
        human_search.contains("Inspect\n") && human_search.contains(" show session "),
        "{human_search}"
    );
    assert!(!human_search.contains("ctx_event_id"));
    assert!(!human_search.contains("provider_session_id"));
    assert!(!human_search.contains("Next"));
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
    assert!(verbose_search.contains("Event"));
    assert!(verbose_search.contains("Ctx session"));
    assert!(verbose_search.contains("Provider session"));
    assert!(verbose_search.contains("Rank"));
    assert!(verbose_search.contains("Retrieval score"));
    assert!(verbose_search.contains(" show session "));
    assert!(verbose_search.contains(" show event "));
    assert!(verbose_search.contains(" search onboarding --session"));
    assert!(!human_search.contains("work_record"));
    assert!(!human_search.contains("history_record"));

    let file_search = json_output(ctx(&temp).args([
        "search",
        "--file",
        "crates/foo/src/lib.rs",
        "--format=json",
    ]));
    assert_eq!(file_search["query"], "");
    assert!(file_search["results"].is_array());

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

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["schema_version"], 2);
    assert!(status["indexed_items"].as_u64().unwrap() > 0);
    assert_eq!(status["semantic"]["status"], "disabled");
    assert_eq!(status["semantic"]["reason"], "semantic_disabled");
    assert_eq!(status["daemon"]["enabled"], true);
    assert!(status["daemon"]["jobs"]["core_refresh"]["status"].is_string());

    let doctor = json_output(ctx(&temp).args(["doctor", "--format=json"]));
    assert_eq!(doctor["schema_version"], 1);
    let findings = doctor["findings"].as_array().unwrap();
    assert_eq!(doctor["ok"], findings.is_empty());
    assert_eq!(doctor["daemon"]["enabled"], true);
    assert_eq!(doctor["source_epoch"]["lexical"]["status"], "ready");
    assert_eq!(doctor["pro"]["error_code"], "pro_not_installed");
    assert_eq!(
        fs::read(&obsolete_relational).unwrap(),
        obsolete_relational_bytes,
        "current commands must neither open for mutation nor clean up obsolete relational state"
    );
}

#[test]
fn foreground_core_observations_are_truthful_and_recorded_once_after_output() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));

    let (search, search_output_bytes) =
        measured_json_output(ctx(&temp).env_remove("CTX_LOCAL_USAGE_ENABLED").args([
            "search",
            "onboarding",
            "--provider",
            "codex",
            "--refresh",
            "off",
            "--format=json",
        ]));
    let search_results = search["results"].as_array().unwrap();
    let search_result_count = search_results.len();
    let delivered_context_bytes = search_results
        .iter()
        .map(|result| result["snippet"].as_str().unwrap().len())
        .sum::<usize>();
    let ctx_event_id = search_results[0]["ctx_event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ctx_session_id = search_results[0]["ctx_session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (empty_search, empty_search_output_bytes) =
        measured_json_output(ctx(&temp).env_remove("CTX_LOCAL_USAGE_ENABLED").args([
            "search",
            "no-result-marker-7f8a",
            "--provider",
            "codex",
            "--refresh",
            "off",
            "--format=json",
        ]));
    assert!(empty_search["results"].as_array().unwrap().is_empty());

    let (shown_session, show_session_output_bytes) =
        measured_json_output(ctx(&temp).env_remove("CTX_LOCAL_USAGE_ENABLED").args([
            "show",
            "session",
            &ctx_session_id,
            "--format=json",
        ]));
    assert!(!shown_session["events"].as_array().unwrap().is_empty());

    let (shown_event, show_event_output_bytes) =
        measured_json_output(ctx(&temp).env_remove("CTX_LOCAL_USAGE_ENABLED").args([
            "show",
            "event",
            &ctx_event_id,
            "--format=json",
        ]));
    assert!(!shown_event["events"].as_array().unwrap().is_empty());

    let (sources, sources_output_bytes) = measured_json_output(
        ctx(&temp)
            .env_remove("CTX_LOCAL_USAGE_ENABLED")
            .args(["sources", "--format=json"]),
    );
    assert!(!sources["sources"].as_array().unwrap().is_empty());

    let blocked_parent = temp.path().join("blocked-show-output");
    fs::write(&blocked_parent, "not a directory").unwrap();
    let failed_show = ctx(&temp)
        .env_remove("CTX_LOCAL_USAGE_ENABLED")
        .arg("show")
        .arg("session")
        .arg(&ctx_session_id)
        .arg("--format=json")
        .arg("--out")
        .arg(blocked_parent.join("session.json"))
        .assert()
        .failure()
        .get_output()
        .clone();
    let failed_show_output_bytes = failed_show
        .stdout
        .len()
        .saturating_add(failed_show.stderr.len());
    assert!(failed_show_output_bytes > 0);

    let connection = Connection::open(data_root(&temp).join("usage.sqlite")).unwrap();
    let totals = |operation: &str, outcome: &str, value_class: &str| {
        connection
            .query_row(
                r#"
                SELECT
                    SUM(calls),
                    SUM(result_count),
                    SUM(citation_count),
                    SUM(delivered_output_bytes),
                    SUM(delivered_context_bytes),
                    SUM(matched_normalized_session_bytes)
                FROM daily_usage
                WHERE surface = 'cli'
                  AND operation = ?1
                  AND outcome = ?2
                  AND value_class = ?3
                "#,
                params![operation, outcome, value_class],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .unwrap()
    };

    let search_complete: (String, i64) = connection
        .query_row(
            "SELECT context_coverage, matched_normalized_session_bytes \
             FROM daily_usage \
             WHERE surface = 'cli' AND operation = 'search' \
               AND value_class = 'result_bearing'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(search_complete.0, "complete");
    assert!(search_complete.1 >= delivered_context_bytes as i64);
    assert_eq!(
        totals("search", "success", "result_bearing"),
        (
            1,
            search_result_count as i64,
            0,
            search_output_bytes as i64,
            delivered_context_bytes as i64,
            search_complete.1,
        )
    );
    assert_eq!(
        totals("search", "success", "empty"),
        (1, 0, 0, empty_search_output_bytes as i64, 0, 0)
    );
    assert_eq!(
        totals("show_session", "success", "not_applicable"),
        (1, 0, 0, show_session_output_bytes as i64, 0, 0)
    );
    assert_eq!(
        totals("show_event", "success", "not_applicable"),
        (1, 0, 0, show_event_output_bytes as i64, 0, 0)
    );
    assert_eq!(
        totals("sources", "success", "not_applicable"),
        (1, 0, 0, sources_output_bytes as i64, 0, 0)
    );
    assert_eq!(
        totals("show_session", "failure", "not_applicable"),
        (1, 0, 0, failed_show_output_bytes as i64, 0, 0)
    );
}
