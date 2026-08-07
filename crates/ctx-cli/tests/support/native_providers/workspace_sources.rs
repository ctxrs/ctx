use super::*;

#[test]
fn trae_cli_imports_explicit_workspace_storage_with_current_default_discovery() {
    let temp = tempdir();
    let empty_sources = json_output(ctx(&temp).args(["sources", "--format=json", "--all"]));
    assert_current_trae_default_source(&empty_sources, "missing", false);

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

    let sources = json_output(ctx(&temp).args(["sources", "--format=json", "--all"]));
    assert_current_trae_default_source(&sources, "missing", false);
    assert_no_trae_workspace_storage_source(&sources);

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

    let sources = json_output(ctx(&temp).args(["sources", "--format=json", "--all"]));
    assert_current_trae_default_source(&sources, "missing", false);
    assert_no_trae_workspace_storage_source(&sources);

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
fn trae_cn_workspace_storage_is_not_imported_by_import_all_without_current_database() {
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
    assert_current_trae_default_source(&sources, "missing", false);
    assert_no_trae_workspace_storage_source(&sources);
}

#[test]
fn trae_current_database_is_included_in_import_all_without_workspace_storage_union() {
    let temp = tempdir();
    let current_query = "current-default-only-token";
    install_current_trae_fixture(&temp, current_query);
    install_default_trae_fixture(&temp, "workspace-stale-only-token");

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
    assert_current_trae_default_source(&sources, "available", true);
    assert_no_trae_workspace_storage_source(&sources);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_authoritative_provider_publication(&imported);
    assert_eq!(
        imported["totals"]["current_source_count"], 1,
        "{imported:#}"
    );
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    assert_eq!(provider_core_counts(&data_root(&temp), "trae"), (1, 2));
    let imported_text = imported.to_string();
    assert!(
        !imported_text.contains("workspaceStorage"),
        "legacy workspaceStorage must not be unioned into import-all: {imported:#}"
    );

    let current_search = json_output(ctx(&temp).args([
        "search",
        current_query,
        "--provider",
        "trae",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&current_search, "trae", current_query, 1, "message");
}

#[test]
fn trae_encrypted_current_database_is_unknown_and_never_imported() {
    let temp = tempdir();
    let database = install_current_trae_encrypted_fixture(&temp);
    let before_bytes = fs::read(&database).unwrap();
    let before_entries = fs::read_dir(database.parent().unwrap()).unwrap().count();

    let sources = json_output(ctx(&temp).args(["sources", "--format=json", "--all"]));
    let source = current_trae_default_source(&sources);
    assert_eq!(source["status"], "unknown");
    assert_eq!(source["status_reason"], "blocked_auth_or_encryption");
    assert_eq!(source["import_support"], "unsupported");
    assert_eq!(source["native_import"], false);
    assert_eq!(source["importable"], false);
    let reason = source["unsupported_reason"].as_str().unwrap();
    assert!(reason.contains("SQLCipher-encrypted relational history"));
    assert!(reason.contains("cannot be imported without Trae key access"));

    let output = ctx(&temp)
        .args(["import", "--all", "--format=json", "--progress", "none"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("all_provider_terminal_coverage_unavailable")
            && stderr.contains("SQLCipher-encrypted relational history"),
        "{stderr}"
    );

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "trae",
        "--path",
        database.to_str().unwrap(),
        "--progress",
        "none",
    ]));
    assert!(stderr.contains("is not importable"), "{stderr}");
    assert!(
        stderr.contains("SQLCipher-encrypted relational history"),
        "{stderr}"
    );
    assert_eq!(fs::read(&database).unwrap(), before_bytes);
    assert_eq!(
        fs::read_dir(database.parent().unwrap()).unwrap().count(),
        before_entries
    );
}

#[test]
fn trae_current_database_imports_supported_chat_with_malformed_sibling() {
    let temp = tempdir();
    let query = "trae-mixed-current-oracle";
    install_current_trae_fixture(&temp, query);
    let database = temp
        .path()
        .join(".config/Trae/ModularData/ai-agent/database.db");
    Connection::open(&database)
        .unwrap()
        .execute(
            "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
            params!["chat.ChatSessionStore.index", "invalid JSON"],
        )
        .unwrap();

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
    assert_current_trae_default_source(&sources, "available", true);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_authoritative_provider_publication_with_rejections(&imported, 1);
    assert_eq!(
        imported["totals"]["current_source_count"], 1,
        "{imported:#}"
    );
    assert_eq!(imported["totals"]["current_rejected_records"], 1);
    assert_eq!(provider_core_counts(&data_root(&temp), "trae"), (1, 2));

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "trae",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "trae", query, 1, "message");
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

fn assert_current_trae_default_source<'a>(
    sources: &'a Value,
    expected_status: &str,
    expected_importable: bool,
) -> &'a Value {
    let source = current_trae_default_source(sources);
    assert_eq!(source["source_format"], "trae_state_vscdb");
    assert_eq!(source["status"], expected_status);
    assert_eq!(source["status_reason"], Value::Null);
    assert_eq!(source["import_support"], "native");
    assert_eq!(source["native_import"], true);
    assert_eq!(source["importable"], expected_importable);
    source
}

fn current_trae_default_source(sources: &Value) -> &Value {
    let source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "trae")
        .unwrap_or_else(|| panic!("missing current Trae source in {sources:#}"));
    assert_eq!(source["source_format"], "trae_state_vscdb");
    let path = source["path"].as_str().unwrap();
    assert!(
        path.ends_with(".config/Trae/ModularData/ai-agent/database.db"),
        "unexpected Trae automatic source path: {path}"
    );
    source
}

fn assert_no_trae_workspace_storage_source(sources: &Value) {
    assert!(
        sources["sources"].as_array().unwrap().iter().all(|source| {
            source["provider"] != "trae"
                || !source["path"]
                    .as_str()
                    .is_some_and(|path| path.contains("workspaceStorage"))
        }),
        "Trae workspaceStorage must remain explicit-path-only: {sources:#}"
    );
}
