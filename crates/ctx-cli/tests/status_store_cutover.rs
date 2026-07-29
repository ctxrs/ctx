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
    assert!(!data_root.exists(), "status created a pristine data root");

    let doctor = json_output(
        ctx(&temp)
            .args(["doctor", "--format=json"])
            .env("CTX_DATA_ROOT", &data_root),
    );
    assert_eq!(doctor["schema_version"], 1);
    assert_eq!(doctor["source_epoch"]["schema_version"], 2);
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
    assert!(
        !data_root.exists(),
        "MCP status created a pristine data root"
    );
}
