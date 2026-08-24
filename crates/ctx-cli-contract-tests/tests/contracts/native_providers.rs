mod support;

use ctx_history_index::VerifiedIndex;
use support::{daemon_test_root as tempdir, *};

#[path = "native_providers/factory_retry.rs"]
mod factory_retry;
#[path = "support/native_providers/daemon.rs"]
mod provider_daemon;
#[path = "support/native_providers/sqlite_sources.rs"]
mod sqlite_sources;
#[path = "support/native_providers/workspace_sources.rs"]
mod workspace_sources;

use provider_daemon::*;

#[test]
fn qwen_kimi_mistral_mux_and_qoder_default_sources_import_search_and_reimport() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("qwen-code/.qwen")),
        &temp.path().join(".qwen"),
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("kimi-code-cli/.kimi-code")),
        &temp.path().join(".kimi-code"),
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("mistral-vibe/v1/logs/session")),
        &temp.path().join(".vibe").join("logs").join("session"),
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("mux/v0.27.0/sessions")),
        &temp.path().join(".mux").join("sessions"),
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("qoder/projects")),
        &temp.path().join(".qoder").join("projects"),
    );
    let _daemon = start_isolated_provider_daemon(&temp);

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
    for (provider, source_format) in [
        ("qwen_code", "qwen_code_chat_jsonl_tree"),
        ("kimi_code_cli", "kimi_code_cli_wire_jsonl_tree"),
        ("mistral_vibe", "mistral_vibe_session_jsonl_tree"),
        ("mux", "mux_session_jsonl_tree"),
        ("qoder", "qoder_transcript_jsonl_tree"),
    ] {
        let source = sources["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| {
                source["provider"] == provider && source["source_format"] == source_format
            })
            .unwrap_or_else(|| panic!("missing {provider} source in {sources:#}"));
        assert_eq!(source["status"], "available");
        assert_eq!(source["import_support"], "native");
        assert_eq!(source["native_import"], true);
        assert_eq!(source["importable"], true);
    }

    // Qwen owns the fixture's one malformed record. Each import receipt is
    // provider-scoped even though the current-generation total remains one.
    for (cli_provider, stored_provider, query, minimum_events, rejected_records) in [
        ("qwen-code", "qwen_code", "qwen jsonl oracle prompt", 2, 1),
        (
            "kimi-code-cli",
            "kimi_code_cli",
            "kimi jsonl oracle prompt",
            5,
            0,
        ),
        (
            "mistral-vibe",
            "mistral_vibe",
            "mistral vibe oracle prompt",
            3,
            0,
        ),
        ("mux", "mux", "mux jsonl oracle prompt", 4, 0),
        ("qoder", "qoder", "qoder jsonl oracle prompt", 6, 0),
    ] {
        let first = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--no-daemon",
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_authoritative_provider_publication_with_rejections(&first, rejected_records);
        if stored_provider == "qwen_code" {
            let diagnostic = first["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|source| source["status"] == "rejection")
                .unwrap_or_else(|| panic!("missing Qwen rejection diagnostic in {first:#}"));
            assert_eq!(diagnostic["provider"], "qwen_code", "{first:#}");
            assert_eq!(diagnostic["line"], 3, "{first:#}");
            assert!(
                diagnostic["detail"]
                    .as_str()
                    .is_some_and(|detail| detail.contains("invalid shape")),
                "{first:#}"
            );
        }
        wait_for_imported_core(&temp, &first);
        assert_eq!(first["totals"]["current_rejected_records"], 1, "{first:#}");
        let (session_count, event_count) = provider_core_counts(&data_root(&temp), stored_provider);
        assert!(
            event_count >= minimum_events,
            "expected at least {minimum_events} indexed {stored_provider} events"
        );
        if stored_provider == "mux" {
            assert_eq!(
                session_count, 2,
                "manifested Mux files must reuse one canonical source-scoped parent session"
            );
        }

        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            cli_provider,
            "--refresh",
            "off",
            "--limit",
            "1",
            "--format=json",
        ]));
        assert_source_backed_search(&search, stored_provider, query);

        let second = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--no-daemon",
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_authoritative_provider_publication_with_rejections(&second, rejected_records);
        assert_eq!(
            second["totals"]["current_rejected_records"], 1,
            "{second:#}"
        );
    }
}

#[test]
fn mimocode_default_and_env_sources_import_search_and_reimport() {
    let temp = tempdir();
    let default_query = "mimocode-default-discovery-oracle";
    let default_db = temp
        .path()
        .join(".local")
        .join("share")
        .join("mimocode")
        .join("mimocode.db");
    install_default_mimocode_fixture(&temp, default_query);

    let sources = json_output(ctx(&temp).args(["sources", "--format=json", "--all"]));
    let source = source_by_path(&sources, "mimocode", &default_db);
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "mimocode_sqlite");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["native_import"], true);
    assert_eq!(source["importable"], true);

    let search = json_output(ctx(&temp).args([
        "search",
        default_query,
        "--provider",
        "mimo-code",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    let freshness_mode = search["freshness"]["mode"].as_str().unwrap();
    assert_eq!(
        freshness_mode, "wait",
        "unexpected freshness mode in {search:#}"
    );
    assert_eq!(search["freshness"]["status"], "completed");
    assert!(
        search["retrieval"]["indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "expected MiMo documents in {search:#}"
    );
    assert_search_provider_oracle(&search, "mimocode", default_query, 1, "message");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "mimo_code",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_authoritative_provider_publication(&second);
    assert_eq!(
        second["totals"]["current_rejected_records"], 0,
        "{second:#}"
    );

    drop(temp);
    let temp = tempdir();
    let default_db = temp.path().join(".local/share/mimocode/mimocode.db");
    let home_query = "mimocode-home-env-oracle";
    let mimocode_home = temp.path().join("mimocode-home");
    let home_db = mimocode_home.join("data").join("mimocode.db");
    write_mimocode_sqlite_fixture(&home_db, home_query, "mimocode-home");
    let home_sources = json_output(ctx(&temp).env("MIMOCODE_HOME", &mimocode_home).args([
        "sources",
        "--format=json",
        "--all",
    ]));
    assert_eq!(
        source_by_path(&home_sources, "mimocode", &home_db)["status"],
        "available"
    );
    assert!(
        !has_provider_source_path(&home_sources, "mimocode", &default_db),
        "MIMOCODE_HOME should replace the default MiMo data root: {home_sources:#}"
    );
    let home_import = json_output(ctx(&temp).env("MIMOCODE_HOME", &mimocode_home).args([
        "import",
        "--provider",
        "mimocode",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_authoritative_provider_publication(&home_import);
    assert_eq!(home_import["totals"]["current_rejected_records"], 0);
    let home_search = json_output(ctx(&temp).args([
        "search",
        home_query,
        "--provider",
        "mimocode",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&home_search, "mimocode", home_query, 1, "message");

    drop(temp);
    let temp = tempdir();
    let custom_query = "mimocode-db-env-oracle";
    let custom_db = temp.path().join("custom-mimocode.db");
    write_mimocode_sqlite_fixture(&custom_db, custom_query, "mimocode-custom");
    let custom_import = json_output(ctx(&temp).env("MIMOCODE_DB", &custom_db).args([
        "import",
        "--provider",
        "mimocode",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_authoritative_provider_publication(&custom_import);
    assert_eq!(custom_import["totals"]["current_rejected_records"], 0);
    let custom_search = json_output(ctx(&temp).args([
        "search",
        custom_query,
        "--provider",
        "mimocode",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&custom_search, "mimocode", custom_query, 1, "message");

    drop(temp);
    let temp = tempdir();
    let xdg_data = temp.path().join("xdg-data");
    let channel_db = xdg_data.join("mimocode").join("mimocode-nightly.db");
    write_mimocode_sqlite_fixture(&channel_db, "mimocode-channel-oracle", "mimocode-channel");
    let channel_sources = json_output(ctx(&temp).env("XDG_DATA_HOME", &xdg_data).args([
        "sources",
        "--format=json",
        "--all",
    ]));
    assert!(
        !has_provider_source_path(&channel_sources, "mimocode", &channel_db),
        "unregistered channel databases must not be discovered: {channel_sources:#}"
    );
    let selected_xdg_db = xdg_data.join("mimocode").join("mimocode.db");
    assert_eq!(
        source_by_path(&channel_sources, "mimocode", &selected_xdg_db)["status"],
        "missing"
    );

    let relative_db = xdg_data.join("mimocode").join("relative.db");
    write_mimocode_sqlite_fixture(
        &relative_db,
        "mimocode-relative-db-oracle",
        "mimocode-relative",
    );
    let relative_sources = json_output(
        ctx(&temp)
            .env("XDG_DATA_HOME", &xdg_data)
            .env("MIMOCODE_DB", "relative.db")
            .args(["sources", "--format=json", "--all"]),
    );
    assert_eq!(
        source_by_path(&relative_sources, "mimocode", &relative_db)["status"],
        "available"
    );
    assert!(
        !has_provider_source_path(&relative_sources, "mimocode", &channel_db),
        "MIMOCODE_DB should select one explicit MiMo database"
    );
}

#[cfg(unix)]
#[test]
fn copilot_cli_import_skips_symlinked_session_files_checkout() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let query = "copilot-cli-symlinked-files-oracle";
    let path = write_native_copilot_fixture(&temp, query);
    // Copilot CLI stores the session working copy under
    // `<session>/files/`, and checked-out repositories legitimately contain
    // symlinks (for example `CLAUDE.md -> AGENTS.md`). The link can never
    // hold an `events.jsonl` transcript, so it must be skipped without
    // failing the whole copilot_cli source.
    let checkout = Path::new(&path).join("copilot-cli-native/files/checkout");
    fs::create_dir_all(&checkout).unwrap();
    fs::write(checkout.join("AGENTS.md"), b"agents\n").unwrap();
    symlink("AGENTS.md", checkout.join("CLAUDE.md")).unwrap();
    let outside_query = "outsidesymlinktargetoracle9f27c4";
    let outside_session = temp.path().join("outside-copilot-session");
    fs::create_dir_all(&outside_session).unwrap();
    let selected_transcript = Path::new(&path).join("copilot-cli-native/events.jsonl");
    let outside_transcript = fs::read_to_string(&selected_transcript)
        .unwrap()
        .replace(query, outside_query);
    fs::write(outside_session.join("events.jsonl"), outside_transcript).unwrap();
    symlink(&outside_session, checkout.join("linked-session")).unwrap();
    let _daemon = start_isolated_provider_daemon(&temp);

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "copilot-cli",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&first, "copilot_cli", "copilot_cli_session_events_jsonl");
    wait_for_imported_core(&temp, &first);
    assert_eq!(first["totals"]["current_rejected_records"], 0, "{first:#}");
    let (session_count, event_count) = provider_core_counts(&data_root(&temp), "copilot_cli");
    assert!(session_count >= 1);
    assert!(event_count >= 1);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "copilot-cli",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "copilot_cli", query, 1, "message");
    let outside_search = json_output(ctx(&temp).args([
        "search",
        outside_query,
        "--provider",
        "copilot-cli",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        outside_search["results"].as_array().unwrap().is_empty(),
        "outside symlink target leaked into Copilot inventory: {outside_search:#}"
    );

    // A second import exercises the membership fence walk against the same
    // symlinked checkout and must republish cleanly.
    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "copilot-cli",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&second, "copilot_cli", "copilot_cli_session_events_jsonl");
    assert_eq!(
        second["totals"]["current_rejected_records"], 0,
        "{second:#}"
    );
}

#[cfg(unix)]
#[test]
fn copilot_cli_reimport_fails_when_transcript_turns_into_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let query = "copilot-cli-transcript-to-symlink-oracle";
    let path = write_native_copilot_fixture(&temp, query);
    let _daemon = start_isolated_provider_daemon(&temp);

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "copilot-cli",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&first, "copilot_cli", "copilot_cli_session_events_jsonl");
    wait_for_imported_core(&temp, &first);

    // A transcript route that turns into a link after admission drops out of
    // the observed membership route set, so the re-import must fail closed
    // instead of silently keeping stale content.
    let session = Path::new(&path).join("copilot-cli-native");
    let real_transcript = session.join("events.real.jsonl");
    fs::rename(session.join("events.jsonl"), &real_transcript).unwrap();
    symlink("events.real.jsonl", session.join("events.jsonl")).unwrap();

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "copilot-cli",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    assert_eq!(second["failure_type"], "source_failure", "{second:#}");
    assert_eq!(
        second["outcome"], "completed_with_source_failures",
        "{second:#}"
    );
    assert_eq!(second["sources"][0]["carried_forward"], true, "{second:#}");
}

#[cfg(unix)]
#[test]
fn copilot_cli_import_still_rejects_symlinked_transcript() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let query = "copilot-cli-symlinked-transcript-oracle";
    let path = write_native_copilot_fixture(&temp, query);
    // A symlink where the transcript itself should be stays fail-closed:
    // skipping is only safe for entries that can never hold a transcript.
    let session = Path::new(&path).join("copilot-cli-native");
    let real_transcript = session.join("events.real.jsonl");
    fs::rename(session.join("events.jsonl"), &real_transcript).unwrap();
    symlink("events.real.jsonl", session.join("events.jsonl")).unwrap();
    let _daemon = start_isolated_provider_daemon(&temp);

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "copilot-cli",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    assert!(
        stderr.contains("symlinked provider source path components are rejected"),
        "{stderr}"
    );
}

#[test]
fn unknown_native_providers_are_rejected_by_public_cli() {
    let temp = tempdir();

    for provider in ["not-a-real-provider", "unsupported-provider-placeholder"] {
        let stderr =
            failure_stderr(ctx(&temp).args(["import", "--provider", provider, "--format=json"]));
        assert!(stderr.contains("unknown provider"), "{provider}: {stderr}");
    }
}

#[test]
fn native_provider_cli_flow_imports_supported_provider_paths() {
    for (cli_provider, stored_provider, expected_format, fixture) in [
        (
            "claude",
            "claude",
            "claude_projects_jsonl_tree",
            write_native_claude_fixture as fn(&TempDir, &str) -> String,
        ),
        (
            "opencode",
            "opencode",
            "opencode_sqlite",
            write_native_opencode_fixture,
        ),
        (
            "mimocode",
            "mimocode",
            "mimocode_sqlite",
            write_native_mimocode_fixture,
        ),
        ("kilo", "kilo", "kilo_sqlite", write_native_kilo_fixture),
        (
            "kiro-cli",
            "kiro_cli",
            "kiro_cli_sqlite",
            write_native_kiro_fixture,
        ),
        (
            "gemini",
            "gemini",
            "gemini_cli_chat_recording_jsonl",
            write_native_gemini_fixture,
        ),
        (
            "cursor",
            "cursor",
            "cursor_agent_transcript_jsonl_tree",
            write_native_cursor_fixture,
        ),
        (
            "copilot-cli",
            "copilot_cli",
            "copilot_cli_session_events_jsonl",
            write_native_copilot_fixture,
        ),
        (
            "factory-ai-droid",
            "factory_ai_droid",
            "factory_ai_droid_sessions_jsonl",
            write_native_factory_droid_fixture,
        ),
        (
            "qwen-code",
            "qwen_code",
            "qwen_code_chat_jsonl_tree",
            write_native_qwen_fixture,
        ),
        (
            "kimi-code-cli",
            "kimi_code_cli",
            "kimi_code_cli_wire_jsonl_tree",
            write_native_kimi_fixture,
        ),
        (
            "forgecode",
            "forgecode",
            "forgecode_sqlite",
            write_native_forgecode_fixture,
        ),
        (
            "mistral-vibe",
            "mistral_vibe",
            "mistral_vibe_session_jsonl_tree",
            write_native_mistral_vibe_fixture,
        ),
        (
            "mux",
            "mux",
            "mux_session_jsonl_tree",
            write_native_mux_fixture,
        ),
        (
            "rovodev",
            "rovodev",
            "rovodev_session_json_tree",
            write_native_rovodev_fixture,
        ),
        (
            "lingma",
            "lingma",
            "lingma_sqlite",
            write_native_lingma_fixture,
        ),
        (
            "codebuddy",
            "codebuddy",
            "codebuddy_history_json",
            write_native_codebuddy_fixture,
        ),
        (
            "auggie",
            "auggie",
            "auggie_session_json",
            write_native_auggie_fixture,
        ),
        (
            "junie",
            "junie",
            "junie_session_events_jsonl_tree",
            write_native_junie_fixture,
        ),
        (
            "firebender",
            "firebender",
            "firebender_chat_history_sqlite",
            write_native_firebender_fixture,
        ),
        (
            "openclaw",
            "openclaw",
            "openclaw_session_jsonl_tree",
            write_native_openclaw_fixture,
        ),
        (
            "hermes",
            "hermes",
            "hermes_state_sqlite",
            write_native_hermes_fixture,
        ),
        (
            "nanoclaw",
            "nanoclaw",
            "nanoclaw_project",
            write_native_nanoclaw_fixture,
        ),
        (
            "continue",
            "continue",
            "continue_cli_sessions_json",
            write_native_continue_fixture,
        ),
        (
            "openhands",
            "openhands",
            "openhands_file_events",
            write_native_openhands_fixture,
        ),
        (
            "qoder",
            "qoder",
            "qoder_transcript_jsonl_tree",
            write_native_qoder_fixture,
        ),
    ] {
        let temp = tempdir();
        let query = format!("{stored_provider}-cli-flow-oracle");
        let path = fixture(&temp, &query);
        let _daemon = start_isolated_provider_daemon(&temp);

        let first = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &path,
            "--no-daemon",
            "--format=json",
        ]));
        if stored_provider == "qwen_code" {
            assert_explicit_source_publication_with_rejections(
                &first,
                stored_provider,
                expected_format,
                1,
            );
        } else {
            assert_explicit_source_publication(&first, stored_provider, expected_format);
        }
        wait_for_imported_core(&temp, &first);
        let expected_rejected_records = u64::from(stored_provider == "qwen_code");
        assert_eq!(
            first["totals"]["current_rejected_records"], expected_rejected_records,
            "{first:#}"
        );
        let (session_count, event_count) = provider_core_counts(&data_root(&temp), stored_provider);
        assert!(session_count >= 1);
        assert!(event_count >= 1);

        let search = json_output(ctx(&temp).args([
            "search",
            &query,
            "--provider",
            cli_provider,
            "--refresh",
            "off",
            "--limit",
            "1",
            "--format=json",
        ]));
        assert_search_provider_oracle(&search, stored_provider, &query, 1, "message");
    }
}

#[test]
fn discovery_only_sqlite_explicit_paths_are_rejected_without_fallback() {
    let temp = tempdir();
    let path = write_native_astrbot_fixture(&temp, "astrbot-explicit-unsupported-oracle");
    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "astrbot",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    assert!(
        stderr.contains("requires provider discovery authority")
            && stderr.contains("no legacy import fallback was used"),
        "{stderr}"
    );
}

#[test]
fn shelley_imports_exact_cwd_automatically_and_exact_path_explicitly() {
    let temp = tempdir();
    let automatic_query = "shelleyautomaticcwdonlymarker";
    let automatic_fixture = PathBuf::from(write_native_shelley_fixture(&temp, automatic_query));
    let automatic_path = temp.path().join("shelley.db");
    fs::copy(automatic_fixture, &automatic_path).unwrap();
    let _daemon = start_isolated_provider_daemon(&temp);

    let automatic_first = json_output(ctx(&temp).current_dir(temp.path()).args([
        "import",
        "--provider",
        "shelley",
        "--no-daemon",
        "--format=json",
    ]));
    assert_authoritative_provider_publication(&automatic_first);
    wait_for_imported_core(&temp, &automatic_first);
    let automatic_snapshot = shelley_lifecycle_snapshot(&data_root(&temp));
    assert_eq!(
        automatic_snapshot.route_ownership.len(),
        1,
        "{automatic_first:#}"
    );
    let automatic_route = automatic_snapshot.route_ownership[0].0.clone();
    assert_eq!(
        automatic_snapshot.source_ids.len(),
        1,
        "{automatic_first:#}"
    );
    assert_eq!(
        (
            automatic_snapshot.session_ids.len(),
            automatic_snapshot.record_count
        ),
        (1, 2),
        "{automatic_first:#}"
    );
    assert_eq!(
        automatic_snapshot.route_ownership,
        vec![(
            automatic_route.clone(),
            automatic_snapshot.source_ids.clone()
        )]
    );

    let explicit_temp = tempdir();
    let explicit_query = "shelleyexplicitpathonlymarker";
    let explicit_fixture =
        PathBuf::from(write_native_shelley_fixture(&explicit_temp, explicit_query));
    let explicit_path = explicit_temp.path().join("selected/shelley.db");
    fs::create_dir_all(explicit_path.parent().unwrap()).unwrap();
    fs::copy(explicit_fixture, &explicit_path).unwrap();

    let explicit_first = json_output(ctx(&temp).current_dir(temp.path()).args([
        "import",
        "--provider",
        "shelley",
        "--path",
        explicit_path.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&explicit_first, "shelley", "shelley_sqlite");
    wait_for_imported_core(&temp, &explicit_first);
    let explicit_route = explicit_first["sources"][0]["route_identity"]
        .as_str()
        .unwrap()
        .to_owned();
    let explicit_lineage = explicit_first["sources"][0]["catalog_lineage"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(explicit_route, automatic_route);
    let explicit_snapshot = shelley_lifecycle_snapshot(&data_root(&temp));
    assert_eq!(explicit_snapshot.source_ids, automatic_snapshot.source_ids);
    assert_eq!(
        explicit_snapshot.session_ids,
        automatic_snapshot.session_ids
    );
    assert_eq!(explicit_snapshot.event_ids, automatic_snapshot.event_ids);
    assert_eq!(
        explicit_snapshot.record_count,
        automatic_snapshot.record_count
    );
    assert_eq!(
        explicit_snapshot.route_ownership,
        vec![(explicit_route.clone(), explicit_snapshot.source_ids.clone())]
    );
    let explicit_bodies = provider_core_records(&data_root(&temp), "shelley")
        .into_iter()
        .filter_map(|record| record.content.normalized_body)
        .collect::<Vec<_>>();
    assert!(
        explicit_bodies
            .iter()
            .any(|body| body.contains(explicit_query)),
        "exact --path content was not published: {explicit_bodies:#?}"
    );
    assert!(
        explicit_bodies
            .iter()
            .all(|body| !body.contains(automatic_query)),
        "competing automatic-CWD content survived exact --path replacement: {explicit_bodies:#?}"
    );

    let explicit_search = json_output(ctx(&temp).args([
        "search",
        explicit_query,
        "--provider",
        "shelley",
        "--refresh",
        "off",
        "--limit",
        "1",
        "--format=json",
    ]));
    assert_search_provider_oracle(&explicit_search, "shelley", explicit_query, 1, "message");
    let competing_cwd_search = json_output(ctx(&temp).args([
        "search",
        automatic_query,
        "--provider",
        "shelley",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(
        competing_cwd_search["results"].as_array().unwrap().len(),
        0,
        "exact --path did not take precedence over the competing CWD database: {competing_cwd_search:#}"
    );

    let changed_query = "shelleycontentchangedonlymarker";
    append_native_shelley_event(explicit_path.to_str().unwrap(), changed_query);
    let explicit_changed = json_output(ctx(&temp).current_dir(temp.path()).args([
        "import",
        "--provider",
        "shelley",
        "--path",
        explicit_path.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&explicit_changed, "shelley", "shelley_sqlite");
    assert_eq!(
        explicit_changed["sources"][0]["route_identity"],
        explicit_route
    );
    assert_eq!(
        explicit_changed["sources"][0]["catalog_lineage"],
        explicit_lineage
    );
    assert_ne!(
        explicit_changed["sources"][0]["published_generation"],
        explicit_first["sources"][0]["published_generation"]
    );
    let changed_snapshot = shelley_lifecycle_snapshot(&data_root(&temp));
    assert_eq!(changed_snapshot.source_ids, explicit_snapshot.source_ids);
    assert_eq!(changed_snapshot.session_ids, explicit_snapshot.session_ids);
    assert!(changed_snapshot
        .event_ids
        .is_superset(&explicit_snapshot.event_ids));
    assert_eq!(
        changed_snapshot.event_ids.len(),
        explicit_snapshot.event_ids.len() + 1
    );
    assert_eq!(
        changed_snapshot.record_count,
        explicit_snapshot.record_count + 1
    );
    assert_eq!(
        changed_snapshot.route_ownership,
        vec![(explicit_route, changed_snapshot.source_ids.clone())]
    );
    let changed_search = json_output(ctx(&temp).args([
        "search",
        changed_query,
        "--provider",
        "shelley",
        "--refresh",
        "off",
        "--limit",
        "1",
        "--format=json",
    ]));
    assert_search_provider_oracle(&changed_search, "shelley", changed_query, 1, "message");
}

#[derive(Debug, PartialEq, Eq)]
struct ShelleyLifecycleSnapshot {
    source_ids: BTreeSet<String>,
    session_ids: BTreeSet<String>,
    event_ids: BTreeSet<String>,
    record_count: usize,
    route_ownership: Vec<(String, BTreeSet<String>)>,
}

fn shelley_lifecycle_snapshot(data_root: &Path) -> ShelleyLifecycleSnapshot {
    let records = provider_core_records(data_root, "shelley");
    let index = VerifiedIndex::open(data_root.join("search/lexical")).unwrap();
    let route_ownership = index
        .manifest()
        .source_routes()
        .iter()
        .filter_map(|route| {
            let source_ids = route
                .sources()
                .iter()
                .filter(|source| source.provider() == "shelley")
                .map(|source| source.identity().to_string())
                .collect::<BTreeSet<_>>();
            (!source_ids.is_empty())
                .then(|| (route.route_identity().as_str().to_owned(), source_ids))
        })
        .collect();
    ShelleyLifecycleSnapshot {
        source_ids: records
            .iter()
            .map(|record| record.source.identity().to_string())
            .collect(),
        session_ids: records
            .iter()
            .map(|record| record.session_id.to_string())
            .collect(),
        event_ids: records
            .iter()
            .map(|record| record.event_id.to_string())
            .collect(),
        record_count: records.len(),
        route_ownership,
    }
}

fn source_by_path<'a>(packet: &'a Value, provider: &str, path: &Path) -> &'a Value {
    let expected_path = path.display().to_string();
    packet["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == provider
                && source["path"]
                    .as_str()
                    .is_some_and(|path| path == expected_path)
        })
        .unwrap_or_else(|| panic!("missing {provider} source {expected_path} in {packet:#}"))
}

fn has_provider_source_path(packet: &Value, provider: &str, path: &Path) -> bool {
    let expected_path = path.display().to_string();
    packet["sources"].as_array().unwrap().iter().any(|source| {
        source["provider"] == provider
            && source["path"]
                .as_str()
                .is_some_and(|path| path == expected_path)
    })
}

#[test]
fn native_provider_cli_preserves_complete_tool_outputs_without_legacy_payloads() {
    for (provider, source_format, fixture, query, sentinel, output_is_searchable) in [
        (
            "qoder",
            "qoder_transcript_jsonl_tree",
            write_native_qoder_fixture as fn(&TempDir, &str) -> String,
            "qoder-policy-real-message-oracle",
            "qoderleakproofxylophonium",
            true,
        ),
        (
            "openhands",
            "openhands_file_events",
            write_native_openhands_fixture,
            "openhands-policy-real-message-oracle",
            "openhandssuccesstooloutputsentinel",
            true,
        ),
        (
            "continue",
            "continue_cli_sessions_json",
            write_native_continue_fixture,
            "continue-policy-real-message-oracle",
            "continuesuccesstooloutputsentinel",
            false,
        ),
    ] {
        let temp = tempdir();
        let path = fixture(&temp, query);
        let imported = json_output(ctx(&temp).args([
            "import",
            "--provider",
            provider,
            "--path",
            &path,
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_explicit_source_publication(&imported, provider, source_format);
        wait_for_imported_core(&temp, &imported);
        assert_eq!(
            imported["totals"]["current_rejected_records"], 0,
            "{imported:#}"
        );

        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            provider,
            "--refresh",
            "off",
            "--format=json",
        ]));
        assert_source_backed_search(&search, provider, query);

        let search = json_output(ctx(&temp).args([
            "search",
            sentinel,
            "--provider",
            provider,
            "--refresh",
            "off",
            "--format=json",
        ]));
        if output_is_searchable {
            assert_source_backed_search(&search, provider, sentinel);
        } else {
            assert!(
                search["results"].as_array().unwrap().is_empty(),
                "{provider} must not invent a Core event for nested result context: {search:#}"
            );
        }

        assert!(
            provider_core_records(&data_root(&temp), provider)
                .iter()
                .all(|record| !serde_json::to_value(record)
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .contains_key("repository_file_observations")),
            "unscoped file paths must not cross the Core repository boundary"
        );
    }
}

#[path = "native_providers/additional.rs"]
mod additional;
