use super::*;

#[test]
fn trae_cli_imports_explicit_workspace_storage_without_default_discovery() {
    let temp = tempdir();
    let empty_sources = json_output(ctx(&temp).args(["sources", "--format=json", "--all"]));
    assert!(
        empty_sources["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|source| source["provider"] != "trae"),
        "Trae workspace storage is explicit-path-only: {empty_sources:#}"
    );

    let fixture = PathBuf::from(provider_history_fixture("trae/User/workspaceStorage"))
        .join("trae-workspace-1/state.vscdb");
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "trae-cn",
        "--path",
        fixture.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_explicit_source_publication(&imported, "trae", "trae_state_vscdb");
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert_eq!(provider_core_counts(&data_root(&temp), "trae"), (1, 2));

    let search = json_output(ctx(&temp).args([
        "search",
        "trae oracle answer",
        "--provider",
        "trae-cn",
        "--refresh",
        "off",
        "--format=json",
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
        fixture.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_explicit_source_publication(&second, "trae", "trae_state_vscdb");
    assert_eq!(second["totals"]["current_rejected_records"], 0);
    assert_noop_publication(&second);
}

#[test]
fn trae_cn_workspace_storage_returns_verified_empty_until_explicit_path_import() {
    let temp = tempdir();
    let query = "trae-cn-explicit-discovery-oracle";
    install_default_trae_cn_fixture(&temp, query);
    let workspace_storage = temp
        .path()
        .join("Library/Application Support/Trae CN/User/workspaceStorage")
        .join("cn-workspace/state.vscdb");

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
    assert!(
        sources["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|source| source["provider"] != "trae"),
        "Trae CN workspace storage must not be auto-discovered: {sources:#}"
    );

    let empty = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "trae-cn",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert_eq!(empty["schema_version"], 1, "{empty:#}");
    assert_eq!(empty["filters"]["provider"], "trae", "{empty:#}");
    assert_eq!(empty["freshness"]["mode"], "wait", "{empty:#}");
    assert_eq!(empty["freshness"]["status"], "completed", "{empty:#}");
    assert_eq!(empty["freshness"]["source_count"], 0, "{empty:#}");
    assert_eq!(empty["retrieval"]["indexed_documents"], 0, "{empty:#}");
    assert!(empty["results"].as_array().unwrap().is_empty(), "{empty:#}");

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "trae-cn",
        "--path",
        workspace_storage.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_explicit_source_publication(&imported, "trae", "trae_state_vscdb");
    assert_eq!(imported["totals"]["current_rejected_records"], 0);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "trae-cn",
        "--refresh",
        "off",
        "--format=json",
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
        .join("Library/Application Support/Trae/User/workspaceStorage")
        .join("standard-workspace/state.vscdb");

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
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
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_explicit_source_publication(&imported, "trae", "trae_state_vscdb");
    assert_eq!(imported["totals"]["current_rejected_records"], 0);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "trae",
        "--refresh",
        "off",
        "--format=json",
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

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_authoritative_provider_publication(&imported);
    assert_eq!(
        imported["totals"]["current_source_count"], 0,
        "{imported:#}"
    );
    assert_eq!(
        imported["totals"]["current_indexed_documents"], 0,
        "{imported:#}"
    );
    assert_eq!(
        imported["totals"]["current_rejected_records"], 0,
        "{imported:#}"
    );

    let sources = json_output(ctx(&temp).args(["sources", "--format=json", "--all"]));
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
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_authoritative_provider_publication(&imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert_eq!(provider_core_counts(&data_root(&temp), "astrbot"), (1, 3));

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "astrbot",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "astrbot", query, 1, "message");
}
