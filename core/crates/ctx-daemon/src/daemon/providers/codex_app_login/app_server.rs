use super::process::CodexLoginCompletion;
use super::*;

mod rpc;

pub(super) use rpc::{send_codex_jsonrpc, wait_for_codex_response};

pub(super) fn spawn_codex_app_server(
    account_dir: &PathBuf,
    codex_bin: &str,
) -> anyhow::Result<tokio::process::Child> {
    let codex_bin = codex_bin.trim();
    if codex_bin.is_empty() {
        anyhow::bail!("codex-cli runtime path is empty");
    }
    if !std::path::Path::new(codex_bin).is_absolute() {
        anyhow::bail!("codex-cli runtime path must be absolute, got `{codex_bin}`");
    }
    let mut cmd = Command::new(codex_bin);
    for key in ctx_core::env::DAEMON_AUTH_ENV_VARS {
        cmd.env_remove(key);
    }
    cmd.args(CODEX_APP_SERVER_ARGS)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for key in [
        "CTX_PROVIDER_SESSION_REF",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "CLAUDE_THREAD_ID",
        "GEMINI_SESSION_ID",
        "GEMINI_THREAD_ID",
        "ACP_SESSION_ID",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("CODEX_HOME", account_dir);
    cmd.spawn()
        .with_context(|| format!("spawning codex app-server via `{codex_bin}`"))
}

pub(super) async fn wait_for_codex_login_completion(
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    login_id: &str,
) -> anyhow::Result<CodexLoginCompletion> {
    loop {
        let line = reader
            .next_line()
            .await
            .context("codex login read failed")?;
        let line = line.ok_or_else(|| anyhow::anyhow!("codex login stdout closed"))?;
        let value: serde_json::Value = serde_json::from_str(&line)?;
        let method = value.get("method").and_then(|v| v.as_str());
        if method != Some("account/login/completed") {
            continue;
        }
        let params = value.get("params").unwrap_or(&serde_json::Value::Null);
        let found_login_id = params
            .get("loginId")
            .or_else(|| params.get("login_id"))
            .and_then(|v| v.as_str());
        if let Some(found) = found_login_id {
            if found != login_id {
                continue;
            }
        }
        let success = params
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let error = params
            .get("error")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        return Ok(CodexLoginCompletion { success, error });
    }
}

pub(super) async fn fetch_codex_account_details(
    stdin: &mut tokio::process::ChildStdin,
    reader: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "account/read",
        "params": { "refreshToken": false }
    });
    send_codex_jsonrpc(stdin, &request).await?;
    let response = wait_for_codex_response(reader, 3, CODEX_LOGIN_RPC_TIMEOUT).await?;
    let account = response
        .get("result")
        .and_then(|v| v.get("account"))
        .and_then(|v| v.as_object());
    let Some(account) = account else {
        return Ok((None, None));
    };
    if account.get("type").and_then(|v| v.as_str()) != Some("chatgpt") {
        return Ok((None, None));
    }
    let email = account
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let plan_type = account
        .get("planType")
        .or_else(|| account.get("plan_type"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok((email, plan_type))
}
