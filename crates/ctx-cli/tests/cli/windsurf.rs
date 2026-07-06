#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn windsurf_default_discovery_is_native_and_search_refresh_imports() {
    let temp = tempdir();
    let query = "windsurf-native-default-discovery-oracle";
    install_default_windsurf_fixture(&temp, query);

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let windsurf = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "windsurf")
        .unwrap();
    assert_eq!(windsurf["status"], "available");
    assert_eq!(
        windsurf["source_format"],
        "windsurf_cascade_hook_transcript_jsonl_tree"
    );
    assert_eq!(windsurf["import_support"], "native");
    assert_eq!(windsurf["native_import"], true);
    assert_eq!(windsurf["importable"], true);
    assert!(windsurf["path"]
        .as_str()
        .unwrap()
        .ends_with(".windsurf/transcripts"));

    let search =
        json_output(ctx(&temp).args(["search", query, "--provider", "windsurf", "--json"]));
    assert_eq!(search["freshness"]["mode"], "auto");
    assert_eq!(search["freshness"]["status"], "completed");
    assert_eq!(search["freshness"]["source_count"], 1);
    assert_eq!(search["freshness"]["totals"]["failed"], 0);
    assert_eq!(search["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(search["freshness"]["totals"]["imported_events"], 3);
    assert_search_provider_oracle(&search, "windsurf", query, 1, "message");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "windsurf",
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["failed"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
}

pub(crate) fn install_default_windsurf_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_windsurf_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".windsurf").join("transcripts"));
}

pub(crate) fn write_native_windsurf_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp.path().join("native-windsurf/transcripts");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("windsurf-cli-native.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            json!({
                "status": "done",
                "type": "user_input",
                "user_input": {"user_response": query}
            }),
            json!({
                "status": "done",
                "type": "planner_response",
                "planner_response": {"response": "native import ok"}
            }),
            json!({
                "status": "done",
                "type": "code_action",
                "code_action": {
                    "path": "src/windsurf_cli_native.py",
                    "new_content": "print('native import ok')\n"
                }
            })
        ),
    )
    .unwrap();
    root.to_str().unwrap().to_owned()
}
