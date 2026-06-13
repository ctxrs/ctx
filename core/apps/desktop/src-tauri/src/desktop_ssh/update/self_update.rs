use super::*;

fn remote_self_update_cmd(ctx_bin: &str, channel: &str, release_base_url: &str) -> String {
    format!(
        "if [ -x {ctx_bin} ]; then {ctx_bin} self-update --yes --channel {channel} --base-url {release_base_url}; else echo 'ctx not executable at configured remote path' >&2; exit 127; fi",
        ctx_bin = remote_path_expr(ctx_bin),
        channel = shell_escape(channel),
        release_base_url = shell_escape(release_base_url),
    )
}

pub(crate) fn run_remote_daemon_self_update(
    app: &tauri::AppHandle,
    host: &str,
    user: Option<&str>,
    remote_port: u16,
    remote_data_dir: Option<&str>,
    remote_ctx_bin: &str,
    remote_arch: &str,
    channel: &str,
    daemon_base_url: &str,
    daemon_auth_token: &str,
    release_base_url: &str,
) -> Result<()> {
    let ctx_bin = validate_remote_ctx_bin(remote_ctx_bin)?;
    let backup_ctx_bin = remote_update_backup_ctx_bin(&ctx_bin)?;
    backup_remote_ctx_bin_over_ssh(host, user, &ctx_bin, &backup_ctx_bin)
        .context("backing up remote daemon binary before self-update")?;
    let update_cmd = remote_self_update_cmd(&ctx_bin, channel, release_base_url);
    let mut daemon_stopped = false;
    let mut bundle_synced = false;
    let update_result = (|| {
        let output =
            run_remote_ssh_shell(host, user, &update_cmd).context("running remote self-update")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if !stderr.is_empty() { stderr } else { stdout };
            anyhow::bail!("remote self-update failed: {detail}");
        }

        sync_remote_bundle_metadata_over_ssh(
            app,
            host,
            user,
            remote_data_dir,
            remote_arch,
            channel,
        )
        .context("syncing remote bundle metadata before daemon restart")?;
        bundle_synced = true;
        stop_remote_daemon_over_ssh(host, user, remote_port, remote_data_dir, &ctx_bin)
            .context("stopping remote daemon after self-update")?;
        daemon_stopped = true;
        start_remote_daemon_over_ssh(
            host,
            user,
            remote_port,
            remote_data_dir,
            &ctx_bin,
            Some(channel),
            Some(release_base_url),
        )
        .context("starting remote daemon after self-update")?;
        wait_for_remote_daemon_health(daemon_base_url, daemon_auth_token)
            .context("waiting for restarted remote daemon health")?;
        Ok(())
    })();

    match update_result {
        Ok(()) => {
            if let Err(err) = cleanup_remote_update_backup_over_ssh(host, user, &backup_ctx_bin) {
                eprintln!("failed to remove remote update backup {backup_ctx_bin}: {err:#}");
            }
            if let Err(err) = cleanup_remote_bundle_backup_over_ssh(host, user, remote_data_dir) {
                eprintln!("failed to remove remote bundle update backup: {err:#}");
            }
            Ok(())
        }
        Err(err) if daemon_stopped => {
            match rollback_remote_daemon_update_over_ssh(
                host,
                user,
                remote_port,
                remote_data_dir,
                &ctx_bin,
                &backup_ctx_bin,
                daemon_base_url,
                daemon_auth_token,
                bundle_synced,
            ) {
                Ok(()) => Err(anyhow!(
                    "{err:#}; restored the previous remote daemon binary/bundles and restarted it"
                )),
                Err(rollback_err) => Err(anyhow!("{err:#}; rollback failed: {rollback_err:#}")),
            }
        }
        Err(err) => {
            if bundle_synced {
                if let Err(restore_err) =
                    restore_remote_bundle_backup_over_ssh(host, user, remote_data_dir)
                {
                    return Err(anyhow!(
                        "{err:#}; failed to restore pre-update remote bundle metadata while the old daemon was still running: {restore_err:#}"
                    ));
                }
            }
            if let Err(restore_err) =
                restore_remote_ctx_bin_over_ssh(host, user, &ctx_bin, &backup_ctx_bin)
            {
                return Err(anyhow!(
                    "{err:#}; failed to restore pre-update daemon binary while the old daemon was still running: {restore_err:#}"
                ));
            }
            if let Err(cleanup_err) =
                cleanup_remote_update_backup_over_ssh(host, user, &backup_ctx_bin)
            {
                eprintln!(
                    "failed to remove remote update backup {backup_ctx_bin}: {cleanup_err:#}"
                );
            }
            Err(err)
        }
    }
}

fn stop_remote_daemon_over_ssh(
    host: &str,
    user: Option<&str>,
    remote_port: u16,
    remote_data_dir: Option<&str>,
    remote_ctx_bin: &str,
) -> Result<()> {
    let cmd = remote_stop_daemon_cmd(remote_port, remote_data_dir, remote_ctx_bin);
    let output = run_remote_ssh_shell(host, user, &cmd).context("stopping remote daemon")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    anyhow::bail!("remote stop command failed: {detail}");
}

pub(crate) fn remote_stop_daemon_cmd(
    remote_port: u16,
    remote_data_dir: Option<&str>,
    remote_ctx_bin: &str,
) -> String {
    let data_dir = remote_data_dir
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("~/.ctx");
    format!(
        "if ! command -v lsof >/dev/null 2>&1; then echo 'lsof unavailable on remote host' >&2; exit 127; fi; \
ctx_bin={ctx_bin}; \
data_dir={data_dir}; \
expected_cmd=\"$ctx_bin serve --bind 127.0.0.1:{port} --data-dir $data_dir\"; \
set -- $(lsof -tiTCP:{port} -sTCP:LISTEN 2>/dev/null); \
if [ $# -eq 0 ]; then echo \"remote daemon stop failed (no listener on port {port})\" >&2; exit 1; fi; \
if [ $# -ne 1 ]; then echo \"remote daemon stop failed (expected exactly one listener on port {port}, found $#)\" >&2; exit 1; fi; \
pid=\"$1\"; \
cmdline=\"$(ps -p \"$pid\" -o args= 2>/dev/null || true)\"; \
if [ -z \"$cmdline\" ]; then echo \"remote daemon stop failed (unable to inspect pid $pid on port {port})\" >&2; exit 1; fi; \
case \"$cmdline\" in \
  *\"$expected_cmd\"*) ;; \
  *) echo \"remote daemon stop refused for pid $pid on port {port}: $cmdline\" >&2; exit 1 ;; \
esac; \
kill \"$pid\" >/dev/null 2>&1 || {{ echo \"remote daemon stop failed (kill pid $pid on port {port})\" >&2; exit 1; }}; \
sleep 1",
        port = remote_port,
        ctx_bin = remote_path_expr(remote_ctx_bin),
        data_dir = remote_path_expr(data_dir),
    )
}

pub(crate) fn remote_update_backup_ctx_bin(remote_ctx_bin: &str) -> Result<String> {
    let ctx_bin = validate_remote_ctx_bin(remote_ctx_bin)?;
    Ok(format!("{ctx_bin}.pre-update-backup"))
}

pub(crate) fn remote_backup_ctx_bin_cmd(remote_ctx_bin: &str, backup_ctx_bin: &str) -> String {
    format!(
        "if [ -x {ctx_bin} ]; then cp {ctx_bin} {backup} && chmod 755 {backup}; else echo 'ctx not executable at configured remote path' >&2; exit 127; fi",
        ctx_bin = remote_path_expr(remote_ctx_bin),
        backup = remote_path_expr(backup_ctx_bin),
    )
}

pub(crate) fn remote_restore_ctx_bin_cmd(remote_ctx_bin: &str, backup_ctx_bin: &str) -> String {
    format!(
        "if [ -x {backup} ]; then cp {backup} {ctx_bin} && chmod 755 {ctx_bin}; else echo 'remote daemon backup missing after failed update' >&2; exit 127; fi",
        ctx_bin = remote_path_expr(remote_ctx_bin),
        backup = remote_path_expr(backup_ctx_bin),
    )
}

pub(crate) fn remote_cleanup_backup_ctx_bin_cmd(backup_ctx_bin: &str) -> String {
    format!("rm -f {}", remote_path_expr(backup_ctx_bin))
}

fn backup_remote_ctx_bin_over_ssh(
    host: &str,
    user: Option<&str>,
    remote_ctx_bin: &str,
    backup_ctx_bin: &str,
) -> Result<()> {
    let cmd = remote_backup_ctx_bin_cmd(remote_ctx_bin, backup_ctx_bin);
    let output =
        run_remote_ssh_shell(host, user, &cmd).context("backing up remote daemon binary")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    anyhow::bail!("remote daemon backup failed: {detail}");
}

fn restore_remote_ctx_bin_over_ssh(
    host: &str,
    user: Option<&str>,
    remote_ctx_bin: &str,
    backup_ctx_bin: &str,
) -> Result<()> {
    let cmd = remote_restore_ctx_bin_cmd(remote_ctx_bin, backup_ctx_bin);
    let output =
        run_remote_ssh_shell(host, user, &cmd).context("restoring remote daemon binary")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    anyhow::bail!("remote daemon restore failed: {detail}");
}

fn cleanup_remote_update_backup_over_ssh(
    host: &str,
    user: Option<&str>,
    backup_ctx_bin: &str,
) -> Result<()> {
    let cmd = remote_cleanup_backup_ctx_bin_cmd(backup_ctx_bin);
    let output =
        run_remote_ssh_shell(host, user, &cmd).context("cleaning up remote daemon backup")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    anyhow::bail!("remote daemon backup cleanup failed: {detail}");
}

fn rollback_remote_daemon_update_over_ssh(
    host: &str,
    user: Option<&str>,
    remote_port: u16,
    remote_data_dir: Option<&str>,
    remote_ctx_bin: &str,
    backup_ctx_bin: &str,
    base_url: &str,
    auth_token: &str,
    bundle_synced: bool,
) -> Result<()> {
    let _ = stop_remote_daemon_over_ssh(host, user, remote_port, remote_data_dir, remote_ctx_bin);
    restore_remote_ctx_bin_over_ssh(host, user, remote_ctx_bin, backup_ctx_bin)
        .context("restoring pre-update remote daemon binary")?;
    if bundle_synced {
        restore_remote_bundle_backup_over_ssh(host, user, remote_data_dir)
            .context("restoring pre-update remote bundle metadata")?;
    }
    start_remote_daemon_over_ssh(
        host,
        user,
        remote_port,
        remote_data_dir,
        remote_ctx_bin,
        None,
        None,
    )
    .context("restarting previous remote daemon binary after rollback")?;
    wait_for_remote_daemon_health(base_url, auth_token)
        .context("waiting for rolled back remote daemon health")?;
    if let Err(err) = cleanup_remote_update_backup_over_ssh(host, user, backup_ctx_bin) {
        eprintln!("failed to remove remote update backup {backup_ctx_bin}: {err:#}");
    }
    Ok(())
}

fn wait_for_remote_daemon_health(base_url: &str, auth_token: &str) -> Result<()> {
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..REMOTE_UPDATE_HEALTH_RETRIES {
        match probe_daemon_health_with_auth(base_url, Some(auth_token)) {
            Ok(()) => return Ok(()),
            Err(err) => last_err = Some(err),
        }
        if attempt + 1 < REMOTE_UPDATE_HEALTH_RETRIES {
            std::thread::sleep(std::time::Duration::from_millis(
                REMOTE_UPDATE_HEALTH_DELAY_MS,
            ));
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("requesting /api/health failed")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_self_update_command_threads_channel_and_release_base_url() {
        let cmd = remote_self_update_cmd(
            "~/.ctx/bin/ctx",
            "canary",
            "https://updates.example/functions/v1",
        );
        assert!(cmd.contains("self-update --yes --channel 'canary' --base-url 'https://updates.example/functions/v1'"));
        assert!(cmd.contains("\"$HOME/.ctx/bin/ctx\""));
    }

    #[test]
    fn remote_health_wait_validates_a_protected_route() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_server = std::sync::Arc::clone(&observed);
        let server = std::thread::spawn(move || {
            let health_body =
                "{\"pid\":1,\"data_root\":\"/tmp/test\",\"compatibility\":{\"desktop_exact_version\":\"1.0.0\",\"desktop_build_id\":\"build-a\",\"desktop_dev_instance_id\":\"dev\"}}";
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept request");
                let mut buf = [0_u8; 2048];
                let size = std::io::Read::read(&mut stream, &mut buf).expect("read request");
                let request = String::from_utf8_lossy(&buf[..size]).to_string();
                let request_lower = request.to_ascii_lowercase();
                observed_server
                    .lock()
                    .expect("lock observed requests")
                    .push(request.clone());
                let (status_line, body) = if request.starts_with("GET /api/health ") {
                    ("HTTP/1.1 200 OK", health_body)
                } else if request.starts_with("GET /api/workspaces ")
                    && request_lower.contains("authorization: bearer remote-token")
                {
                    ("HTTP/1.1 200 OK", "[]")
                } else {
                    ("HTTP/1.1 401 Unauthorized", "{\"error\":\"unauthorized\"}")
                };
                let response = format!(
                    "{status_line}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body,
                );
                std::io::Write::write_all(&mut stream, response.as_bytes())
                    .expect("write response");
            }
        });

        let base_url = format!("http://{}", addr);
        wait_for_remote_daemon_health(&base_url, "remote-token")
            .expect("remote daemon health wait succeeds");

        server.join().expect("join test server");
        let requests = observed.lock().expect("lock observed requests");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET /api/health "));
        assert!(requests[1].starts_with("GET /api/workspaces "));
        assert!(requests[1]
            .to_ascii_lowercase()
            .contains("authorization: bearer remote-token"));
    }
}
