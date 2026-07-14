use serde_json::Value;
use tempfile::TempDir;

use super::{assert_history_envelope, assert_no_history_envelope, ctx};

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

pub(crate) fn mcp_content_text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap()
}

pub(crate) fn assert_useful_mcp_text<'a>(result: &'a Value, expected: &[&str]) -> &'a str {
    let text = mcp_content_text(result);
    assert!(
        !text.trim_start().starts_with('{'),
        "MCP content text should not be raw JSON:\n{text}"
    );
    assert!(
        !text.contains("ctx returned structured JSON"),
        "MCP content text should not be the old stub:\n{text}"
    );
    for needle in expected {
        assert!(
            text.contains(needle),
            "MCP content text missing {needle:?}:\n{text}"
        );
    }
    text
}

pub(crate) fn assert_mcp_history_envelope(result: &Value) -> String {
    let nonce = assert_history_envelope(mcp_content_text(result));
    assert_eq!(
        result["structuredContent"]["_ctx_history_envelope"]["version"],
        1
    );
    assert_eq!(
        result["structuredContent"]["_ctx_history_envelope"]["trust"],
        "untrusted"
    );
    assert_eq!(
        result["structuredContent"]["_ctx_history_envelope"]["nonce"],
        nonce
    );
    nonce
}

pub(crate) fn assert_mcp_has_no_history_envelope(result: &Value) {
    assert_no_history_envelope(mcp_content_text(result));
    assert!(
        result["structuredContent"]["_ctx_history_envelope"].is_null(),
        "unexpected structured history envelope: {result:#}"
    );
}
