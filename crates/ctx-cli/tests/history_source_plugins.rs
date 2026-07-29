mod support;

use support::*;

#[test]
fn history_source_plugins_are_discoverable_but_not_importable_in_the_new_epoch() {
    let temp = tempdir();
    let plugin =
        write_history_source_plugin_with_refresh(&temp, "dorkos", true, Some("auto"), None);

    let sources = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args(["sources", "--format=json"]),
    );
    let plugin_source = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["history_source"] == "dorkos/default")
        .unwrap();
    assert_eq!(plugin_source["kind"], "history_source_plugin");
    assert_eq!(plugin_source["status"], "unsupported");
    assert_eq!(plugin_source["importable"], false);
    assert_eq!(
        plugin_source["unsupported_reason"],
        "history source plugin has no v0.26 source-backed adapter"
    );
    assert!(!plugin.run_marker.exists());
}

#[test]
fn invalid_or_oversized_plugin_manifests_remain_bounded_diagnostics() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let invalid = plugin_root.join("invalid");
    let oversized = plugin_root.join("oversized");
    fs::create_dir_all(&invalid).unwrap();
    fs::create_dir_all(&oversized).unwrap();
    fs::write(invalid.join("ctx-history-plugin.json"), "{not-json").unwrap();
    fs::write(
        oversized.join("ctx-history-plugin.json"),
        vec![b' '; 2 * 1024 * 1024],
    )
    .unwrap();

    let sources = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args(["sources", "--format=json"]),
    );
    let failures = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|source| {
            source["kind"] == "history_source_plugin" && source["status"] == "invalid"
        })
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 2);
    assert!(failures.iter().all(|source| source["importable"] == false));
    assert!(failures.iter().any(|source| source["error"]
        .as_str()
        .unwrap()
        .contains("parse history source plugin manifest")));
    assert!(failures.iter().any(|source| source["error"]
        .as_str()
        .unwrap()
        .contains("exceeds max bytes")));
}

#[test]
fn plugin_import_is_rejected_before_command_execution() {
    let temp = tempdir();
    let plugin =
        write_history_source_plugin_with_refresh(&temp, "dorkos", true, Some("auto"), None);

    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "dorkos/default",
                "--progress",
                "none",
            ]),
    );
    assert!(
        stderr.contains("history source plugin imports have no source-backed adapter"),
        "{stderr}"
    );
    assert!(!plugin.run_marker.exists());
}
