mod support;

use support::*;

fn success_stdout(command: &mut Command) -> String {
    let stdout = command.assert().success().get_output().stdout.clone();
    String::from_utf8(stdout).unwrap()
}

fn object_keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn sources_json_keeps_the_v1_top_level_and_source_fields() {
    let temp = tempdir();
    let codex_home = temp.path().join("codex-home");
    fs::create_dir_all(codex_home.join("sessions")).unwrap();
    fs::write(codex_home.join("sessions/session.jsonl"), "{}\n").unwrap();
    let packet = json_output(ctx(&temp).env("CODEX_HOME", codex_home).args([
        "sources",
        "--provider",
        "codex",
        "--format=json",
    ]));

    assert_eq!(packet["schema_version"], 1);
    assert_eq!(
        object_keys(&packet),
        BTreeSet::from([
            "hidden_missing_sources",
            "issues",
            "issues_truncated",
            "schema_version",
            "scope",
            "sources",
        ])
    );
    assert_eq!(packet["issues_truncated"], false);
    let source = packet["sources"].as_array().unwrap().first().unwrap();
    assert_eq!(
        object_keys(source),
        BTreeSet::from([
            "exists",
            "import_support",
            "importable",
            "native_import",
            "path",
            "provider",
            "source_format",
            "status",
            "unsupported_reason",
        ])
    );

    let no_path_report =
        json_output(ctx(&temp).args(["sources", "--provider", "firebender", "--format=json"]));
    assert_eq!(object_keys(&no_path_report), object_keys(&packet));
    assert!(no_path_report["sources"].as_array().unwrap().is_empty());
    assert_eq!(no_path_report["issues_truncated"], false);
    let issues = no_path_report["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0]["provider"], "firebender");
    assert!(issues[0]["path"].is_null());
    assert_eq!(issues[0]["code"], "insufficient_official_evidence");
    assert_eq!(issues[0]["message_truncated"], false);
}

#[test]
fn provider_filtered_human_sources_and_import_errors_are_actionable() {
    let no_disk = tempdir();
    let stdout = success_stdout(ctx(&no_disk).env("OPENCODE_DB", ":memory:").args([
        "sources",
        "--provider",
        "opencode",
    ]));
    assert!(stdout.contains("has no disk history selected"), "{stdout}");
    assert!(
        stdout.contains("ctx import --provider opencode --path <path>"),
        "{stdout}"
    );
    let stderr = failure_stderr(ctx(&no_disk).env("OPENCODE_DB", ":memory:").args([
        "import",
        "--provider",
        "opencode",
        "--format=json",
    ]));
    assert!(stderr.contains("has no disk history selected"), "{stderr}");
    assert!(
        stderr.contains("ctx import --provider opencode --path <path>"),
        "{stderr}"
    );

    let unreconstructible = tempdir();
    let stdout = success_stdout(
        ctx(&unreconstructible)
            .env("CLAUDE_CONFIG_DIR", "relative-provider-root")
            .args(["sources", "--provider", "claude"]),
    );
    assert!(
        stdout.contains("history location could not be selected safely"),
        "{stdout}"
    );
    assert!(
        stdout.contains("ctx import --provider claude --path <path>"),
        "{stdout}"
    );
    assert!(!stdout.contains("relative-provider-root"), "{stdout}");
    let stderr = failure_stderr(
        ctx(&unreconstructible)
            .env("CLAUDE_CONFIG_DIR", "relative-provider-root")
            .args(["import", "--provider", "claude", "--format=json"]),
    );
    assert!(
        stderr.contains("automatic history location cannot be safely reconstructed"),
        "{stderr}"
    );
    assert!(!stderr.contains("relative-provider-root"), "{stderr}");

    let unestablished = tempdir();
    let stdout = success_stdout(ctx(&unestablished).args(["sources", "--provider", "firebender"]));
    assert!(
        stdout.contains("has no established automatic history location"),
        "{stdout}"
    );
    assert!(
        stdout.contains("ctx import --provider firebender --path <path>"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("insufficient_official_evidence"),
        "{stdout}"
    );
    let stderr = failure_stderr(ctx(&unestablished).args([
        "import",
        "--provider",
        "firebender",
        "--format=json",
    ]));
    assert!(
        stderr.contains("has no official automatic history location established")
            || stderr.contains("no official automatic Firebender history location is established"),
        "{stderr}"
    );
    assert!(
        stderr.contains("ctx import --provider firebender --path <path>"),
        "{stderr}"
    );
}

#[test]
fn unsupported_discovery_is_human_only_and_provider_filtered_import_does_not_dispatch() {
    let temp = tempdir();
    let mux_root = temp.path().join("custom-mux");
    let sessions = mux_root.join("sessions/session-id");
    fs::create_dir_all(&sessions).unwrap();
    let archive = sessions.join("chat-archive.jsonl");
    fs::write(&archive, b"not legacy mux session JSONL\n").unwrap();

    let stdout = success_stdout(ctx(&temp).env("MUX_ROOT", &mux_root).args([
        "sources",
        "--provider",
        "mux",
    ]));
    let concise_sessions = Path::new("~").join("custom-mux/sessions");
    assert!(
        stdout.contains(&concise_sessions.display().to_string()),
        "{stdout}"
    );
    assert!(!stdout.contains(temp.path().to_str().unwrap()), "{stdout}");
    assert!(stdout.contains("unsupported"), "{stdout}");
    assert!(stdout.contains("Mux chat-archive.jsonl"), "{stdout}");
    assert!(
        stdout.contains("ctx import --provider mux --path <path>"),
        "{stdout}"
    );

    let json = json_output(ctx(&temp).env("MUX_ROOT", &mux_root).args([
        "sources",
        "--provider",
        "mux",
        "--format=json",
    ]));
    assert_eq!(json["schema_version"], 1);
    assert_eq!(
        object_keys(&json),
        BTreeSet::from([
            "hidden_missing_sources",
            "issues",
            "issues_truncated",
            "schema_version",
            "scope",
            "sources",
        ])
    );
    assert_eq!(json["issues_truncated"], false);
    let source = json["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["status"] == "unsupported")
        .unwrap();
    assert_eq!(source["import_support"], "unsupported");
    assert_eq!(source["native_import"], false);
    assert_eq!(source["importable"], false);

    let stderr = failure_stderr(ctx(&temp).env("MUX_ROOT", &mux_root).args([
        "import",
        "--provider",
        "mux",
        "--format=json",
    ]));
    assert!(
        stderr.contains("detected unsupported history at"),
        "{stderr}"
    );
    assert!(
        stderr.contains(mux_root.join("sessions").to_str().unwrap()),
        "{stderr}"
    );
    assert!(
        stderr.contains("current ctx cannot import that path"),
        "{stderr}"
    );
    assert!(
        stderr.contains("ctx import --provider mux --path <path>"),
        "{stderr}"
    );
}

#[test]
fn explicit_manual_paths_still_import_and_current_unsupported_shapes_stop_at_admission() {
    let manual = tempdir();
    let manual_state = manual.path().join("state");
    fs::create_dir_all(&manual_state).unwrap();
    let _daemon =
        start_source_refresh_daemon(&manual, &data_root(&manual), manual.path(), &manual_state);
    let query = "factory-manual-custom-root-oracle";
    let factory = write_native_factory_droid_fixture(&manual, query);
    let imported = json_output(ctx(&manual).args([
        "import",
        "--provider",
        "factory-ai-droid",
        "--path",
        &factory,
        "--no-daemon",
        "--progress",
        "none",
        "--format=json",
    ]));
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert!(imported["totals"]["current_source_count"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    let search = json_output(ctx(&manual).args([
        "search",
        query,
        "--provider",
        "factory-ai-droid",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "factory_ai_droid", query, 1, "message");

    let unsupported = tempdir();
    let codex = unsupported.path().join("rollout.jsonl.zst");
    fs::write(&codex, b"compressed").unwrap();
    let kiro = unsupported.path().join("sessions");
    let kiro_cli = kiro.join("cli");
    fs::create_dir_all(&kiro_cli).unwrap();
    fs::write(kiro_cli.join("session-id.json"), b"{}").unwrap();
    fs::write(kiro_cli.join("session-id.jsonl"), b"{}\n").unwrap();
    let qoder = unsupported.path().join("projects/bucket/session.jsonl");
    fs::create_dir_all(qoder.parent().unwrap()).unwrap();
    fs::write(&qoder, b"{}\n").unwrap();
    let openclaw = unsupported.path().join("openclaw-agent.sqlite");
    fs::write(&openclaw, b"sqlite").unwrap();
    let openhands = unsupported.path().join("conversation/events/event-1.json");
    fs::create_dir_all(openhands.parent().unwrap()).unwrap();
    fs::write(&openhands, b"{}").unwrap();
    let mux = unsupported.path().join("chat-archive.jsonl");
    fs::write(&mux, b"{}\n").unwrap();
    let cline = unsupported.path().join("sessions.index.json");
    fs::write(&cline, b"{}").unwrap();

    for (provider, path, reason) in [
        ("codex", codex, "Codex compressed .jsonl.zst"),
        ("kiro-cli", kiro, "Kiro ACP/v3"),
        ("qoder", qoder, "Qoder direct SDK JSONL"),
        ("openclaw", openclaw, "OpenClaw openclaw-agent.sqlite"),
        ("openhands", openhands, "OpenHands CLI events"),
        ("mux", mux, "Mux chat-archive.jsonl"),
        ("cline", cline, "current Cline SDK"),
    ] {
        let stderr = failure_stderr(ctx(&unsupported).args([
            "import",
            "--provider",
            provider,
            "--path",
            path.to_str().unwrap(),
            "--no-daemon",
            "--progress",
            "none",
        ]));
        assert!(stderr.contains("is not importable"), "{stderr}");
        assert!(stderr.contains(reason), "{stderr}");
    }
}

#[test]
fn unsupported_reports_do_not_enter_setup_import_all_search_refresh_or_status() {
    let temp = tempdir();
    let state = temp.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let _daemon = start_source_refresh_daemon(&temp, &data_root(&temp), temp.path(), &state);
    let mux_root = temp.path().join("unsupported-mux");
    let sessions = mux_root.join("sessions/session-id");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("chat-archive.jsonl"), b"{}\n").unwrap();

    let setup = json_output(ctx(&temp).env("MUX_ROOT", &mux_root).args([
        "setup",
        "--wait",
        "--progress",
        "none",
        "--format=json",
    ]));
    assert!(
        setup["import"].is_null() || setup["import"]["totals"]["imported_sources"] == 0,
        "{setup:#}"
    );
    assert!(
        setup["import"].is_null() || setup["import"]["totals"]["failed_sources"] == 0,
        "{setup:#}"
    );

    let baseline_status = json_output(ctx(&temp).args(["status", "--format=json"]));
    let discovery_status = json_output(
        ctx(&temp)
            .env("MUX_ROOT", &mux_root)
            .args(["status", "--format=json"]),
    );
    assert_eq!(
        object_keys(&discovery_status),
        object_keys(&baseline_status)
    );
    assert_eq!(
        discovery_status["schema_version"],
        baseline_status["schema_version"]
    );
    for field in ["indexed_events", "indexed_sessions", "indexed_sources"] {
        assert_eq!(discovery_status[field], baseline_status[field], "{field}");
    }
    assert_eq!(
        discovery_status["daemon"]["status"],
        baseline_status["daemon"]["status"]
    );

    let import_all = success_stdout(ctx(&temp).env("MUX_ROOT", &mux_root).args([
        "import",
        "--all",
        "--progress",
        "none",
    ]));
    assert!(
        import_all.contains("No source changes were found."),
        "{import_all}"
    );
    assert!(import_all.contains("Searchable events"), "{import_all}");
    assert!(!import_all.contains("generation"), "{import_all}");

    ctx(&temp).args(["daemon", "disable"]).assert().success();
    let search = json_output(ctx(&temp).env("MUX_ROOT", &mux_root).args([
        "search",
        "unsupported-mux-should-not-dispatch",
        "--provider",
        "mux",
        "--refresh",
        "background",
        "--format=json",
    ]));
    assert_eq!(search["freshness"]["status"], "daemon_unavailable");
    assert_eq!(search["freshness"]["source_count"], 0);
    assert!(search["results"].as_array().unwrap().is_empty());
}
