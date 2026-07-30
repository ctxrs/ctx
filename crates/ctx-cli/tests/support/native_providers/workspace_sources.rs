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
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'trae'"
        ),
        1
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'trae'"
        ),
        2
    );

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
    assert_eq!(second["totals"]["rejected_records"], 0);
    assert_eq!(second["sources"][0]["catalog_changed"], false, "{second:#}");
}

#[test]
fn trae_cn_workspace_storage_requires_explicit_path_for_search_refresh() {
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

    let stderr = failure_stderr(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "trae-cn",
        "--refresh",
        "wait",
        "--format=json",
    ]));
    assert!(
        stderr.contains("no executable source-backed routes were registered"),
        "{stderr}"
    );

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
    assert_eq!(imported["totals"]["rejected_records"], 0);

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
    assert_eq!(imported["totals"]["rejected_records"], 0);

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

    let stderr =
        failure_stderr(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert!(
        stderr.contains("no executable source-backed routes were registered"),
        "{stderr}"
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
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'astrbot'"
        ),
        1
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'astrbot'"
        ),
        3
    );

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
