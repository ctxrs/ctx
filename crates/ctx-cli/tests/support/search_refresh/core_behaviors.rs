#[test]
fn search_refresh_exact_noop_and_repeated_tiny_appends_stay_bounded() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let sessions = temp.path().join(".codex/sessions");
    copy_dir_all(&fixture, &sessions);
    let appended_source = sessions.join("2026/06/23/root.jsonl");
    let _daemon = start_source_refresh_daemon(&temp);

    let initial = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &initial, "codex", "onboarding", 1, "message");
    let initial_generation = assert_published_generation(&initial, "wait");
    let initial_documents = initial["retrieval"]["indexed_documents"].as_u64().unwrap();
    let initial_status =
        assert_daemon_publication(&temp, &initial_generation, 1, &["codex", "codex"]);
    let initial_job = &initial_status["daemon"]["jobs"]["core_refresh"];
    let initial_current = initial_job["receipt"]["current"].clone();

    let index_root = search_refresh_data_root(&temp).join("search/lexical");
    let meta_path = active_generation_meta_path(&index_root, &initial_generation);
    let manifest_path = index_root
        .join("ctx-generations")
        .join(format!("{initial_generation}.json"));
    let initial_meta = published_file_state(&meta_path);
    let initial_manifest = published_file_state(&manifest_path);
    let initial_manifests = generation_manifest_paths(&temp);
    let (initial_opstamp, initial_segments) = tantivy_meta_facts(&initial_meta);
    let initial_index_bytes = directory_bytes(&index_root);
    assert!(!initial_segments.is_empty());

    let unchanged = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert!(unchanged["results"][0].get("source_exists").is_none());
    assert!(unchanged["results"][0]["citations"][0]
        .get("source_exists")
        .is_none());
    assert_eq!(generation_id(&unchanged), initial_generation);
    assert_eq!(
        unchanged["retrieval"]["indexed_documents"], initial_documents,
        "{unchanged:#}"
    );
    let unchanged_status =
        assert_daemon_publication(&temp, &initial_generation, 1, &["codex", "codex"]);
    let unchanged_job = &unchanged_status["daemon"]["jobs"]["core_refresh"];
    assert_eq!(
        unchanged_job["generation_changed"], false,
        "{unchanged_job:#}"
    );
    assert_eq!(
        unchanged_job["receipt"]["current"], initial_current,
        "{unchanged_job:#}"
    );
    assert_published_file_unchanged(&meta_path, &initial_meta);
    assert_published_file_unchanged(&manifest_path, &initial_manifest);
    assert_eq!(generation_manifest_paths(&temp), initial_manifests);
    assert_eq!(
        tantivy_meta_facts(&published_file_state(&meta_path)),
        (initial_opstamp, initial_segments.clone())
    );

    let append_runs = LEXICAL_SEGMENT_MERGE_FAN_IN + 2;
    let mut previous_generation = initial_generation;
    let mut previous_opstamp = initial_opstamp;
    let mut previous_segments = initial_segments.clone();
    let mut expected_documents = initial_documents;
    let mut expected_complete_records = initial_current["current_complete_records"]
        .as_u64()
        .unwrap();
    let mut expected_retained_records = initial_current["current_retained_records"]
        .as_u64()
        .unwrap();
    let mut expected_source_bytes = initial_current["current_certified_source_bytes"]
        .as_u64()
        .unwrap();
    let mut saw_coalescing = false;

    for append_ordinal in 0..append_runs {
        let source_bytes_before = fs::metadata(&appended_source).unwrap().len();
        let append_query = format!("canonical tiny append refresh oracle {append_ordinal}");
        append_codex_message(
            &appended_source,
            &format!("2026-06-23T15:00:{:02}.000Z", append_ordinal + 8),
            "assistant",
            &append_query,
        );
        let appended_bytes = fs::metadata(&appended_source).unwrap().len() - source_bytes_before;
        assert!(appended_bytes > 0);

        let appended = json_output(ctx(&temp).args([
            "search",
            &append_query,
            "--provider",
            "codex",
            "--refresh",
            "wait",
            "--format=json",
        ]));
        assert_source_backed_search_show_oracle(
            &temp,
            &appended,
            "codex",
            &append_query,
            1,
            "message",
        );
        let append_generation = assert_published_generation(&appended, "wait");
        assert_ne!(append_generation, previous_generation);
        expected_documents += 1;
        expected_complete_records += 1;
        expected_retained_records += 1;
        expected_source_bytes += appended_bytes;
        assert_eq!(
            appended["retrieval"]["indexed_documents"], expected_documents,
            "{appended:#}"
        );

        let append_status =
            assert_daemon_publication(&temp, &append_generation, 1, &["codex", "codex"]);
        let append_job = &append_status["daemon"]["jobs"]["core_refresh"];
        let append_current = &append_job["receipt"]["current"];
        assert_eq!(
            append_current["current_indexed_documents"], expected_documents,
            "{append_job:#}"
        );
        assert_eq!(
            append_current["current_complete_records"], expected_complete_records,
            "{append_job:#}"
        );
        assert_eq!(
            append_current["current_retained_records"], expected_retained_records,
            "{append_job:#}"
        );
        assert_eq!(
            append_current["current_certified_source_bytes"], expected_source_bytes,
            "{append_job:#}"
        );

        let append_meta_path = active_generation_meta_path(&index_root, &append_generation);
        let append_meta = published_file_state(&append_meta_path);
        let (append_opstamp, append_segments) = tantivy_meta_facts(&append_meta);
        assert!(append_opstamp > previous_opstamp);
        // Route checkpoints and Tantivy indexing workers may expose more than
        // one scheduler-produced segment in a publication. The product
        // contract is the native merge policy's amortized fan-in bound and an
        // observable coalescing publication after the run crosses that bound,
        // not a particular synchronous segment shape for each append.
        assert!(
            append_segments.len() < initial_segments.len() + LEXICAL_SEGMENT_MERGE_FAN_IN,
            "same-tier active segments exceeded the configured fan-in bound: \
             initial={initial_segments:?}, current={append_segments:?}"
        );
        saw_coalescing |= append_segments.len() <= previous_segments.len();
        previous_generation = append_generation;
        previous_opstamp = append_opstamp;
        previous_segments = append_segments;
    }

    assert!(
        saw_coalescing,
        "the product refresh path crossed merge fan-in {LEXICAL_SEGMENT_MERGE_FAN_IN} \
         without coalescing"
    );
    let appended_index_bytes = directory_bytes(&index_root);
    assert!(
        appended_index_bytes <= initial_index_bytes.saturating_mul(4),
        "{append_runs} tiny appends exceeded the amortized retained-byte budget: \
         before={initial_index_bytes}, after={appended_index_bytes}"
    );
    let retained_manifests = generation_manifest_paths(&temp).len();
    assert!(
        (1..=2).contains(&retained_manifests),
        "inactive generation manifests accumulated after {append_runs} appends: {retained_manifests}"
    );
}

#[test]
fn search_refreshes_discovered_codex_sessions_before_query() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);
    let _daemon = start_source_refresh_daemon(&temp);

    let search = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &search, "codex", "onboarding", 1, "message");
    assert_eq!(search["freshness"]["source_count"], 1);
    let generation = assert_published_generation(&search, "wait");
    let status = assert_daemon_publication(&temp, &generation, 1, &["codex", "codex"]);
    assert!(
        status["lexical"]["indexed_documents"].as_u64().unwrap() >= 2,
        "{status:#}"
    );
}

#[test]
fn search_refreshes_discovered_codex_prompt_history_before_query() {
    let temp = tempdir();
    let history = temp.path().join(".codex/history.jsonl");
    fs::create_dir_all(history.parent().unwrap()).unwrap();
    fs::write(
        &history,
        concat!(
            r#"{"session_id":"prompt-refresh-session","ts":1784371200,"text":"prompt history search refresh oracle"}"#,
            "\n"
        ),
    )
    .unwrap();
    let _daemon = start_source_refresh_daemon(&temp);

    let search = json_output(ctx(&temp).args([
        "search",
        "prompt history search refresh oracle",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(
        &temp,
        &search,
        "codex",
        "prompt history search refresh oracle",
        1,
        "message",
    );
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["retrieval"]["indexed_documents"], 1);
    let generation = assert_published_generation(&search, "wait");
    assert_daemon_publication(&temp, &generation, 1, &["codex"]);
}

#[test]
fn machine_readable_search_attempts_enabled_daemon_self_healing() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    copy_dir_all(&fixture, &temp.path().join(".codex").join("sessions"));
    let missing_exe = temp.path().join("missing-ctx-binary");

    let output = ctx(&temp)
        .args([
            "search",
            "onboarding",
            "--provider",
            "codex",
            "--format=json",
        ])
        .env("CTX_DAEMON_AUTOSTART_EXE", &missing_exe)
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("ctx daemon did not start")
            && stderr.contains("spawn ctx daemon")
            && stderr.contains("No such file"),
        "{stderr}"
    );
    let autostart_status: Value = serde_json::from_slice(
        &fs::read(search_refresh_data_root(&temp).join("daemon/status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(autostart_status["status"], "failed", "{autostart_status:#}");
    assert_eq!(
        autostart_status["reason"], "spawn_failed",
        "{autostart_status:#}"
    );
    assert!(!search_refresh_data_root(&temp)
        .join("search/lexical/active-generation.json")
        .exists());
    assert!(!search_refresh_data_root(&temp).join("work.sqlite").exists());

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(
        status["history_epoch"]["status"], "unavailable",
        "{status:#}"
    );
    assert_eq!(status["lexical"]["status"], "unavailable", "{status:#}");
    assert_eq!(status["refresh"]["status"], "unavailable", "{status:#}");
    assert_eq!(
        status["refresh"]["reason"], "daemon_unavailable",
        "{status:#}"
    );
    assert_eq!(status["daemon"]["running"], false, "{status:#}");
    assert_eq!(
        status["daemon"]["core_refresh_endpoint"]["available"], false,
        "{status:#}"
    );
    assert!(status.get("prior_epoch").is_none(), "{status:#}");
}

fn assert_generation_authority_machine_error(
    temp: &TempDir,
    arguments: &[&str],
    generation_id: &str,
    expected_code: &str,
    expected_retryable: bool,
) {
    let output = ctx(temp)
        .args(arguments)
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        output.stdout.is_empty(),
        "{arguments:?}: {:?}",
        output.stdout
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1, "{arguments:?}: {stderr}");
    assert!(!stderr.contains("Error:"), "{arguments:?}: {stderr}");
    let value: Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(
        value["error_code"], expected_code,
        "{arguments:?}: {value:#}"
    );
    assert_eq!(
        value["retryable"], expected_retryable,
        "{arguments:?}: {value:#}"
    );
    assert_eq!(value["error"], value["detail"], "{arguments:?}: {value:#}");
    assert!(
        value["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(generation_id)),
        "{arguments:?}: {value:#}"
    );
}

#[test]
fn query_authority_default_background_search_show_and_locate_use_typed_json() {
    const MISSING_EVENT_ID: &str = "00000000-0000-0000-0000-000000000001";

    for (malformed, error_code, retryable) in [
        (false, "source_unavailable", true),
        (true, "publication_authority_invalid", false),
    ] {
        let temp = tempdir();
        let data_root = search_refresh_data_root(&temp);
        let generation_id = initialize_generation_only_core(&data_root);
        if malformed {
            republish_active_generation_metadata(&data_root, &generation_id, b"{".to_vec());
        }

        for arguments in [
            vec!["search", "authority-oracle", "--format=json"],
            vec![
                "search",
                "authority-oracle",
                "--refresh=background",
                "--format=json",
            ],
            vec!["show", "event", MISSING_EVENT_ID, "--format=json"],
            vec!["locate", "event", MISSING_EVENT_ID, "--format=json"],
        ] {
            assert_generation_authority_machine_error(
                &temp,
                &arguments,
                &generation_id,
                error_code,
                retryable,
            );
        }
    }
}

#[test]
fn persistent_daemon_passively_publishes_appended_source_without_foreground_command() {
    let temp = tempdir();
    let source = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("29")
        .join("watch.jsonl");
    write_codex_session(
        &source,
        "019c08d7-0000-7000-8000-000000000001",
        &[(
            "2026-07-29T12:00:00Z",
            "user",
            "persistent daemon initial oracle",
        )],
    );
    let _daemon = start_source_refresh_daemon(&temp);

    let initial = json_output(ctx(&temp).args([
        "search",
        "persistent daemon initial oracle",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    let initial_generation = assert_published_generation(&initial, "wait");

    append_codex_message(
        &source,
        "2026-07-29T12:01:00Z",
        "assistant",
        "passive append searchable generation oracle",
    );

    // Observe publication only through the daemon-owned receipt. No ctx
    // command is run between the append and the new verified generation.
    let job_path = search_refresh_data_root(&temp).join("daemon/jobs/core-refresh.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    let passive_generation = loop {
        let job = fs::read(&job_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        if let Some(generation) = job.as_ref().and_then(|job| {
            (job["request_state"] == "published")
                .then(|| job["published_generation"].as_str().map(str::to_owned))
                .flatten()
                .filter(|generation| generation != &initial_generation)
        }) {
            break generation;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not passively publish appended source: job={job:#?}, wakeup={:#?}",
            fs::read(search_refresh_data_root(&temp).join("daemon/wakeup.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        );
        std::thread::sleep(Duration::from_millis(25));
    };

    let search = json_output(ctx(&temp).args([
        "search",
        "passive append searchable generation oracle",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(
        search["retrieval"]["generation_id"], passive_generation,
        "{search:#}"
    );
    assert_eq!(
        search["results"].as_array().map(Vec::len),
        Some(1),
        "{search:#}"
    );
}

#[test]
fn live_daemon_prepare_uninstall_disables_stops_and_removes_coordination() {
    let temp = tempdir();
    let mut daemon = start_source_refresh_daemon(&temp);
    let daemon_pid = daemon.pid();

    let report =
        json_output(ctx(&temp).args(["daemon", "disable", "--prepare-uninstall", "--format=json"]));
    assert_eq!(report["command"], "daemon_prepare_uninstall", "{report:#}");
    assert_eq!(report["daemon_enabled"], false, "{report:#}");
    assert_eq!(report["daemon_running"], false, "{report:#}");
    assert_eq!(report["owner_lock_released"], true, "{report:#}");
    assert_eq!(report["endpoint_released"], true, "{report:#}");
    assert_eq!(report["supervisor_removed"], true, "{report:#}");
    assert_eq!(report["coordination_state_removed"], true, "{report:#}");
    assert_eq!(report["retry_safe"], true, "{report:#}");

    let exit = daemon
        .child
        .as_mut()
        .expect("daemon child")
        .wait()
        .expect("wait for uninstalled daemon");
    assert!(exit.success(), "daemon {daemon_pid} exit status: {exit}");
    daemon.child = None;
    let data_root = search_refresh_data_root(&temp);
    let config = fs::read_to_string(data_root.join("config.toml")).unwrap();
    assert!(config.contains("enabled = false"), "{config}");
    for relative in [
        "daemon/daemon.lock",
        "daemon/daemon.guard",
        "daemon/query-endpoint.json",
        "daemon/source-refresh-endpoint.json",
        "daemon/upgrade-handoff.json",
        "daemon/upgrade-restart-requests",
        "daemon/supervisor.json",
    ] {
        assert!(
            !data_root.join(relative).exists(),
            "uninstall retained {relative}"
        );
    }
}

#[test]
fn interrupted_prepare_uninstall_is_retry_safe() {
    let temp = tempdir();
    let mut daemon = start_source_refresh_daemon(&temp);

    let mut interrupted = ctx(&temp);
    interrupted
        .args(["daemon", "disable", "--prepare-uninstall", "--format=json"])
        .env("CTX_DAEMON_UNINSTALL_ABORT_AFTER_DISABLE_FOR_TESTS", "1");
    if cfg!(debug_assertions) {
        interrupted.assert().code(89);
    } else {
        let report = json_output(&mut interrupted);
        assert_eq!(report["ok"], true, "{report:#}");
        assert_eq!(report["retry_safe"], true, "{report:#}");
    }
    let config_after_interrupt =
        fs::read_to_string(search_refresh_data_root(&temp).join("config.toml")).unwrap();
    assert!(
        config_after_interrupt.contains("enabled = false"),
        "{config_after_interrupt}"
    );

    let report =
        json_output(ctx(&temp).args(["daemon", "disable", "--prepare-uninstall", "--format=json"]));
    assert_eq!(report["ok"], true, "{report:#}");
    assert_eq!(report["retry_safe"], true, "{report:#}");
    if let Some(child) = daemon.child.as_mut() {
        let exit = child.wait().expect("wait for interrupted-uninstall daemon");
        assert!(exit.success(), "{exit}");
    }
    daemon.child = None;

    // A third invocation models a host uninstaller retry after Core cleanup
    // completed but before it removed the installed executable.
    let repeated =
        json_output(ctx(&temp).args(["daemon", "disable", "--prepare-uninstall", "--format=json"]));
    assert_eq!(repeated["ok"], true, "{repeated:#}");
    assert_eq!(repeated["daemon_running"], false, "{repeated:#}");
}

#[test]
fn search_refresh_wait_skips_malformed_jsonl_rows() {
    let temp = tempdir();
    write_malformed_claude_session(&temp);
    let _daemon = start_source_refresh_daemon(&temp);

    let output = ctx(&temp)
        .args([
            "search",
            "rejected refresh search marker",
            "--provider",
            "claude",
            "--refresh",
            "wait",
            "--format=json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let search: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_source_backed_search_show_oracle(
        &temp,
        &search,
        "claude",
        "rejected refresh search marker",
        1,
        "message",
    );
    assert_eq!(search["retrieval"]["indexed_documents"], 2, "{search:#}");
    let generation = assert_published_generation(&search, "wait");
    assert_daemon_publication(&temp, &generation, 1, &["claude"]);

    let later_valid_row = json_output(ctx(&temp).args([
        "search",
        "valid rows remain searchable",
        "--provider",
        "claude",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(
        &temp,
        &later_valid_row,
        "claude",
        "valid rows remain searchable",
        1,
        "message",
    );
    assert_eq!(
        later_valid_row["freshness"]["status"],
        "existing_generation"
    );
    assert_eq!(generation_id(&later_valid_row), generation);
}

#[test]
fn search_refresh_wait_human_output_uses_daemon_job_progress_without_stderr_noise() {
    let temp = tempdir();
    write_malformed_claude_session(&temp);
    let _daemon = start_source_refresh_daemon(&temp);

    let output = ctx(&temp)
        .args([
            "search",
            "rejected refresh search marker",
            "--provider",
            "claude",
            "--refresh",
            "wait",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("rejected refresh search marker"),
        "{stdout}"
    );
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    let generation = status["lexical"]["generation_id"]
        .as_str()
        .expect("human search should publish a Core generation")
        .to_owned();
    let status = assert_daemon_publication(&temp, &generation, 1, &["claude"]);
    let job = &status["daemon"]["jobs"]["core_refresh"];
    assert_eq!(job["progress"]["phase"], "published", "{status:#}");
}

fn write_malformed_claude_session(temp: &TempDir) {
    let project = temp.path().join(".claude").join("projects").join("-repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("claude-session.jsonl"),
        concat!(
            r#"{"sessionId":"claude-session","timestamp":"2026-06-24T10:00:00Z","cwd":"/repo","version":"test","type":"user","message":{"role":"user","content":[{"type":"text","text":"rejected refresh search marker"}]},"uuid":"claude-user"}"#,
            "\n",
            "{malformed-jsonl-row\n",
            r#"{"sessionId":"claude-session","timestamp":"2026-06-24T10:00:01Z","cwd":"/repo","version":"test","type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"valid rows remain searchable"}]},"uuid":"claude-assistant"}"#,
            "\n"
        ),
    )
    .unwrap();
}

#[test]
fn search_refresh_off_serves_published_generation_without_refreshing_sources() {
    let temp = tempdir();
    let history = temp.path().join(".codex/history.jsonl");
    fs::create_dir_all(history.parent().unwrap()).unwrap();
    fs::write(
        &history,
        concat!(
            r#"{"session_id":"off-refresh-session","ts":1784371200,"text":"published off mode oracle"}"#,
            "\n"
        ),
    )
    .unwrap();
    let mut daemon = start_source_refresh_daemon(&temp);

    let initial = json_output(ctx(&temp).args([
        "search",
        "published off mode oracle",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    let published_generation = assert_published_generation(&initial, "wait");
    assert_daemon_publication(&temp, &published_generation, 1, &["codex"]);

    daemon.stop();
    write_codex_session(
        &temp
            .path()
            .join(".codex/sessions/2026/07/18/unpublished-off-mode.jsonl"),
        "019fac90-0000-7000-8000-000000000018",
        &[(
            "2026-07-18T12:00:01.000Z",
            "user",
            "unpublishedxylophonicquasar",
        )],
    );

    let off = json_output(ctx(&temp).args([
        "search",
        "unpublishedxylophonicquasar",
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(off["freshness"]["mode"], "off");
    assert_eq!(off["freshness"]["status"], "existing_generation");
    assert_eq!(off["freshness"]["source_count"], 0);
    assert_eq!(generation_id(&off), published_generation);
    assert!(off["results"].as_array().unwrap().is_empty(), "{off:#}");

    let unavailable = failure_stderr(ctx(&temp).args([
        "search",
        "unpublishedxylophonicquasar",
        "--provider",
        "codex",
        "--refresh",
        "background",
        "--format=json",
    ]));
    assert!(
        unavailable.contains("ctx daemon start was suppressed (autostart_disabled)"),
        "{unavailable}"
    );
    let retained = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(
        retained["lexical"]["generation_id"], published_generation,
        "{retained:#}"
    );
}

#[test]
fn search_refresh_wait_recovers_after_invalid_source_is_removed() {
    let temp = tempdir();
    let overlapping_codex_home = search_refresh_data_root(&temp).join("overlapping-codex");
    fs::create_dir_all(&overlapping_codex_home).unwrap();
    let query = "pi-later-good-refresh-oracle";
    install_default_pi_fixture(&temp, query);
    let _daemon = start_source_refresh_daemon_with_codex_home(&temp, Some(&overlapping_codex_home));

    let stderr =
        failure_stderr(ctx(&temp).args(["search", query, "--refresh", "wait", "--format=json"]));
    assert!(
        stderr.contains("daemon-owned source-backed refresh failed"),
        "{stderr}"
    );
    assert!(
        stderr.contains("provider source root")
            && stderr.contains("overlaps or contains the ctx data root"),
        "{stderr}"
    );
    assert!(
        !search_refresh_data_root(&temp)
            .join("search/lexical/active-generation.json")
            .exists(),
        "overlap rejection must happen before Tantivy initialization"
    );
    assert!(
        generation_manifest_paths(&temp).is_empty(),
        "a failed cold scan must not publish a generation manifest"
    );
    let uncommitted = failure_stderr(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        uncommitted.contains("source_unavailable: active verified Core generation is missing"),
        "{uncommitted}"
    );
    let failed = assert_daemon_refresh_failure(&temp, 0, None);
    assert_eq!(
        failed["history_epoch"]["reason"], "core_refresh_failed",
        "{failed:#}"
    );
    assert_eq!(failed["lexical"]["status"], "unavailable", "{failed:#}");

    fs::remove_dir_all(&overlapping_codex_home).unwrap();
    let recovered_output = ctx(&temp)
        .args([
            "search",
            query,
            "--provider",
            "pi",
            "--refresh",
            "wait",
            "--format=json",
        ])
        .output()
        .unwrap();
    assert!(
        recovered_output.status.success(),
        "recovery search failed:\n{}\nstatus:\n{:#}\ngeneration manifests: {:#?}",
        String::from_utf8_lossy(&recovered_output.stderr),
        json_output(ctx(&temp).args(["status", "--format=json"])),
        generation_manifest_paths(&temp),
    );
    let recovered: Value = serde_json::from_slice(&recovered_output.stdout).unwrap();
    assert_source_backed_search_show_oracle(&temp, &recovered, "pi", query, 1, "message");
    let generation = assert_published_generation(&recovered, "wait");
    assert_daemon_publication(&temp, &generation, 1, &["pi"]);
}

#[test]
fn source_refresh_daemon_stop_start_resumes_exact_generation() {
    let temp = tempdir();
    let query = "pi-daemon-restart-resume-oracle";
    install_default_pi_fixture(&temp, query);
    let mut daemon = start_source_refresh_daemon(&temp);
    let first_pid = daemon.pid();

    let initial = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &initial, "pi", query, 1, "message");
    let generation = assert_published_generation(&initial, "wait");
    assert_daemon_publication(&temp, &generation, 1, &["pi"]);

    daemon.stop();
    let stopped = wait_for_status(&temp, "stopped source-refresh daemon", |status| {
        status["daemon"]["running"] == false
    });
    assert_eq!(stopped["daemon"]["running"], false, "{stopped:#}");
    let offline = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &offline, "pi", query, 1, "message");
    assert_eq!(assert_published_generation(&offline, "off"), generation);

    let restarted = restart_source_refresh_daemon(&temp);
    assert_ne!(restarted.pid(), first_pid);
    let resumed = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &resumed, "pi", query, 1, "message");
    assert_eq!(assert_published_generation(&resumed, "wait"), generation);
    assert_daemon_publication(&temp, &generation, 1, &["pi"]);
}

#[test]
fn search_refresh_invalid_source_failure_retains_last_published_generation() {
    let temp = tempdir();
    let overlapping_codex_home = search_refresh_data_root(&temp).join("overlapping-codex");
    let query = "pi-retained-generation-oracle";
    install_default_pi_fixture(&temp, query);
    let _daemon = start_source_refresh_daemon_with_codex_home(&temp, Some(&overlapping_codex_home));

    let initial = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    let initial_generation = assert_published_generation(&initial, "wait");
    assert_daemon_publication(&temp, &initial_generation, 1, &["pi"]);

    fs::create_dir_all(&overlapping_codex_home).unwrap();
    let stderr = failure_stderr(ctx(&temp).args([
        "search",
        "anything",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert!(
        stderr.contains("provider source root")
            && stderr.contains("overlaps or contains the ctx data root"),
        "{stderr}"
    );
    assert!(stderr.contains("retained_generation=Some"), "{stderr}");
    assert!(stderr.contains(&initial_generation), "{stderr}");

    let failed = assert_daemon_refresh_failure(&temp, 0, Some(&initial_generation));
    assert_eq!(failed["history_epoch"]["status"], "ready", "{failed:#}");
    assert_eq!(failed["lexical"]["status"], "ready", "{failed:#}");
    assert!(failed["lexical"]["reason"].is_null(), "{failed:#}");
    assert_eq!(
        failed["lexical"]["generation_id"], initial_generation,
        "{failed:#}"
    );

    let retained = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &retained, "pi", query, 1, "message");
    assert_eq!(retained["freshness"]["status"], "existing_generation");
    assert_eq!(generation_id(&retained), initial_generation);
}

#[test]
fn search_refresh_imports_fresh_work_after_large_source_backed_generation() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("codex-sessions"));
    let discovered = temp.path().join(".codex").join("sessions");
    copy_dir_all(&fixture, &discovered);
    let root_session = discovered.join("2026/06/23/root.jsonl");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&root_session)
        .unwrap();
    for index in 0..10_000 {
        writeln!(
            file,
            "{}",
            json!({
                "timestamp": "2026-06-23T15:00:00.000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": format!("large-source-backed-baseline-{index}")
                    }]
                }
            })
        )
        .unwrap();
    }
    drop(file);
    let _daemon = start_source_refresh_daemon(&temp);

    let initial = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &initial, "codex", "onboarding", 1, "message");
    let initial_generation = assert_published_generation(&initial, "wait");
    let initial_documents = initial["retrieval"]["indexed_documents"].as_u64().unwrap();
    assert!(initial_documents >= 10_000, "{initial:#}");
    assert_daemon_publication(&temp, &initial_generation, 1, &["codex", "codex"]);

    let fresh_query = "fresh work after large source generation oracle";
    let history = temp.path().join(".codex/history.jsonl");
    fs::write(
        &history,
        format!(
            "{{\"session_id\":\"large-generation-fresh\",\"ts\":1784371200,\"text\":\"{fresh_query}\"}}\n"
        ),
    )
    .unwrap();
    let fresh = json_output(ctx(&temp).args([
        "search",
        fresh_query,
        "--provider",
        "codex",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_source_backed_search_show_oracle(&temp, &fresh, "codex", fresh_query, 1, "message");
    assert_eq!(fresh["freshness"]["source_count"], 2);
    let fresh_generation = assert_published_generation(&fresh, "wait");
    assert_ne!(fresh_generation, initial_generation);
    assert!(
        fresh["retrieval"]["indexed_documents"].as_u64().unwrap() > initial_documents,
        "{fresh:#}"
    );
    assert_daemon_publication(&temp, &fresh_generation, 2, &["codex", "codex", "codex"]);
    assert!(!search_refresh_data_root(&temp).join("work.sqlite").exists());
}

fn active_generation_meta_path(index_root: &Path, expected_generation: &str) -> PathBuf {
    let pointer_path = index_root.join("active-generation.json");
    let pointer: Value = serde_json::from_slice(
        &fs::read(&pointer_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", pointer_path.display())),
    )
    .unwrap_or_else(|error| panic!("parse {}: {error}", pointer_path.display()));
    assert_eq!(
        pointer["active"]["generation_id"].as_str(),
        Some(expected_generation),
        "{pointer:#}"
    );
    let directory = pointer["active"]["directory"]
        .as_str()
        .expect("active generation pointer directory");
    index_root
        .join("index-generations")
        .join(directory)
        .join("meta.json")
}
