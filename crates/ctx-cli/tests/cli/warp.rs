#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn warp_cli_imports_explicit_sqlite() {
    let temp = tempdir();
    let fixture = provider_history_fixture("warp/v1/warp.sqlite");
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "warp",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "warp");
    assert_eq!(imported["sources"][0]["source_format"], "warp_sqlite");
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 4);

    let search = json_output(ctx(&temp).args([
        "search",
        "Warp sqlite oracle answer",
        "--provider",
        "warp",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "warp", "Warp sqlite oracle answer", 1, "message");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "warp",
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
pub(crate) fn warp_native_default_discovery_auto_imports_for_search() {
    let temp = tempdir();
    install_default_warp_fixture(&temp);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
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
        "--json",
    ]));
    assert_eq!(search["freshness"]["mode"], "auto");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["failed"], 0);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_events"], 4);
    assert_search_provider_oracle(&search, "warp", "Warp sqlite oracle answer", 1, "message");
}

#[test]
pub(crate) fn warp_native_default_discovery_is_included_in_import_all() {
    let temp = tempdir();
    install_default_warp_fixture(&temp);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--json", "--progress", "none"]));
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| {
            source["provider"] == "warp" && source["source_format"] == "warp_sqlite"
        }));
    assert_eq!(imported["totals"]["failed"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 4);

    let search = json_output(ctx(&temp).args([
        "search",
        "Warp sqlite oracle answer",
        "--provider",
        "warp",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "warp", "Warp sqlite oracle answer", 1, "message");
}

pub(crate) fn install_default_warp_fixture(temp: &TempDir) {
    let target = temp.path().join(".local/state/warp-terminal");
    fs::create_dir_all(&target).unwrap();
    fs::copy(
        provider_history_fixture("warp/v1/warp.sqlite"),
        target.join("warp.sqlite"),
    )
    .unwrap();
}
