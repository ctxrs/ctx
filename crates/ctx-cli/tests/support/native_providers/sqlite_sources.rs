use super::*;

#[test]
fn warp_cli_imports_explicit_sqlite() {
    let temp = tempdir();
    let fixture = provider_history_fixture("warp/v1/warp.sqlite");
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "warp",
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["schema_version"], 2);
    assert_eq!(imported["sources"][0]["provider"], "warp");
    assert_eq!(imported["sources"][0]["source_format"], "warp_sqlite");
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 3);

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
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["rejected_records"], 0);
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
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
    assert_eq!(search["freshness"]["totals"]["rejected_records"], 0);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_events"], 3);
    assert_search_provider_oracle(&search, "warp", "Warp sqlite oracle answer", 1, "message");
}

#[test]
fn warp_native_default_discovery_is_included_in_import_all() {
    let temp = tempdir();
    install_default_warp_fixture(&temp);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| {
            source["provider"] == "warp" && source["source_format"] == "warp_sqlite"
        }));
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 3);

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
    assert_eq!(imported["sources"][0]["provider"], "lingma");
    assert_eq!(imported["sources"][0]["source_format"], "lingma_sqlite");
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search =
        json_output(ctx(&temp).args(["search", query, "--provider", "lingma", "--format=json"]));
    assert_search_provider_oracle(&search, "lingma", query, 1, "message");

    let alias_search =
        json_output(ctx(&temp).args(["search", query, "--provider", "qoder-cn", "--format=json"]));
    assert_search_provider_oracle(&alias_search, "lingma", query, 1, "message");

    let second = json_output(ctx(&temp).args(["import", "--provider", "lingma", "--format=json"]));
    assert_eq!(second["totals"]["rejected_records"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
}

#[test]
fn tabnine_cli_imports_explicit_agent_home_searches_and_reimports() {
    let temp = tempdir();
    let fixture = provider_history_fixture("tabnine-cli/.tabnine/agent");

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "tabnine",
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["schema_version"], 2);
    assert_eq!(imported["sources"][0]["provider"], "tabnine");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "tabnine_cli_chat_recording_jsonl"
    );
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 2);
    assert_eq!(imported["totals"]["imported_events"], 6);

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
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["rejected_records"], 0);
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
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
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["sources"][0]["provider"], "deepagents");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "deepagents_sessions_sqlite"
    );
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search = json_output(ctx(&temp).args([
        "search",
        "deepagents fixture oracle",
        "--provider",
        "dcode",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(
        &search,
        "deepagents",
        "deepagents fixture oracle",
        1,
        "message",
    );

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "deepagents",
        "--path",
        default_db.to_str().unwrap(),
        "--format=json",
    ]));
    assert_eq!(second["totals"]["rejected_records"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'deepagents'"
        ),
        2
    );
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
            "crush oracle",
            2,
            3,
        ),
        (
            "goose",
            "goose",
            "goose_sessions_sqlite",
            "goose/v14/sessions.db",
            "goose oracle",
            1,
            2,
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
            2,
        ),
    ] {
        let temp = tempdir();
        let fixture = provider_history_fixture(fixture);

        let imported = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &fixture,
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_eq!(imported["schema_version"], 2);
        assert_eq!(imported["sources"][0]["provider"], stored_provider);
        assert_eq!(imported["sources"][0]["source_format"], source_format);
        assert_eq!(imported["totals"]["rejected_records"], 0);
        assert_eq!(imported["totals"]["imported_sessions"], sessions);
        assert_eq!(imported["totals"]["imported_events"], events);
        if stored_provider == "crush" {
            let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
            assert_eq!(
                sqlite_count(
                    &conn,
                    "SELECT COUNT(*) FROM ctx_sessions \
                     WHERE provider = 'crush' AND provider_session_id = 'crush-child'",
                ),
                1,
                "NativePath must preserve the structurally valid child without conversation messages"
            );
            assert_eq!(
                sqlite_count(
                    &conn,
                    "SELECT COUNT(*) FROM ctx_events \
                     WHERE provider = 'crush' AND provider_session_id = 'crush-child' \
                     AND event_type = 'command_output'",
                ),
                0,
                "the successful child-only shell command output must remain absent from Core"
            );
            assert_eq!(
                sqlite_count(
                    &conn,
                    "SELECT COUNT(*) FROM ctx_events \
                     WHERE provider = 'crush' AND provider_session_id = 'crush-root'",
                ),
                3,
                "the fourth Crush event must not duplicate a root message"
            );
        }

        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            cli_provider,
            "--refresh",
            "off",
            "--format=json",
        ]));
        assert_search_provider_oracle(&search, stored_provider, query, 1, "message");

        let result = &search["results"].as_array().unwrap()[0];
        let ctx_event_id = result["ctx_event_id"].as_str().unwrap();
        let located =
            json_output(ctx(&temp).args(["locate", "event", ctx_event_id, "--format=json"]));
        assert_eq!(located["provider"], stored_provider);
        assert_eq!(located["source"]["source_format"], source_format);
        assert!(located["source"]["path"]
            .as_str()
            .is_some_and(|path| path.ends_with(".db") || path.ends_with(".sqlite3")));

        let second = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &fixture,
            "--format=json",
            "--progress",
            "none",
        ]));
        assert_eq!(second["totals"]["rejected_records"], 0);
        assert_eq!(second["totals"]["imported_sessions"], 0);
        assert_eq!(second["totals"]["imported_events"], 0);
    }
}
