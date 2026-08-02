use super::*;

#[test]
fn warp_cli_imports_default_sqlite() {
    let temp = tempdir();
    install_default_warp_fixture(&temp);
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "warp",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_authoritative_provider_publication(&imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert_eq!(provider_core_counts(&data_root(&temp), "warp"), (1, 3));

    let search = json_output(ctx(&temp).args([
        "search",
        "Warp sqlite oracle answer",
        "--provider",
        "warp",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "warp", "Warp sqlite oracle answer", 1, "message");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "warp",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_authoritative_provider_publication(&second);
    assert_eq!(second["totals"]["current_rejected_records"], 0);
}

#[test]
fn warp_native_default_discovery_auto_imports_for_search() {
    let temp = tempdir();
    install_default_warp_fixture(&temp);

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "warp")
        .unwrap_or_else(|| panic!("missing Warp source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "warp_sqlite");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["native_import"], true);
    assert_eq!(source["importable"], true);

    let search = json_output(ctx(&temp).args([
        "search",
        "Warp sqlite oracle answer",
        "--provider",
        "warp",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_eq!(search["freshness"]["mode"], "wait");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert!(
        search["retrieval"]["indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "{search:#}"
    );
    assert_search_provider_oracle(&search, "warp", "Warp sqlite oracle answer", 1, "message");
}

#[test]
fn warp_native_default_discovery_is_included_in_import_all() {
    let temp = tempdir();
    install_default_warp_fixture(&temp);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_authoritative_provider_publication(&imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert_eq!(provider_core_counts(&data_root(&temp), "warp"), (1, 3));

    let search = json_output(ctx(&temp).args([
        "search",
        "Warp sqlite oracle answer",
        "--provider",
        "warp",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "warp", "Warp sqlite oracle answer", 1, "message");
}

#[test]
fn lingma_cli_default_source_imports_home_local_db() {
    let temp = tempdir();
    let query = "lingma-default-import-oracle";
    install_default_lingma_fixture(&temp, query);

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "lingma")
        .unwrap_or_else(|| panic!("missing Lingma source in {sources:#}"));
    assert_eq!(source["source_format"], "lingma_sqlite");
    assert_eq!(source["status"], "available");
    assert_eq!(source["importable"], true);

    let imported =
        json_output(ctx(&temp).args(["import", "--provider", "lingma", "--format=json"]));
    assert_authoritative_provider_publication(&imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert_eq!(provider_core_counts(&data_root(&temp), "lingma"), (1, 2));

    let search =
        json_output(ctx(&temp).args(["search", query, "--provider", "lingma", "--format=json"]));
    assert_search_provider_oracle(&search, "lingma", query, 1, "message");

    let alias_search =
        json_output(ctx(&temp).args(["search", query, "--provider", "qoder-cn", "--format=json"]));
    assert_search_provider_oracle(&alias_search, "lingma", query, 1, "message");

    let second = json_output(ctx(&temp).args(["import", "--provider", "lingma", "--format=json"]));
    assert_authoritative_provider_publication(&second);
    assert_eq!(second["totals"]["current_rejected_records"], 0);
}

#[test]
fn tabnine_cli_imports_default_agent_home_searches_and_reimports() {
    let temp = tempdir();
    let fixture = PathBuf::from(provider_history_fixture("tabnine-cli/.tabnine/agent"));
    copy_dir_all(&fixture, &temp.path().join(".tabnine/agent"));

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "tabnine",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_authoritative_provider_publication(&imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert_eq!(provider_core_counts(&data_root(&temp), "tabnine"), (2, 7));

    let search = json_output(ctx(&temp).args([
        "search",
        "tabnine jsonl oracle answer",
        "--provider",
        "tabnine",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(
        &search,
        "tabnine",
        "tabnine jsonl oracle answer",
        1,
        "message",
    );

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "tabnine",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_authoritative_provider_publication(&second);
    assert_eq!(second["totals"]["current_rejected_records"], 0);
}

#[test]
fn deepagents_cli_sources_import_search_and_reimport_with_aliases() {
    let temp = tempdir();
    let default_db = temp.path().join(".deepagents/.state/sessions.db");
    fs::create_dir_all(default_db.parent().unwrap()).unwrap();
    fs::copy(
        provider_history_fixture("deepagents/v1/sessions.db"),
        &default_db,
    )
    .unwrap();
    let _daemon = start_isolated_provider_daemon(&temp);

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "deepagents")
        .unwrap_or_else(|| panic!("missing Deep Agents source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "deepagents_sessions_sqlite");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["importable"], true);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "deep-agents",
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_authoritative_provider_publication(&imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert_eq!(
        provider_core_counts(&data_root(&temp), "deepagents"),
        (1, 3)
    );

    let search = json_output(ctx(&temp).args([
        "search",
        "deepagents fixture oracle",
        "--provider",
        "dcode",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search(&search, "deepagents", "deepagents fixture oracle");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "deepagents",
        "--no-daemon",
        "--format=json",
    ]));
    assert_authoritative_provider_publication(&second);
    assert_eq!(second["totals"]["current_rejected_records"], 0);
    assert_eq!(provider_core_counts(&data_root(&temp), "deepagents").1, 3);
}

#[test]
fn sqlite_cli_imports_crush_goose_zed_kiro_and_forgecode_and_searches() {
    for (cli_provider, stored_provider, source_format, fixture, query, sessions, events) in [
        (
            "zed",
            "zed",
            "zed_threads_sqlite",
            "zed/v1/threads.db",
            "zed sqlite oracle",
            2,
            5,
        ),
        (
            "crush",
            "crush",
            "crush_sqlite",
            "crush/v1/crush.db",
            "crush sqlite search oracle request",
            2,
            4,
        ),
        (
            "goose",
            "goose",
            "goose_sessions_sqlite",
            "goose/v14/sessions.db",
            "goose sqlite search oracle",
            1,
            3,
        ),
        (
            "kiro-cli",
            "kiro_cli",
            "kiro_cli_sqlite",
            "kiro-cli/v2/data.sqlite3",
            "kiro oracle",
            1,
            3,
        ),
        (
            "forgecode",
            "forgecode",
            "forgecode_sqlite",
            "forgecode/v1/forge.db",
            "forgecode oracle",
            1,
            3,
        ),
    ] {
        let temp = tempdir();
        let fixture = PathBuf::from(provider_history_fixture(fixture));
        let explicit = if stored_provider == "crush" {
            let default = temp.path().join(".crush/crush.db");
            fs::create_dir_all(default.parent().unwrap()).unwrap();
            fs::copy(&fixture, &default).unwrap();
            None
        } else if stored_provider == "zed" {
            let default = temp.path().join(".local/share/zed/threads/threads.db");
            fs::create_dir_all(default.parent().unwrap()).unwrap();
            fs::copy(&fixture, &default).unwrap();
            None
        } else {
            Some(fixture)
        };
        let _daemon = start_isolated_provider_daemon(&temp);

        let mut first_command = ctx(&temp);
        if stored_provider == "crush" {
            first_command.current_dir(temp.path());
        }
        first_command.args(["import", "--provider", cli_provider]);
        if let Some(explicit) = explicit.as_ref() {
            first_command.args(["--path", explicit.to_str().unwrap()]);
        }
        first_command.args(["--no-daemon", "--format=json", "--progress", "none"]);
        let imported = json_output(&mut first_command);
        if explicit.is_some() {
            assert_explicit_source_publication(&imported, stored_provider, source_format);
            assert_eq!(imported["totals"]["current_rejected_records"], 0);
        } else {
            assert_authoritative_provider_publication(&imported);
            assert_eq!(imported["totals"]["current_rejected_records"], 0);
        }
        assert_eq!(
            provider_core_counts(&data_root(&temp), stored_provider),
            (sessions, events),
            "unexpected {stored_provider} Core counts"
        );
        if stored_provider == "crush" {
            let records = provider_core_records(&data_root(&temp), "crush");
            assert_eq!(
                records
                    .iter()
                    .filter(|record| {
                        record.provider_session_id.as_deref() == Some("crush-child")
                            && record.event_type == "command_output"
                    })
                    .count(),
                1,
                "the successful child-only shell command output must remain in self-contained Core"
            );
            assert_eq!(
                records
                    .iter()
                    .filter(|record| {
                        record.provider_session_id.as_deref() == Some("crush-root")
                    })
                    .count(),
                3,
                "the fourth Crush event must not duplicate a root message"
            );
        }

        let mut search_command = ctx(&temp);
        if stored_provider == "crush" {
            search_command.current_dir(temp.path());
        }
        let search = json_output(search_command.args([
            "search",
            query,
            "--provider",
            cli_provider,
            "--refresh",
            "off",
            "--format=json",
        ]));
        assert_source_backed_search(&search, stored_provider, query);

        let result = &search["results"].as_array().unwrap()[0];
        let ctx_event_id = result["ctx_event_id"].as_str().unwrap();
        let shown = json_output(ctx(&temp).args(["show", "event", ctx_event_id, "--format=json"]));
        assert_eq!(shown["event"]["provider"], stored_provider);
        assert_eq!(shown["event"]["source_format"], source_format);
        assert!(shown["event"].get("source_path").is_none());

        let mut second_command = ctx(&temp);
        if stored_provider == "crush" {
            second_command.current_dir(temp.path());
        }
        second_command.args(["import", "--provider", cli_provider]);
        if let Some(explicit) = explicit.as_ref() {
            second_command.args(["--path", explicit.to_str().unwrap()]);
        }
        second_command.args(["--no-daemon", "--format=json", "--progress", "none"]);
        let second = json_output(&mut second_command);
        if explicit.is_some() {
            assert_explicit_source_publication(&second, stored_provider, source_format);
            assert_eq!(second["totals"]["current_rejected_records"], 0);
            assert_noop_publication(&second);
            assert_eq!(second["sources"][0]["catalog_changed"], false, "{second:#}");
        } else {
            assert_authoritative_provider_publication(&second);
            assert_eq!(second["totals"]["current_rejected_records"], 0);
            assert_noop_publication(&second);
        }
    }
}
