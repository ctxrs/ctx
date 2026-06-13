use super::app_server::{send_codex_jsonrpc, spawn_codex_app_server, wait_for_codex_response};
use super::*;

#[path = "process/monitor.rs"]
mod monitor;

pub(super) use monitor::monitor_codex_login;

pub(super) struct CodexLoginProcess {
    pub(super) login_id: String,
    pub(super) auth_url: String,
    account_dir: PathBuf,
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

pub(super) struct CodexLoginCompletion {
    pub(super) success: bool,
    pub(super) error: Option<String>,
}

pub(super) async fn start_codex_login_process(
    account_dir: &PathBuf,
    codex_bin: &str,
) -> anyhow::Result<CodexLoginProcess> {
    let mut child = spawn_codex_app_server(account_dir, codex_bin)?;
    let result = async {
        let stdout = child
            .stdout
            .take()
            .context("codex app-server stdout unavailable")?;
        let mut reader = BufReader::new(stdout).lines();
        let mut stdin = child
            .stdin
            .take()
            .context("codex app-server stdin unavailable")?;

        let init_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "ctx",
                    "title": "ctx",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        });
        send_codex_jsonrpc(&mut stdin, &init_request).await?;
        wait_for_codex_response(&mut reader, 1, CODEX_LOGIN_RPC_TIMEOUT).await?;
        send_codex_jsonrpc(
            &mut stdin,
            &serde_json::json!({"jsonrpc": "2.0", "method": "initialized"}),
        )
        .await?;

        let login_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "account/login/start",
            "params": { "type": "chatgpt" }
        });
        send_codex_jsonrpc(&mut stdin, &login_request).await?;
        let response = wait_for_codex_response(&mut reader, 2, CODEX_LOGIN_RPC_TIMEOUT).await?;
        let result = response
            .get("result")
            .and_then(|v| v.as_object())
            .context("codex login missing result")?;
        let auth_url = result
            .get("authUrl")
            .or_else(|| result.get("auth_url"))
            .and_then(|v| v.as_str())
            .context("codex login missing auth_url")?
            .to_string();
        let login_id = result
            .get("loginId")
            .or_else(|| result.get("login_id"))
            .and_then(|v| v.as_str())
            .context("codex login missing login_id")?
            .to_string();
        anyhow::Ok((login_id, auth_url, stdin, reader))
    }
    .await;

    let (login_id, auth_url, stdin, reader) = match result {
        Ok(parts) => parts,
        Err(err) => {
            let _ = child.kill().await;
            return Err(err);
        }
    };

    Ok(CodexLoginProcess {
        login_id,
        auth_url,
        account_dir: account_dir.clone(),
        child,
        stdin,
        reader,
    })
}
