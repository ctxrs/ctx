use super::{ctx, support::*};

fn source_entries(report: &Value) -> &[Value] {
    report["sources"].as_array().unwrap()
}

fn source_entry<'a>(report: &'a Value, provider: &str, source_format: Option<&str>) -> &'a Value {
    source_entries(report)
        .iter()
        .find(|source| {
            source["provider"] == provider
                && source_format.is_none_or(|format| source["source_format"] == format)
        })
        .unwrap_or_else(|| panic!("missing {provider} source in {report:#}"))
}

fn write_maximum_missing_openclaw_roots(temp: &TempDir) -> Vec<String> {
    fs::create_dir_all(data_root(temp)).unwrap();
    let mut config = String::new();
    for index in (0..64).rev() {
        let name = format!("openclaw-{index:02}");
        let path = temp.path().join(format!("missing-{name}"));
        config.push_str(&format!(
            "[sources.roots.{name}]\nprovider = \"openclaw\"\npath = {}\n\n",
            json!(path),
        ));
    }
    fs::write(data_root(temp).join("config.toml"), config).unwrap();
    (0..64)
        .map(|index| format!("openclaw-{index:02}"))
        .collect()
}

#[test]
fn setup_skips_empty_codex_session_tree() {
    let temp = tempdir();
    fs::create_dir_all(temp.path().join(".codex").join("sessions")).unwrap();

    let setup =
        json_output(ctx(&temp).args(["setup", "--wait", "--format=json", "--progress", "none"]));
    assert!(
        setup["catalog"]["cataloged_sessions"].is_null(),
        "{setup:#}"
    );
    assert!(setup["catalog"]["source_files"].is_null(), "{setup:#}");
    let current = &setup["refresh_request"]["receipt"]["current"];
    assert_eq!(current["current_source_count"], 0, "{setup:#}");

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
    let codex_sessions = source_entry(&sources, "codex", Some("codex_session_jsonl_tree"));
    assert_eq!(codex_sessions["status"], "empty");
    assert_eq!(codex_sessions["importable"], false);
}

#[test]
fn sources_default_hides_unsupported_missing_locations() {
    let temp = tempdir();

    let sources = json_output(ctx(&temp).args(["--color=always", "sources", "--format=json"]));
    assert_eq!(sources["scope"], "default");
    assert!(sources["hidden_missing_sources"].as_u64().unwrap() > 0);
    let visible = source_entries(&sources);
    for provider in ["codex", "claude", "cursor", "pi", "opencode", "copilot_cli"] {
        assert!(visible.iter().any(|source| source["provider"] == provider));
    }

    let text = success_stdout(ctx(&temp).arg("sources"));
    assert!(text.contains("missing provider locations are hidden"));
    assert!(text.contains("ctx sources --all"));

    let all_sources = json_output(ctx(&temp).args(["sources", "--format=json", "--all"]));
    assert_eq!(all_sources["scope"], "all");
    assert_eq!(all_sources["hidden_missing_sources"], 0);
    let all = source_entries(&all_sources);
    assert!(all.len() > visible.len());
}

#[test]
fn configured_missing_roots_remain_listed_without_exposing_automatic_missing_routes() {
    let temp = tempdir();
    let missing_openclaw = temp.path().join("missing-openclaw-state");
    let missing_goose = temp.path().join("missing-goose-sessions.db");
    fs::create_dir_all(data_root(&temp)).unwrap();
    fs::write(
        data_root(&temp).join("config.toml"),
        format!(
            "[sources.roots.personal-openclaw]\nprovider = \"openclaw\"\npath = {:?}\ngroup = \"personal\"\n\n[sources.roots.work-goose]\nprovider = \"goose\"\npath = {:?}\ngroup = \"work\"\n",
            missing_openclaw.display().to_string(),
            missing_goose.display().to_string(),
        ),
    )
    .unwrap();

    let default = json_output(ctx(&temp).args(["sources", "--format=json"]));
    assert_eq!(default["scope"], "default");
    let goose = source_entries(&default)
        .iter()
        .find(|source| source["path"] == missing_goose.to_str().unwrap())
        .unwrap_or_else(|| panic!("configured Goose source missing from {default:#}"));
    assert_eq!(goose["provider"], "goose");
    assert_eq!(goose["status"], "missing");
    assert_eq!(
        goose["selection"],
        json!({"kind": "configured", "root": "work-goose", "group": "work"}),
    );
    assert!(source_entries(&default).iter().all(|source| {
        source["provider"] != "goose" || source["path"] == missing_goose.to_str().unwrap()
    }));
    let openclaw = default["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "configured_root_missing")
        .unwrap_or_else(|| panic!("route-less OpenClaw root missing from {default:#}"));
    assert_eq!(openclaw["provider"], "openclaw");
    assert_eq!(openclaw["path"], missing_openclaw.to_str().unwrap());
    assert_eq!(
        openclaw["configured_root"],
        json!({
            "name": "personal-openclaw",
            "path": missing_openclaw.to_str().unwrap(),
            "group": "personal",
        }),
    );

    let all = json_output(ctx(&temp).args(["sources", "--all", "--format=json"]));
    assert_eq!(all["scope"], "all");
    assert!(source_entries(&all)
        .iter()
        .any(|source| source["path"] == missing_goose.to_str().unwrap()));
    assert!(all["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue["code"] == "configured_root_missing"
            && issue["configured_root"]["name"] == "personal-openclaw"));

    let human = success_stdout(ctx(&temp).arg("sources"));
    assert!(human.contains("personal-openclaw (personal)"), "{human}");
    assert!(
        human.contains("configured history root is missing"),
        "{human}"
    );
    assert!(
        human.contains("--root <replacement-path> --replace"),
        "{human}"
    );
}

#[test]
fn maximum_missing_roots_precede_automatic_issues_without_exceeding_json_bounds() {
    let temp = tempdir();
    let expected_names = write_maximum_missing_openclaw_roots(&temp);

    let sources = json_output(
        ctx(&temp)
            .env("CLAUDE_CONFIG_DIR", "relative-account")
            .args(["sources", "--format=json"]),
    );

    assert_eq!(sources["issues_truncated"], true, "{sources:#}");
    let issues = sources["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 64, "{sources:#}");
    let actual_names = issues
        .iter()
        .map(|issue| {
            assert_eq!(issue["code"], "configured_root_missing", "{issue:#}");
            let root = issue
                .get("configured_root")
                .and_then(Value::as_object)
                .unwrap_or_else(|| panic!("missing configured_root object in {issue:#}"));
            assert!(root["group"].is_null(), "{issue:#}");
            root["name"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(actual_names, expected_names);

    let human = success_stdout(
        ctx(&temp)
            .env("CLAUDE_CONFIG_DIR", "relative-account")
            .arg("sources"),
    );
    assert_eq!(
        human.matches("configured history root is missing").count(),
        64,
        "{human}"
    );
    assert_eq!(human.matches("--replace").count(), 64, "{human}");
}

#[test]
fn request_scoped_import_paths_do_not_enter_the_automatic_source_inventory() {
    let temp = tempdir();
    let explicit_root = temp.path().join("external-codex-history");
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &explicit_root,
    );
    let explicit_path = explicit_root.to_str().unwrap();

    let before = json_output(ctx(&temp).args(["sources", "--provider", "codex", "--format=json"]));
    assert!(source_entries(&before)
        .iter()
        .all(|source| source["path"] != explicit_path));

    for _ in 0..2 {
        json_output(ctx(&temp).args([
            "import",
            "--provider",
            "codex",
            "--path",
            explicit_path,
            "--format=json",
            "--progress",
            "none",
        ]));
    }

    let after = json_output(ctx(&temp).args(["sources", "--provider", "codex", "--format=json"]));
    let matches = source_entries(&after)
        .iter()
        .filter(|source| source["path"] == explicit_path)
        .collect::<Vec<_>>();
    assert!(matches.is_empty(), "request overlay leaked into {after:#}");

    let human = success_stdout(ctx(&temp).args(["sources", "--provider", "codex"]));
    let concise_path = Path::new("~").join("external-codex-history");
    assert!(
        !human.contains(&concise_path.display().to_string()),
        "{human}"
    );
    assert!(!human.contains(explicit_path), "{human}");
    assert!(!human.contains(temp.path().to_str().unwrap()), "{human}");
}

#[test]
fn sources_provider_filter_rejects_unsupported_providers() {
    let temp = tempdir();

    ctx(&temp)
        .args([
            "sources",
            "--provider",
            "not-a-real-provider",
            "--format=json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown provider"));
}

#[test]
fn sources_json_reports_typed_discovery_issues_additively() {
    let temp = tempdir();

    let sources = json_output(
        ctx(&temp)
            .env("CLAUDE_CONFIG_DIR", "relative-account")
            .args(["sources", "--provider", "claude", "--format=json"]),
    );

    assert_eq!(sources["schema_version"], 1);
    assert_eq!(sources["scope"], "all");
    assert_eq!(sources["hidden_missing_sources"], 0);
    assert!(source_entries(&sources).is_empty());
    assert_eq!(sources["issues_truncated"], false);
    let issues = sources["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0]["provider"], "claude");
    assert_eq!(issues[0]["path"], "relative-account");
    assert_eq!(issues[0]["code"], "selector_unreconstructible");
    assert_eq!(
        issues[0]["message"],
        "the selected provider root cannot be reconstructed safely; use an exact --path"
    );
    assert_eq!(issues[0]["message_truncated"], false);
}

#[test]
fn sources_json_and_human_output_expose_configured_root_conflict_repairs() {
    let temp = tempdir();
    let legacy_root = temp.path().join(".openhands");
    let configured_root = legacy_root.join("conversations");
    fs::create_dir_all(configured_root.join("conversation/events")).unwrap();
    fs::write(
        configured_root.join("conversation/events/event-00001.json"),
        "{}",
    )
    .unwrap();
    fs::create_dir_all(legacy_root.join("v1_conversations/legacy")).unwrap();
    fs::write(legacy_root.join("v1_conversations/legacy/event.json"), "{}").unwrap();
    fs::create_dir_all(data_root(&temp)).unwrap();
    fs::write(
        data_root(&temp).join("config.toml"),
        format!(
            "[sources.roots.work]\nprovider = \"openhands\"\npath = {:?}\nkind = \"current-conversations\"\n",
            configured_root.display().to_string(),
        ),
    )
    .unwrap();

    let sources =
        json_output(ctx(&temp).args(["sources", "--provider", "openhands", "--format=json"]));
    let issues = sources["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1, "{sources:#}");
    assert_eq!(issues[0]["provider"], "openhands");
    assert_eq!(issues[0]["path"], configured_root.to_str().unwrap());
    assert_eq!(issues[0]["code"], "configured_root_conflict");
    assert_eq!(issues[0]["conflict_kind"], "automatic_configured");
    assert_eq!(
        issues[0]["configured_roots"],
        json!([{
            "name": "work",
            "path": configured_root.to_str().unwrap(),
        }]),
    );

    let human = success_stdout(ctx(&temp).args(["sources", "--provider", "openhands"]));
    assert!(human.contains("automatic/configured"), "{human}");
    assert!(human.contains("ctx sources remove work"), "{human}");
    assert!(human.contains("automatic=false"), "{human}");
    assert!(!human.contains("ctx import"), "{human}");
}

#[test]
fn sources_lists_supported_personal_agent_provider_defaults() {
    let temp = tempdir();
    install_default_hermes_fixture(&temp, "hermes-sources-oracle");
    install_default_kilo_fixture(&temp, "kilo-sources-oracle");
    install_default_kiro_fixture(&temp, "kiro-sources-oracle");
    install_default_astrbot_fixture(&temp, "astrbot-sources-oracle");
    install_default_continue_fixture(&temp, "continue-sources-oracle");
    install_default_forgecode_fixture(&temp, "forgecode-sources-oracle");
    install_default_mistral_vibe_fixture(&temp, "mistral-vibe-sources-oracle");
    install_default_mux_fixture(&temp, "mux-sources-oracle");
    install_default_lingma_fixture(&temp, "lingma-sources-oracle");
    install_default_qoder_fixture(&temp, "qoder-sources-oracle");
    install_default_auggie_fixture(&temp, "auggie-sources-oracle");
    install_default_junie_fixture(&temp, "junie-sources-oracle");
    install_default_warp_fixture(&temp);

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
    for (provider, source_format) in [
        ("hermes", "hermes_state_sqlite"),
        ("kilo", "kilo_sqlite"),
        ("kiro_cli", "kiro_cli_sqlite"),
        ("astrbot", "astrbot_data_v4_sqlite"),
        ("continue", "continue_cli_sessions_json"),
        ("forgecode", "forgecode_sqlite"),
        ("mistral_vibe", "mistral_vibe_session_jsonl_tree"),
        ("mux", "mux_session_jsonl_tree"),
        ("lingma", "lingma_sqlite"),
        ("qoder", "qoder_transcript_jsonl_tree"),
        ("auggie", "auggie_session_json"),
        ("junie", "junie_session_events_jsonl_tree"),
        ("warp", "warp_sqlite"),
    ] {
        let source = source_entry(&sources, provider, Some(source_format));
        assert_eq!(source["status"], "available");
        assert_eq!(source["import_support"], "native");
        assert_eq!(source["native_import"], true);
        assert_eq!(source["importable"], true);
        assert!(source["unsupported_reason"].is_null());
    }
}

#[test]
fn sources_reports_adjacent_current_and_legacy_routes_without_unsupported_duplicates() {
    let temp = tempdir();

    let openclaw_sessions = temp.path().join(".openclaw/agents/personal-agent/sessions");
    fs::create_dir_all(&openclaw_sessions).unwrap();
    fs::write(
        temp.path().join(".openclaw/openclaw.json"),
        r#"{"agents":{"list":[{"id":"personal-agent"}]}}"#,
    )
    .unwrap();
    fs::write(openclaw_sessions.join("session.jsonl"), "{}\n").unwrap();

    install_default_kiro_fixture(&temp, "kiro-coexistence-oracle");
    fs::create_dir_all(temp.path().join(".kiro/sessions")).unwrap();

    let qoder_root = temp.path().join(".qoder/projects");
    let qoder_project = qoder_root.join("project");
    fs::create_dir_all(&qoder_project).unwrap();
    fs::write(qoder_project.join("direct-session.jsonl"), "{}\n").unwrap();

    let mux_root = temp.path().join(".mux/sessions");
    let mux_workspace = mux_root.join("workspace");
    fs::create_dir_all(&mux_workspace).unwrap();
    fs::write(mux_workspace.join("chat-archive.jsonl"), "{}\n").unwrap();

    let openhands_conversations = temp.path().join("openhands-conversations");
    let openhands_events = openhands_conversations.join("conversation/events");
    fs::create_dir_all(&openhands_events).unwrap();
    fs::write(openhands_events.join("event-1.json"), "{}").unwrap();

    let sources = json_output(
        ctx(&temp)
            .env("OPENHANDS_CONVERSATIONS_DIR", &openhands_conversations)
            .args(["sources", "--format=json"]),
    );

    for (provider, format, path) in [
        (
            "openclaw",
            "openclaw_session_jsonl_tree",
            openclaw_sessions.as_path(),
        ),
        ("qoder", "qoder_transcript_jsonl_tree", qoder_root.as_path()),
        ("mux", "mux_session_jsonl_tree", mux_root.as_path()),
        (
            "openhands",
            "openhands_cli_file_events",
            openhands_conversations.as_path(),
        ),
    ] {
        let matches = source_entries(&sources)
            .iter()
            .filter(|source| source["provider"] == provider)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{provider}: {sources:#}");
        assert_eq!(matches[0]["source_format"], format);
        assert_eq!(matches[0]["path"], path.to_str().unwrap());
        assert_eq!(matches[0]["status"], "available");
        assert_eq!(matches[0]["importable"], true);
        assert!(matches[0]["unsupported_reason"].is_null());
    }

    let kiro = source_entries(&sources)
        .iter()
        .filter(|source| source["provider"] == "kiro_cli")
        .collect::<Vec<_>>();
    assert_eq!(kiro.len(), 2, "{sources:#}");
    assert!(kiro.iter().any(|source| {
        source["source_format"] == "kiro_cli_sqlite"
            && source["status"] == "available"
            && source["importable"] == true
    }));
    assert!(kiro.iter().any(|source| {
        source["status"] == "unsupported"
            && source["path"] == temp.path().join(".kiro/sessions").to_str().unwrap()
    }));
}

#[test]
fn hermes_sources_and_imports_are_native_and_read_only() {
    let temp = tempdir();
    let query = "hermes-automatic-import-oracle";
    install_default_hermes_fixture(&temp, query);
    let database = temp.path().join(".hermes/state.db");
    let original = fs::read(&database).unwrap();

    let sources =
        json_output(ctx(&temp).args(["sources", "--provider", "hermes", "--format=json"]));
    let [source] = source_entries(&sources) else {
        panic!("one Hermes source expected: {sources:#}");
    };
    assert_eq!(source["status"], "available");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["native_import"], true);
    assert_eq!(source["importable"], true);
    assert!(source["unsupported_reason"].is_null());
    assert_eq!(fs::read(&database).unwrap(), original);

    let human = success_stdout(ctx(&temp).args(["sources", "--provider", "hermes"]));
    assert!(!human.contains("cannot be imported"), "{human}");
    assert_eq!(fs::read(&database).unwrap(), original);

    let automatic = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "hermes",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_authoritative_provider_publication(&automatic);
    assert!(provider_core_counts(&data_root(&temp), "hermes").1 >= 2);
    assert_eq!(fs::read(&database).unwrap(), original);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "hermes",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "hermes", query, 1, "message");

    let explicit_temp = tempdir();
    let explicit_query = "hermes-explicit-import-oracle";
    let explicit_path = PathBuf::from(write_native_hermes_fixture(&explicit_temp, explicit_query));
    let explicit_original = fs::read(&explicit_path).unwrap();
    let explicit = json_output(ctx(&explicit_temp).args([
        "import",
        "--provider",
        "hermes",
        "--path",
        explicit_path.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_explicit_source_publication(&explicit, "hermes", "hermes_state_sqlite");
    assert!(provider_core_counts(&data_root(&explicit_temp), "hermes").1 >= 2);
    assert_eq!(fs::read(&explicit_path).unwrap(), explicit_original);
}

#[test]
fn sources_uses_exact_cwd_and_ignores_shelley_db_env_override() {
    let temp = tempdir();
    let fixture = PathBuf::from(write_native_shelley_fixture(&temp, "shelley-cwd-oracle"));
    let cwd_db = temp.path().join("shelley.db");
    fs::copy(fixture, &cwd_db).unwrap();
    let ignored_env_db = temp.path().join("custom-shelley.db");
    fs::write(&ignored_env_db, b"sqlite fixture marker").unwrap();

    let sources = json_output(
        ctx(&temp)
            .current_dir(temp.path())
            .env("SHELLEY_DB", &ignored_env_db)
            .args(["sources", "--format=json"]),
    );
    let source = source_entries(&sources)
        .iter()
        .find(|source| {
            source["provider"] == "shelley" && source["path"] == cwd_db.to_str().unwrap()
        })
        .unwrap_or_else(|| panic!("missing Shelley source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert!(source_entries(&sources)
        .iter()
        .all(|source| source["path"] != ignored_env_db.to_str().unwrap()));
}

#[test]
fn sources_falls_back_to_userprofile_when_home_unset() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );

    let sources = json_output(
        ctx(&temp)
            .env_remove("HOME")
            .env("USERPROFILE", temp.path())
            .args(["sources", "--format=json"]),
    );
    let codex = source_entry(&sources, "codex", Some("codex_session_jsonl_tree"));
    assert_eq!(codex["status"], "available");
    assert!(Path::new(codex["path"].as_str().unwrap()).starts_with(temp.path()));
}

#[test]
fn sources_discovers_forgecode_env_and_legacy_db() {
    let temp = tempdir();
    let fixture = PathBuf::from(write_native_forgecode_fixture(
        &temp,
        "forgecode-env-sources-oracle",
    ));
    let env_root = temp.path().join("custom-forge");
    fs::create_dir_all(&env_root).unwrap();
    let env_db = env_root.join(".forge.db");
    fs::copy(&fixture, &env_db).unwrap();

    let sources = json_output(
        ctx(&temp)
            .env("FORGE_CONFIG", &env_root)
            .args(["sources", "--format=json"]),
    );
    let source = source_entry(&sources, "forgecode", None);
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "forgecode_sqlite");
    assert_eq!(source["path"], env_db.to_str().unwrap());

    let legacy_temp = tempdir();
    let legacy_fixture = PathBuf::from(write_native_forgecode_fixture(
        &legacy_temp,
        "forgecode-legacy-sources-oracle",
    ));
    let legacy_root = legacy_temp.path().join("forge");
    fs::create_dir_all(&legacy_root).unwrap();
    let legacy_db = legacy_root.join(".forge.db");
    fs::copy(legacy_fixture, &legacy_db).unwrap();

    let sources = json_output(ctx(&legacy_temp).args(["sources", "--format=json"]));
    let source = source_entry(&sources, "forgecode", None);
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "forgecode_sqlite");
    assert_eq!(source["path"], legacy_db.to_str().unwrap());
}
fn nanoclaw_identity_snapshot(temp: &TempDir) -> (String, String, usize) {
    let records = provider_core_records(&data_root(temp), "nanoclaw");
    assert!(!records.is_empty(), "missing NanoClaw Core records");
    let sources = records
        .iter()
        .map(|record| record.source.identity().as_uuid())
        .collect::<BTreeSet<_>>();
    let sessions = records
        .iter()
        .map(|record| record.session_id.as_uuid())
        .collect::<BTreeSet<_>>();
    assert_eq!(sources.len(), 1, "NanoClaw source identity forked");
    assert_eq!(sessions.len(), 1, "NanoClaw session identity forked");
    (
        sources.into_iter().next().unwrap().to_string(),
        sessions.into_iter().next().unwrap().to_string(),
        records.len(),
    )
}

fn registered_nanoclaw(temp: &TempDir, query: &str) -> (PathBuf, PathBuf) {
    let project = PathBuf::from(write_native_nanoclaw_fixture(temp, query));
    let registered = project
        .parent()
        .unwrap()
        .join(".")
        .join(project.file_name().unwrap());
    write_nanoclaw_systemd_registration(temp, &registered);
    let unrelated_cwd = temp.path().join("unrelated-cwd");
    fs::create_dir_all(&unrelated_cwd).unwrap();
    (project, unrelated_cwd)
}

fn assert_nanoclaw_counts(counts: &Value, sources: u64, documents: u64) {
    assert_eq!(counts["current_source_count"], sources, "{counts:#}");
    assert_eq!(counts["current_indexed_documents"], documents, "{counts:#}");
}

fn assert_nanoclaw_search(temp: &TempDir, query: &str, cwd: Option<&Path>) {
    let mut command = ctx(temp);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let search = json_output(command.args([
        "search",
        query,
        "--provider",
        "nanoclaw",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "nanoclaw", query, 1, "message");
}

fn import_nanoclaw(temp: &TempDir, cwd: Option<&Path>, path: Option<&Path>) -> Value {
    let mut command = ctx(temp);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.arg("import");
    if let Some(path) = path {
        command.args(["--provider", "nanoclaw", "--path", path.to_str().unwrap()]);
    } else {
        command.args(["--all", "--progress", "none"]);
    }
    json_output(command.arg("--format=json"))
}

#[test]
fn nanoclaw_automatic_then_explicit_import_preserves_one_source_session_and_result() {
    let temp = tempdir();
    let query = "nanoclaw-lexical-registration-auto-refresh-oracle";
    let (project, unrelated_cwd) = registered_nanoclaw(&temp, query);

    let mut sources_command = ctx(&temp);
    sources_command.current_dir(&unrelated_cwd);
    let sources = json_output(sources_command.args(["sources", "--format=json"]));
    let nanoclaw = source_entry(&sources, "nanoclaw", None);
    assert_eq!(nanoclaw["status"], "available");
    assert_eq!(nanoclaw["import_support"], "native");
    assert_eq!(nanoclaw["native_import"], true);
    assert_eq!(nanoclaw["importable"], true);
    assert!(nanoclaw["unsupported_reason"].is_null());
    assert_eq!(nanoclaw["path"], project.to_str().unwrap());

    let mut setup_command = ctx(&temp);
    setup_command.current_dir(&unrelated_cwd);
    let imported_generation =
        json_output(setup_command.args(["setup", "--wait", "--format=json", "--progress", "none"]));
    let current = &imported_generation["refresh_request"]["receipt"]["current"];
    assert_nanoclaw_counts(current, 1, 2);
    let automatic_identity = nanoclaw_identity_snapshot(&temp);
    assert_nanoclaw_search(&temp, query, None);

    ctx(&temp).args(["daemon", "enable"]).assert().success();
    let imported = import_nanoclaw(&temp, None, Some(&project));
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert_nanoclaw_counts(&imported["totals"], 1, 2);
    assert_eq!(nanoclaw_identity_snapshot(&temp), automatic_identity);
    assert_nanoclaw_search(&temp, query, None);
}

#[test]
fn nanoclaw_explicit_then_automatic_import_preserves_one_source_session_and_result() {
    let temp = tempdir();
    let query = "nanoclaw-explicit-then-automatic-oracle";
    let (project, unrelated_cwd) = registered_nanoclaw(&temp, query);

    ctx(&temp).args(["daemon", "enable"]).assert().success();
    let explicit = import_nanoclaw(&temp, None, Some(&project));
    assert_nanoclaw_counts(&explicit["totals"], 1, 2);
    let explicit_identity = nanoclaw_identity_snapshot(&temp);

    let automatic = import_nanoclaw(&temp, Some(&unrelated_cwd), None);
    assert_nanoclaw_counts(&automatic["totals"], 1, 2);
    assert_eq!(nanoclaw_identity_snapshot(&temp), explicit_identity);
    assert_nanoclaw_search(&temp, query, None);
}

#[test]
fn nanoclaw_service_registration_import_all_indexes_two_checkouts_without_exact_cwd() {
    let temp = tempdir();
    let first_query = "zephyrcobaltquasar";
    let first_fixture = PathBuf::from(write_native_nanoclaw_fixture(&temp, first_query));
    let first_project = temp.path().join("registered NanoClaw checkout one");
    fs::rename(&first_fixture, &first_project).unwrap();
    write_nanoclaw_systemd_registration(&temp, &first_project);

    let second_query = "marigoldvelvetpulsar";
    let second_fixture = PathBuf::from(write_native_nanoclaw_fixture(&temp, second_query));
    let second_project = temp.path().join("registered NanoClaw checkout two");
    fs::rename(&second_fixture, &second_project).unwrap();
    write_nanoclaw_systemd_registration(&temp, &second_project);
    let unrelated_cwd = temp.path().join("unrelated-cwd");
    fs::create_dir_all(&unrelated_cwd).unwrap();

    let mut sources_command = ctx(&temp);
    sources_command.current_dir(&unrelated_cwd);
    let sources =
        json_output(sources_command.args(["sources", "--provider", "nanoclaw", "--format=json"]));
    let mut nanoclaw_paths = source_entries(&sources)
        .iter()
        .filter(|source| source["provider"] == "nanoclaw")
        .map(|source| {
            assert_eq!(source["status"], "available");
            assert_eq!(source["import_support"], "native");
            source["path"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();
    nanoclaw_paths.sort();
    let mut expected_paths = vec![
        first_project.to_str().unwrap().to_owned(),
        second_project.to_str().unwrap().to_owned(),
    ];
    expected_paths.sort();
    assert_eq!(nanoclaw_paths, expected_paths, "{sources:#}");

    let imported = import_nanoclaw(&temp, Some(&unrelated_cwd), None);
    assert_nanoclaw_counts(&imported["totals"], 2, 4);

    // Import waits for the published receipt. Stop the isolated daemon before
    // searching so no background refresh can race the refresh-off assertion.
    json_output(ctx(&temp).args(["daemon", "disable", "--format=json"]));

    for query in [first_query, second_query] {
        assert_nanoclaw_search(&temp, query, Some(&unrelated_cwd));
    }
}

fn write_nanoclaw_systemd_registration(temp: &TempDir, project: &Path) {
    let slug = nanoclaw_test_sha1_slug(project.to_string_lossy().as_bytes());
    let unit = temp
        .path()
        .join(".config/systemd/user")
        .join(format!("nanoclaw-v2-{slug}.service"));
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(
        unit,
        format!(
            "[Unit]\nDescription=NanoClaw Personal Assistant\nAfter=network.target\n\n[Service]\nType=simple\nExecStart=/usr/bin/node {}/dist/index.js\nWorkingDirectory={}\nRestart=always\nRestartSec=5\nKillMode=process\nEnvironment=HOME={}\nEnvironment=PATH=/usr/local/bin:/usr/bin:/bin:{}/.local/bin\nStandardOutput=append:{}/logs/nanoclaw.log\nStandardError=append:{}/logs/nanoclaw.error.log\n\n[Install]\nWantedBy=default.target",
            project.display(),
            project.display(),
            temp.path().display(),
            temp.path().display(),
            project.display(),
            project.display(),
        ),
    )
    .unwrap();
}

fn nanoclaw_test_sha1_slug(input: &[u8]) -> String {
    let mut message = input.to_vec();
    let bit_len = (message.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    let mut hash = [
        0x6745_2301_u32,
        0xefcd_ab89_u32,
        0x98ba_dcfe_u32,
        0x1032_5476_u32,
        0xc3d2_e1f0_u32,
    ];
    for chunk in message.chunks_exact(64) {
        let mut words = [0_u32; 80];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let start = index * 4;
            *word = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = hash;
        for (index, word) in words.iter().enumerate() {
            let (function, constant) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(function)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        hash[0] = hash[0].wrapping_add(a);
        hash[1] = hash[1].wrapping_add(b);
        hash[2] = hash[2].wrapping_add(c);
        hash[3] = hash[3].wrapping_add(d);
        hash[4] = hash[4].wrapping_add(e);
    }

    let bytes = hash[0].to_be_bytes();
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}
