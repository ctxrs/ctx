#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn deepagents_cli_sources_import_search_and_reimport_with_aliases() {
    let temp = tempdir();
    let default_db = temp.path().join(".deepagents/.state/sessions.db");
    fs::create_dir_all(default_db.parent().unwrap()).unwrap();
    fs::copy(
        provider_history_fixture("deepagents/v1/sessions.db"),
        &default_db,
    )
    .unwrap();

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
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
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["sources"][0]["provider"], "deepagents");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "deepagents_sessions_sqlite"
    );
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 3);

    let search = json_output(ctx(&temp).args([
        "search",
        "deepagents fixture oracle",
        "--provider",
        "dcode",
        "--refresh",
        "off",
        "--json",
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
        "--json",
    ]));
    assert_eq!(second["totals"]["failed"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(
            &conn,
            "SELECT COUNT(*) FROM events e JOIN sessions s ON e.session_id = s.id WHERE s.provider = 'deepagents'"
        ),
        3
    );
}
