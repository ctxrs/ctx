mod support;

use support::*;

#[test]
fn pristine_status_doctor_and_mcp_status_create_nothing() {
    let temp = tempdir();
    let data_root = temp.path().join("pristine-data-root");

    let status = json_output(
        ctx(&temp)
            .args(["status", "--format=json"])
            .env("CTX_DATA_ROOT", &data_root),
    );
    assert_eq!(status["schema_version"], 2);
    assert_eq!(status["initialized"], false);
    assert_eq!(
        status["lexical"]["path"],
        json!(data_root.join("search/lexical"))
    );
    assert_eq!(
        status["semantic"]["flat_f32"]["path"],
        json!(data_root.join("search/semantic"))
    );
    assert_eq!(status["prior_epoch"]["status"], "absent");
    assert!(!data_root.exists(), "status created a pristine data root");

    let doctor = json_output(
        ctx(&temp)
            .args(["doctor", "--format=json"])
            .env("CTX_DATA_ROOT", &data_root),
    );
    assert_eq!(doctor["schema_version"], 1);
    assert_eq!(doctor["source_epoch"]["schema_version"], 2);
    assert_eq!(doctor["source_epoch"]["prior_epoch"]["status"], "absent");
    assert!(!data_root.exists(), "doctor created a pristine data root");

    let data_root_text = data_root.to_string_lossy().into_owned();
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "ctx-test", "version": "0"}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "status", "arguments": {}}
            }),
        ],
        &[("CTX_DATA_ROOT", data_root_text.as_str())],
    );
    let mcp_status = &responses[1]["result"]["structuredContent"];
    assert_eq!(mcp_status["schema_version"], 2);
    assert_eq!(mcp_status["prior_epoch"]["status"], "absent");
    assert!(
        !data_root.exists(),
        "MCP status created a pristine data root"
    );
}

#[test]
fn setup_and_read_only_status_preserve_prior_epoch_bytes() {
    let temp = tempdir();
    let prior_path = temp.path().join("work.sqlite");
    let sentinel = b"preserved v0.25 history bytes";
    fs::write(&prior_path, sentinel).unwrap();

    let before = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(before["initialized"], false);
    assert_eq!(before["prior_epoch"]["status"], "preserved");
    assert_eq!(before["prior_epoch"]["authority"], "non_authoritative");
    assert_eq!(before["prior_epoch"]["opened"], false);

    let setup = json_output(ctx(&temp).args([
        "setup",
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(setup["schema_version"], 2);
    assert_eq!(setup["history_epoch"]["origin"], "prior_epoch_preserved");
    assert_eq!(setup["history_epoch"]["phase"], "rebuild_pending");
    assert_eq!(setup["prior_epoch"]["status"], "preserved");
    assert_eq!(setup["prior_epoch"]["authority"], "non_authoritative");
    assert_eq!(setup["prior_epoch"]["opened"], false);
    assert_eq!(setup["source_rebuild_required"], true);
    assert_eq!(setup["refresh_request"]["reason"], "explicit_opt_out");
    assert_eq!(
        setup["lexical"]["path"],
        json!(temp.path().join("search/lexical"))
    );
    assert_eq!(
        setup["semantic"]["flat_f32"]["path"],
        json!(temp.path().join("search/semantic"))
    );

    let after = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(after["prior_epoch"]["status"], "preserved");
    assert_eq!(after["prior_epoch"]["active"], false);
    assert_eq!(after["prior_epoch"]["opened"], false);
    assert_eq!(fs::read(&prior_path).unwrap(), sentinel);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(
            !PathBuf::from(format!("{}{suffix}", prior_path.display())).exists(),
            "setup/status created a prior-epoch SQLite auxiliary: {suffix}"
        );
    }
}
