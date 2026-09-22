use super::*;
use ctx_attribution::materializer::{SegmentMaterializer, CORE_MATERIALIZER_REVISION};

#[test]
fn history_only_import_skips_blame_and_normal_import_catches_up() {
    let temp = daemon_test_root();
    let root = data_root(&temp);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("config.toml"), "[indexing]\nmode = \"manual\"\n").unwrap();
    write_codex_setup_session(&temp);
    ctx(&temp)
        .args(["import", "--all", "--no-blame", "--progress=none"])
        .timeout(Duration::from_secs(15))
        .assert()
        .success()
        .stderr("");
    assert!(!root.join("search/attribution").exists());
    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["lexical"]["status"], "ready");
    assert_eq!(status["attribution"]["indexing_enabled"], true);
    assert_eq!(status["attribution"]["currentness"], "not_materialized");
    let search = json_output(ctx(&temp).args([
        "search",
        "setup should import",
        "--refresh=off",
        "--format=json",
    ]));
    assert!(!search["results"].as_array().unwrap().is_empty());
    let output = ctx(&temp)
        .args(["import", "--all", "--progress=json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let events: Vec<Value> = String::from_utf8(output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events
        .iter()
        .any(|event| event["phase"] == "blame_complete"));
    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["attribution"]["currentness"], "current");
    assert!(status["attribution"]["progress"].is_null());

    let owner = SegmentMaterializer::open_for_revision(
        root.join("search/attribution"),
        CORE_MATERIALIZER_REVISION,
    )
    .unwrap();
    let path = temp
        .path()
        .join(".codex/sessions/2026/06/24/codex-session-setup.jsonl");
    let body = fs::read_to_string(&path)
        .unwrap()
        .replace("setup should import", "history only revised");
    fs::write(&path, body).unwrap();
    ctx(&temp)
        .args(["import", "--provider=codex", "--path"])
        .arg(&path)
        .args(["--no-blame", "--progress=none"])
        .timeout(Duration::from_secs(15))
        .assert()
        .success()
        .stderr("");
    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["attribution"]["currentness"], "stale");
    assert!(status["attribution"]["progress"].is_object());
    drop(owner);
    ctx(&temp)
        .args(["import", "--all", "--progress=none"])
        .assert()
        .success()
        .stderr("");
    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["attribution"]["currentness"], "current");
}

#[test]
fn configured_blame_opt_out_applies_to_setup_import_and_preserves_existing_index() {
    let temp = daemon_test_root();
    let root = data_root(&temp);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"manual\"\n[blame]\nenabled = false\n",
    )
    .unwrap();
    write_codex_setup_session(&temp);
    for args in [
        vec!["setup", "--wait", "--progress=none"],
        vec!["import", "--all", "--progress=none"],
    ] {
        ctx(&temp)
            .args(args)
            .timeout(Duration::from_secs(15))
            .assert()
            .success()
            .stderr("");
        assert!(!root.join("search/attribution").exists());
    }
    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["attribution"]["indexing_enabled"], false);
    assert_eq!(status["lexical"]["status"], "ready");
    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"manual\"\n[blame]\nenabled = true\n",
    )
    .unwrap();
    let setup =
        json_output(ctx(&temp).args(["setup", "--wait", "--progress=none", "--format=json"]));
    let before = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(
        before["attribution"]["currentness"], "current",
        "setup={setup:#}; status={before:#}"
    );
    let owner = SegmentMaterializer::open_for_revision(
        root.join("search/attribution"),
        CORE_MATERIALIZER_REVISION,
    )
    .unwrap();
    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"manual\"\n[blame]\nenabled = false\n",
    )
    .unwrap();
    ctx(&temp)
        .args(["import", "--all", "--progress=none"])
        .timeout(Duration::from_secs(15))
        .assert()
        .success()
        .stderr("");
    let after = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(
        after["attribution"]["receipt"],
        before["attribution"]["receipt"]
    );
    assert_eq!(after["attribution"]["indexing_enabled"], false);
    drop(owner);
}

#[test]
fn configured_blame_opt_out_also_applies_to_daemon_refresh() {
    let temp = daemon_test_root();
    write_codex_setup_session(&temp);
    let _daemon = start_source_refresh_daemon_with_config(
        &temp,
        "full",
        "[indexing]\nmode = \"auto\"\n[blame]\nenabled = false\n",
    );
    wait_for_initial_source_refresh(&temp);
    ctx(&temp)
        .args(["import", "--all", "--progress=none"])
        .assert()
        .success();
    let root = data_root(&temp);
    assert!(!root.join("search/attribution").exists());
    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["daemon"]["running"], true);
    assert_eq!(status["attribution"]["indexing_enabled"], false);
    assert_eq!(status["lexical"]["status"], "ready");
}
