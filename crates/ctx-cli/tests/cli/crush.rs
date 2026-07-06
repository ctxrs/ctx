#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn sqlite_cli_imports_crush_goose_zed_kiro_and_forgecode_and_searches() {
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
            4,
        ),
        (
            "goose",
            "goose",
            "goose_sessions_sqlite",
            "goose/v14/sessions.db",
            "goose oracle",
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
        let fixture = provider_history_fixture(fixture);

        let imported = json_output(ctx(&temp).args([
            "import",
            "--provider",
            cli_provider,
            "--path",
            &fixture,
            "--json",
            "--progress",
            "none",
        ]));
        assert_eq!(imported["schema_version"], 1);
        assert_eq!(imported["sources"][0]["provider"], stored_provider);
        assert_eq!(imported["sources"][0]["source_format"], source_format);
        assert_eq!(imported["totals"]["failed"], 0);
        assert_eq!(imported["totals"]["imported_sessions"], sessions);
        assert_eq!(imported["totals"]["imported_events"], events);

        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            cli_provider,
            "--refresh",
            "off",
            "--json",
        ]));
        assert_search_provider_oracle(&search, stored_provider, query, 1, "message");

        let result = &search["results"].as_array().unwrap()[0];
        let ctx_event_id = result["ctx_event_id"].as_str().unwrap();
        let located = json_output(ctx(&temp).args(["locate", "event", ctx_event_id, "--json"]));
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
            "--json",
            "--progress",
            "none",
        ]));
        assert_eq!(second["totals"]["failed"], 0);
        assert_eq!(second["totals"]["imported_sessions"], 0);
        assert_eq!(second["totals"]["imported_events"], 0);
    }
}
