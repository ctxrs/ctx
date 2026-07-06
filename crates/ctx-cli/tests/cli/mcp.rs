#[allow(unused_imports)]
use super::*;

pub(crate) fn mcp_roundtrip(temp: &TempDir, messages: &[Value]) -> Vec<Value> {
    mcp_roundtrip_with_env(temp, messages, &[])
}

pub(crate) fn mcp_roundtrip_with_env(
    temp: &TempDir,
    messages: &[Value],
    envs: &[(&str, &str)],
) -> Vec<Value> {
    let mut stdin = String::new();
    for message in messages {
        stdin.push_str(&serde_json::to_string(message).unwrap());
        stdin.push('\n');
    }
    let mut command = ctx(temp);
    command.args(["mcp", "serve"]);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .write_stdin(stdin)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub(crate) fn mcp_raw_roundtrip(temp: &TempDir, stdin: String) -> Vec<Value> {
    mcp_raw_roundtrip_bytes(temp, stdin.into_bytes())
}

pub(crate) fn mcp_raw_roundtrip_bytes(temp: &TempDir, stdin: Vec<u8>) -> Vec<Value> {
    let output = ctx(temp)
        .args(["mcp", "serve"])
        .write_stdin(stdin)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
pub(crate) fn mcp_rejects_oversized_input_line_and_continues() {
    let temp = tempdir();
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "ctx-test", "version": "0" }
        }
    });
    let mut stdin = "x".repeat(1024 * 1024 + 1);
    stdin.push('\n');
    stdin.push_str(&serde_json::to_string(&initialize).unwrap());
    stdin.push('\n');

    let responses = mcp_raw_roundtrip(&temp, stdin);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert!(
        responses[0]["error"]["data"]["error"]
            .as_str()
            .unwrap()
            .contains("exceeds max line bytes"),
        "{:#}",
        responses[0]
    );
    assert_eq!(responses[1]["result"]["serverInfo"]["name"], "ctx");
}

#[test]
pub(crate) fn mcp_rejects_invalid_utf8_input_line_and_continues() {
    let temp = tempdir();
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "ctx-test", "version": "0" }
        }
    });
    let mut stdin = vec![0xff, b'\n'];
    stdin.extend_from_slice(serde_json::to_string(&initialize).unwrap().as_bytes());
    stdin.push(b'\n');

    let responses = mcp_raw_roundtrip_bytes(&temp, stdin);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert_eq!(
        responses[0]["error"]["data"]["error"],
        "MCP message is not valid UTF-8"
    );
    assert_eq!(responses[1]["result"]["serverInfo"]["name"], "ctx");
}

#[test]
pub(crate) fn mcp_rejects_unknown_tool_arguments() {
    let temp = tempdir();
    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "search",
                "method": "tools/call",
                "params": {
                    "name": "search",
                    "arguments": {
                        "query": "onboarding",
                        "refresh": "strict"
                    }
                }
            }),
        ],
    );

    let error = &responses[1]["error"];
    assert_eq!(error["code"], -32602);
    assert!(error["data"]["error"]
        .as_str()
        .unwrap()
        .contains("unknown argument refresh"));
}
