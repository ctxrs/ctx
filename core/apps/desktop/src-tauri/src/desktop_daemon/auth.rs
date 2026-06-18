use super::*;

use crate::desktop_ssh::{new_ssh_command, remote_path_expr, shell_escape};

#[derive(Debug, Deserialize)]
pub(in super::super) struct DaemonAuthFile {
    pub(in super::super) token: String,
    #[serde(default)]
    pub(in super::super) daemon_url: Option<String>,
}

fn parse_daemon_auth(bytes: &[u8], path: &Path) -> Result<DaemonAuthFile> {
    let auth: DaemonAuthFile =
        serde_json::from_slice(bytes).with_context(|| format!("parsing {}", path.display()))?;
    if auth.token.trim().is_empty() {
        anyhow::bail!("daemon auth file {} contains empty token", path.display());
    }
    Ok(auth)
}

pub(in super::super) fn read_daemon_auth_with_retry(data_dir: &Path) -> Result<DaemonAuthFile> {
    let path = data_dir.join(DAEMON_AUTH_FILENAME);
    let deadline = Instant::now() + DAEMON_AUTH_READ_TIMEOUT;
    loop {
        let err = match std::fs::read(&path) {
            Ok(bytes) => return parse_daemon_auth(&bytes, &path),
            Err(err) if err.kind() == ErrorKind::NotFound => {
                anyhow!("daemon auth file not found at {}", path.display())
            }
            Err(err) => anyhow::Error::new(err)
                .context(format!("reading daemon auth file {}", path.display())),
        };
        if Instant::now() > deadline {
            return Err(err);
        }
        std::thread::sleep(DAEMON_AUTH_RETRY_DELAY);
    }
}

fn read_daemon_auth_if_present(data_dir: &Path) -> Result<Option<DaemonAuthFile>> {
    let path = data_dir.join(DAEMON_AUTH_FILENAME);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(parse_daemon_auth(&bytes, &path)?)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => {
            Err(anyhow::Error::new(err)
                .context(format!("reading daemon auth file {}", path.display())))
        }
    }
}

pub(in super::super) fn resolve_env_local_daemon(
    app: &tauri::AppHandle,
) -> Result<Option<(String, String)>> {
    let url = match std::env::var("CTX_DESKTOP_DAEMON_URL") {
        Ok(v) => v.trim().to_string(),
        Err(_) => return Ok(None),
    };
    if url.is_empty() {
        return Ok(None);
    }
    let token = match std::env::var("CTX_DESKTOP_DAEMON_TOKEN") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => {
            let data_dir = daemon_data_dir(app)?;
            match read_daemon_auth_if_present(&data_dir)? {
                Some(auth) if auth.daemon_url.as_deref() == Some(url.as_str()) => auth.token,
                _ => {
                    anyhow::bail!(
                        "CTX_DESKTOP_DAEMON_URL is set but no matching token found (set CTX_DESKTOP_DAEMON_TOKEN)"
                    );
                }
            }
        }
    };
    Ok(Some((url, token)))
}

pub(in super::super) fn resolve_existing_local_daemon(
    app: &tauri::AppHandle,
    data_dir: &Path,
) -> Result<Option<(String, String, Option<u32>)>> {
    let Some(auth) = read_daemon_auth_if_present(data_dir)? else {
        return Ok(None);
    };
    let Some(url) = auth.daemon_url.as_deref() else {
        return Ok(None);
    };
    let desktop_identity = load_desktop_build_identity(app)?;
    let Ok(health) = daemon_health_with_auth(url, Some(auth.token.as_str())) else {
        return Ok(None);
    };
    if local_daemon_health_matches_expected(&health, data_dir, &desktop_identity) {
        return Ok(Some((
            url.to_string(),
            auth.token,
            normalize_daemon_pid(health.pid),
        )));
    }
    if should_reclaim_incompatible_local_daemon(url, &health, data_dir) {
        if let Err(err) = reclaim_incompatible_local_daemon(url, &health, Some(auth.token.as_str()))
            .with_context(|| format!("reclaiming incompatible local daemon at {url}"))
        {
            eprintln!("{err:#}");
        }
    }
    Ok(None)
}

fn read_remote_daemon_auth(
    host: &str,
    user: Option<&str>,
    remote_data_dir: Option<&str>,
) -> Result<DaemonAuthFile> {
    let target = match user {
        Some(u) if !u.trim().is_empty() => format!("{}@{}", u.trim(), host),
        _ => host.to_string(),
    };
    let data_dir = remote_data_dir
        .filter(|d| !d.trim().is_empty())
        .unwrap_or("~/.ctx");
    let auth_path = format!(
        "{}/{}",
        data_dir.trim_end_matches('/'),
        DAEMON_AUTH_FILENAME
    );
    let cmd = format!("cat -- {}", remote_path_expr(&auth_path));
    let remote_cmd = format!("sh -lc {}", shell_escape(&cmd));

    let output = new_ssh_command()
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=15")
        .arg("-o")
        .arg("ServerAliveInterval=15")
        .arg("-o")
        .arg("ServerAliveCountMax=2")
        .arg(target)
        // NOTE: sshd does not preserve argv boundaries for the remote command; pass as one string.
        .arg(remote_cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("reading daemon auth file over ssh")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains(DAEMON_AUTH_FILENAME) && stderr.contains("No such file") {
            return Err(anyhow!(
                "remote daemon auth file is missing at {auth_path}; the remote daemon is not ready or was started outside managed ctx bootstrap. Reconnect with remote start enabled so ctx can start the managed daemon."
            ));
        }
        return Err(anyhow!("ssh read failed: {}", stderr));
    }

    parse_daemon_auth(&output.stdout, Path::new(&auth_path))
}

pub(in super::super) fn read_remote_daemon_auth_with_retry(
    host: &str,
    user: Option<&str>,
    remote_data_dir: Option<&str>,
) -> Result<DaemonAuthFile> {
    let deadline = Instant::now() + DAEMON_AUTH_REMOTE_TIMEOUT;
    loop {
        let err = match read_remote_daemon_auth(host, user, remote_data_dir) {
            Ok(auth) => return Ok(auth),
            Err(err) => err,
        };
        if Instant::now() > deadline {
            return Err(err);
        }
        std::thread::sleep(DAEMON_AUTH_RETRY_DELAY);
    }
}
