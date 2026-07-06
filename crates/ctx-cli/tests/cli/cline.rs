#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn task_json_cli_imports_cline_and_roo_and_searches() {
    let temp = tempdir();
    let cline = provider_history_fixture("cline/data");

    let imported =
        json_output(ctx(&temp).args(["import", "--provider", "cline", "--path", &cline, "--json"]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "cline");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "cline_task_directory_json"
    );
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 3);
    assert_eq!(imported["totals"]["failed"], 0);

    let second =
        json_output(ctx(&temp).args(["import", "--provider", "cline", "--path", &cline, "--json"]));
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
    assert_eq!(second["totals"]["skipped_events"], 3);

    let search =
        json_output(ctx(&temp).args(["search", "parser note", "--provider", "cline", "--json"]));
    let results = search["results"].as_array().unwrap();
    assert!(!results.is_empty(), "{search:#}");
    assert!(results.iter().all(|result| result["provider"] == "cline"));

    let roo = provider_history_fixture("roo/storage");
    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "roo-code",
        "--path",
        &roo,
        "--json",
    ]));
    assert_eq!(imported["schema_version"], 1);
    assert_eq!(imported["sources"][0]["provider"], "roo_code");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "roo_task_directory_json"
    );
    assert_eq!(imported["totals"]["imported_sessions"], 2);
    assert_eq!(imported["totals"]["imported_events"], 5);
    assert_eq!(imported["totals"]["failed"], 0);

    let search = json_output(ctx(&temp).args([
        "search",
        "fallback claude_messages",
        "--provider",
        "roo",
        "--json",
    ]));
    let results = search["results"].as_array().unwrap();
    assert!(!results.is_empty(), "{search:#}");
    assert!(results
        .iter()
        .all(|result| result["provider"] == "roo_code"));
}
