use std::time::Duration;

use axum::{routing::get, routing::post, Json, Router};
use ctx_http_test_support::mcp_daemon::{
    setup_fake_provider_parent_session, setup_live_provider_parent_session,
};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tower::ServiceBuilder;

fn mcp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ctx-mcp")
}

fn mcp_command() -> Command {
    let mut command = Command::new(mcp_bin());
    for key in [
        "CTX_AUTH_TOKEN",
        "CTX_BUNDLE_DIR",
        "CTX_BUILD_IDENTITY_PATH",
        "CTX_DAEMON_URL",
        "CTX_MCP_DEV_MODE",
        "CTX_MCP_TOKEN",
        "CTX_SESSION_ID",
        "CTX_WORKTREE_ID",
        "CTX_WORKTREE_ROOT",
    ] {
        command.env_remove(key);
    }
    command
}

async fn wait_for_response(
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    response_id: i64,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let next_line = tokio::time::timeout_at(deadline, reader.next_line())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for response id {response_id}"));
        let Some(line) = next_line.unwrap() else {
            break;
        };
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        if value.get("id").and_then(|id| id.as_i64()) == Some(response_id) {
            return value;
        }
    }
    panic!("timed out waiting for response id {response_id}");
}

#[path = "subagent_tools/daemon_http.rs"]
mod daemon_http;
#[path = "subagent_tools/e2e_router.rs"]
mod e2e_router;
#[path = "subagent_tools/live_provider.rs"]
mod live_provider;
