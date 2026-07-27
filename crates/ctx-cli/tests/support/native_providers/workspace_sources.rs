use super::*;

#[test]
fn trae_cli_imports_explicit_workspace_storage_without_default_discovery() {
    let temp = tempdir();
    let empty_sources = json_output(ctx(&temp).args(["sources", "--json", "--all"]));
    assert!(
        empty_sources["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|source| source["provider"] != "trae"),
        "Trae workspace storage is explicit-path-only: {empty_sources:#}"
    );

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
    assert_eq!(imported["schema_version"], 2);
    assert_eq!(imported["sources"][0]["provider"], "trae");
    assert_eq!(imported["sources"][0]["source_format"], "trae_state_vscdb");
    assert_eq!(imported["totals"]["rejected_records"], 0);
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
    assert_eq!(second["totals"]["rejected_records"], 0);
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
}

#[test]
fn trae_cn_workspace_storage_requires_explicit_path_for_search_refresh() {
    let temp = tempdir();
    let query = "trae-cn-explicit-discovery-oracle";
    install_default_trae_cn_fixture(&temp, query);
    let workspace_storage = temp
        .path()
        .join("Library/Application Support/Trae CN/User/workspaceStorage");

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    assert!(
        sources["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|source| source["provider"] != "trae"),
        "Trae CN workspace storage must not be auto-discovered: {sources:#}"
    );

    let stderr = failure_stderr(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "trae-cn",
        "--refresh",
        "wait",
        "--json",
    ]));
    assert!(stderr.contains("found no supported discovered native provider"));

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "trae-cn",
        "--path",
        workspace_storage.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["totals"]["rejected_records"], 0);
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

#[test]
fn trae_workspace_storage_requires_explicit_path_for_search_refresh() {
    let temp = tempdir();
    let query = "trae-standard-explicit-discovery-oracle";
    install_default_trae_fixture(&temp, query);
    let workspace_storage = temp
        .path()
        .join("Library/Application Support/Trae/User/workspaceStorage");

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    assert!(
        sources["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|source| source["provider"] != "trae"),
        "Trae workspace storage must not be auto-discovered: {sources:#}"
    );

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "trae",
        "--path",
        workspace_storage.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "trae",
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

#[test]
fn trae_cn_workspace_storage_is_excluded_from_import_all() {
    let temp = tempdir();
    let query = "trae-cn-import-all-oracle";
    install_default_trae_cn_fixture(&temp, query);

    let stderr =
        failure_stderr(ctx(&temp).args(["import", "--all", "--json", "--progress", "none"]));
    assert!(stderr.contains("no importable provider history sources found"));

    let sources = json_output(ctx(&temp).args(["sources", "--json", "--all"]));
    assert!(sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .all(|source| source["provider"] != "trae"));
}

#[test]
fn astrbot_native_default_discovery_is_included_in_import_all() {
    let temp = tempdir();
    let query = "astrbot-import-all-oracle";
    install_default_astrbot_fixture(&temp, query);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--json", "--progress", "none"]));
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| {
            source["provider"] == "astrbot"
                && source["source_format"] == "astrbot_data_v4_sqlite"
                && source["import_support"] == "native"
        }));
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 3);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "astrbot",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "astrbot", query, 1, "message");
}
