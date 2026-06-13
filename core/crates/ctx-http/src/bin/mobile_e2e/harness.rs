use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

use super::dto::{DaemonAuthFile, WorkspaceSummary};

pub(super) async fn create_workspace(
    client: &reqwest::Client,
    daemon_url: &str,
    auth_token: &str,
    repo_root: &Path,
) -> Result<String> {
    let resp = client
        .post(format!("{daemon_url}/api/workspaces"))
        .bearer_auth(auth_token)
        .json(&serde_json::json!({
            "root_path": repo_root.to_string_lossy(),
            "name": "e2e",
        }))
        .send()
        .await?
        .error_for_status()
        .context("create workspace")?
        .json::<WorkspaceSummary>()
        .await?;
    Ok(resp.id)
}

pub(super) async fn wait_for_health(client: &reqwest::Client, daemon_url: &str) -> Result<()> {
    for _ in 0..60 {
        match client.get(format!("{daemon_url}/api/health")).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    Err(anyhow!("timed out waiting for daemon health"))
}

pub(super) async fn wait_for_public_tunnel(client: &reqwest::Client, base_url: &str) -> Result<()> {
    let health_url = format!("{base_url}/api/health");
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut last_status = None::<reqwest::StatusCode>;

    loop {
        match client.get(&health_url).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            Ok(resp) => last_status = Some(resp.status()),
            Err(_) => {}
        }
        if Instant::now() > deadline {
            let detail = last_status
                .map(|status| format!("last status {status}"))
                .unwrap_or_else(|| "no HTTP response".to_string());
            return Err(anyhow!(
                "timed out waiting for public tunnel health at {health_url}: {detail}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub(super) async fn read_daemon_auth_token(data_dir: &Path) -> Result<String> {
    let path = data_dir.join("daemon_auth.json");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let auth: DaemonAuthFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing daemon auth file {}", path.display()))?;
                if auth.token.trim().is_empty() {
                    return Err(anyhow!(
                        "daemon auth file {} contains empty token",
                        path.display()
                    ));
                }
                return Ok(auth.token);
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("reading daemon auth file {}", path.display()));
            }
        }
        if Instant::now() > deadline {
            return Err(anyhow!("daemon auth file not found at {}", path.display()));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub(super) fn pick_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    Ok(port)
}

pub(super) fn init_git_repo(path: &Path) -> Result<()> {
    run_cmd(
        std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("init"),
    )?;
    run_cmd(std::process::Command::new("git").arg("-C").arg(path).args([
        "config",
        "user.email",
        "e2e@example.com",
    ]))?;
    run_cmd(std::process::Command::new("git").arg("-C").arg(path).args([
        "config",
        "user.name",
        "E2E",
    ]))?;
    std::fs::write(path.join("README.md"), "e2e\n")?;
    run_cmd(
        std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["add", "."]),
    )?;
    run_cmd(
        std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["commit", "-m", "init"]),
    )?;
    Ok(())
}

fn run_cmd(cmd: &mut std::process::Command) -> Result<()> {
    let output = cmd.output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    ))
}
