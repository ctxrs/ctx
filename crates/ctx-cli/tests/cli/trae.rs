#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn trae_cli_imports_explicit_workspace_storage_with_default_discovery() {
    let temp = tempdir();
    let empty_sources = json_output(ctx(&temp).args(["sources", "--json", "--all"]));
    let trae_source = empty_sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "trae")
        .unwrap_or_else(|| panic!("missing Trae default source: {empty_sources:#}"));
    assert_eq!(trae_source["status"], "missing");
    assert_eq!(trae_source["source_format"], "trae_state_vscdb");
    assert_eq!(trae_source["import_support"], "native");
    assert_eq!(trae_source["native_import"], true);
    assert_eq!(trae_source["importable"], false);

    let fixture = provider_history_fixture("trae/User/workspaceStorage");
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "trae-cn",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "trae");
    assert_eq!(imported["sources"][0]["source_format"], "trae_state_vscdb");
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search = json_output(ctx(&temp).args([
        "search",
        "trae oracle answer",
        "--provider",
        "trae-cn",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle_with_scope(
        &search,
        "trae",
        "trae oracle answer",
        1,
        "message",
        "session_result",
        "session",
    );

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "trae",
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

#[test]
pub(crate) fn trae_cn_native_default_discovery_search_refresh_imports_input_history() {
    let temp = tempdir();
    let query = "trae-cn-default-discovery-oracle";
    install_default_trae_cn_fixture(&temp, query);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == "trae"
                && source["status"] == "available"
                && source["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with("Trae CN/User/workspaceStorage"))
        })
        .unwrap_or_else(|| panic!("missing Trae CN source in {sources:#}"));
    assert_eq!(source["status"], "available");
    assert_eq!(source["source_format"], "trae_state_vscdb");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["native_import"], true);
    assert!(source["path"]
        .as_str()
        .unwrap()
        .ends_with("Trae CN/User/workspaceStorage"));

    let search = json_output(ctx(&temp).args(["search", query, "--provider", "trae-cn", "--json"]));
    assert_eq!(search["freshness"]["mode"], "auto");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["failed"], 0);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_events"], 2);
    assert_search_provider_oracle_with_scope(
        &search,
        "trae",
        query,
        1,
        "message",
        "session_result",
        "session",
    );
}

#[test]
pub(crate) fn trae_native_default_discovery_search_refresh_imports_standard_workspace_storage() {
    let temp = tempdir();
    let query = "trae-standard-default-discovery-oracle";
    install_default_trae_fixture(&temp, query);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == "trae"
                && source["status"] == "available"
                && source["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with("Trae/User/workspaceStorage"))
        })
        .unwrap_or_else(|| panic!("missing standard Trae source in {sources:#}"));
    assert_eq!(source["source_format"], "trae_state_vscdb");
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["native_import"], true);

    let search = json_output(ctx(&temp).args(["search", query, "--provider", "trae", "--json"]));
    assert_eq!(search["freshness"]["mode"], "auto");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["failed"], 0);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_events"], 2);
    assert_search_provider_oracle_with_scope(
        &search,
        "trae",
        query,
        1,
        "message",
        "session_result",
        "session",
    );
}

#[test]
pub(crate) fn trae_cn_native_default_discovery_is_included_in_import_all() {
    let temp = tempdir();
    let query = "trae-cn-import-all-oracle";
    install_default_trae_cn_fixture(&temp, query);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--json", "--progress", "none"]));
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| {
            source["provider"] == "trae"
                && source["source_format"] == "trae_state_vscdb"
                && source["import_support"] == "native"
        }));
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "trae-cn",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle_with_scope(
        &search,
        "trae",
        query,
        1,
        "message",
        "session_result",
        "session",
    );
}

pub(crate) fn install_default_trae_cn_fixture(temp: &TempDir, query: &str) {
    let workspace = temp
        .path()
        .join("Library/Application Support/Trae CN/User/workspaceStorage/cn-workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("workspace.json"),
        r#"{"folder":"file:///workspace/trae-cn-default"}"#,
    )
    .unwrap();
    let conn = Connection::open(workspace.join("state.vscdb")).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
        params![
            "icube-ai-agent-storage-input-history",
            json!([
                {
                    "id": "input-1",
                    "inputText": query,
                    "createdAt": "2026-07-05T13:00:00Z"
                },
                {
                    "id": "input-2",
                    "text": format!("{query} follow-up"),
                    "createdAt": "2026-07-05T13:01:00Z"
                }
            ])
            .to_string()
        ],
    )
    .unwrap();
}

pub(crate) fn install_default_trae_fixture(temp: &TempDir, query: &str) {
    let workspace = temp
        .path()
        .join("Library/Application Support/Trae/User/workspaceStorage/standard-workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("workspace.json"),
        r#"{"folder":"file:///workspace/trae-standard-default"}"#,
    )
    .unwrap();
    let conn = Connection::open(workspace.join("state.vscdb")).unwrap();
    conn.execute(
        "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
        params![
            "memento/icube-ai-agent-storage",
            json!({
                "list": [
                    {
                        "id": "standard-session",
                        "title": "Standard Trae default discovery",
                        "createdAt": "2026-07-05T14:00:00Z",
                        "messages": [
                            {
                                "id": "standard-user",
                                "role": "user",
                                "content": query,
                                "createdAt": "2026-07-05T14:00:00Z"
                            },
                            {
                                "id": "standard-assistant",
                                "role": "assistant",
                                "content": format!("{query} assistant reply"),
                                "createdAt": "2026-07-05T14:01:00Z"
                            }
                        ]
                    }
                ]
            })
            .to_string()
        ],
    )
    .unwrap();
}
