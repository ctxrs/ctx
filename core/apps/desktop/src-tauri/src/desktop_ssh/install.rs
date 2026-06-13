use super::*;

#[path = "install/artifacts.rs"]
mod artifacts;

pub(super) use artifacts::*;
fn render_remote_verified_download_fragment(
    artifact: &ResolvedRemoteReleaseArtifact,
    remote_tmp_path: &str,
    make_executable: bool,
    error_label: &str,
) -> String {
    let chmod_cmd = if make_executable {
        "chmod 755 \"$download_tmp\"; "
    } else {
        ""
    };
    format!(
        "download_url={url}; \
download_sha={sha}; \
download_tmp={tmp}; \
if ! command -v sha256sum >/dev/null 2>&1; then echo 'remote host missing sha256sum required for {label} verification' >&2; exit 127; fi; \
rm -f \"$download_tmp\"; \
if command -v curl >/dev/null 2>&1; then curl -fL --retry 3 --connect-timeout 20 --max-time 600 -o \"$download_tmp\" \"$download_url\"; \
elif command -v wget >/dev/null 2>&1; then wget -q -O \"$download_tmp\" \"$download_url\"; \
else echo 'remote host missing curl or wget required to download {label}' >&2; exit 127; fi; \
printf '%s  %s\\n' \"$download_sha\" \"$download_tmp\" | sha256sum -c - >/dev/null; \
{chmod_cmd}",
        url = shell_escape(&artifact.url),
        sha = shell_escape(&artifact.sha256),
        tmp = remote_path_expr(remote_tmp_path),
        label = error_label,
        chmod_cmd = chmod_cmd,
    )
}

fn render_remote_daemon_artifact_validation_cmd(remote_tmp_path: &str) -> String {
    format!(
        "if command -v timeout >/dev/null 2>&1; then validator='timeout 20s'; else validator=''; fi; \
if ! $validator {tmp} serve --help >/dev/null 2>&1; then echo 'managed remote daemon artifact is not a headless ctx daemon: serve --help failed' >&2; exit 126; fi;",
        tmp = remote_path_expr(remote_tmp_path),
    )
}

fn render_remote_ctx_bin_usable_check_cmd(remote_ctx_bin: &str) -> Result<String> {
    let ctx_bin = validate_remote_ctx_bin(remote_ctx_bin)?;
    Ok(format!(
        "if [ ! -x {ctx_bin} ]; then exit 1; fi; \
if command -v timeout >/dev/null 2>&1; then validator='timeout 20s'; else validator=''; fi; \
if ! $validator {ctx_bin} serve --help >/dev/null 2>&1; then echo 'remote managed ctx binary is not a headless daemon: serve --help failed' >&2; exit 1; fi",
        ctx_bin = remote_path_expr(&ctx_bin),
    ))
}

fn render_remote_bundle_sync_cmd(
    artifact: &ResolvedRemoteReleaseArtifact,
    remote_data_dir: &str,
    remote_bundle_dir: &str,
    remote_bundle_backup_dir: &str,
    remote_tmp_root: &str,
    remote_appimage_path: &str,
    remote_extract_root: &str,
    remote_staged_bundle_dir: &str,
) -> String {
    format!(
        "set -eu; \
	tmp_root={tmp_root}; \
	appimage={appimage}; \
	extract_root={extract_root}; \
staged_bundle={staged_bundle}; \
dest={dest}; \
backup={backup}; \
no_previous_marker=\"$backup/.ctx-no-previous-bundle\"; \
cleanup() {{ rm -rf \"$tmp_root\"; }}; \
	trap cleanup EXIT INT TERM; \
	mkdir -p {data_dir}; \
	rm -rf \"$tmp_root\"; \
	mkdir -p \"$extract_root\" \"$staged_bundle\"; \
	{download_appimage}\
	(cd \"$extract_root\" && \"$appimage\" --appimage-extract >/dev/null 2>&1); \
	bundle_src=$(find \"$extract_root\"/squashfs-root -type d -path '*/bundles' -print -quit); \
if [ -z \"$bundle_src\" ]; then echo 'managed remote desktop artifact missing bundles directory' >&2; exit 1; fi; \
cp -R \"$bundle_src\"/. \"$staged_bundle\"/; \
rm -rf \"$backup\"; \
if [ -e \"$dest\" ]; then mv \"$dest\" \"$backup\"; else mkdir -p \"$backup\"; touch \"$no_previous_marker\"; fi; \
if mv \"$staged_bundle\" \"$dest\"; then :; else status=$?; if [ -e \"$no_previous_marker\" ]; then rm -rf \"$dest\" \"$backup\"; elif [ -e \"$backup\" ]; then rm -rf \"$dest\"; mv \"$backup\" \"$dest\"; fi; exit \"$status\"; fi;",
        tmp_root = remote_path_expr(remote_tmp_root),
        appimage = remote_path_expr(remote_appimage_path),
        extract_root = remote_path_expr(remote_extract_root),
        staged_bundle = remote_path_expr(remote_staged_bundle_dir),
        data_dir = remote_path_expr(remote_data_dir),
        dest = remote_path_expr(remote_bundle_dir),
        backup = remote_path_expr(remote_bundle_backup_dir),
        download_appimage =
            render_remote_verified_download_fragment(artifact, remote_appimage_path, true, "managed remote bundle"),
    )
}

pub(super) fn install_remote_daemon_over_ssh(
    _app: &tauri::AppHandle,
    host: &str,
    user: Option<&str>,
    remote_platform: RemoteLinuxPlatform,
    remote_ctx_bin: &str,
    channel: &str,
) -> Result<()> {
    let target = ssh_target(host, user);
    let artifact = resolve_managed_remote_daemon_artifact(remote_platform.arch, channel)?;
    let parent_dir = remote_ctx_bin_parent_dir(remote_ctx_bin)?;
    let remote_ctx_bin = validate_remote_ctx_bin(remote_ctx_bin)?;
    let temp_remote_path = format!("{remote_ctx_bin}.tmp-{}", std::process::id());
    let download_cmd = render_remote_verified_download_fragment(
        &artifact,
        &temp_remote_path,
        true,
        "managed remote daemon",
    );
    let validate_cmd = render_remote_daemon_artifact_validation_cmd(&temp_remote_path);
    let install_cmd = format!(
        "set -eu; tmp={tmp}; cleanup() {{ rm -f \"$tmp\"; }}; trap cleanup EXIT INT TERM; mkdir -p {parent}; {download_cmd}{validate_cmd}mv -f \"$tmp\" {dest}",
        parent = remote_path_expr(&parent_dir),
        tmp = remote_path_expr(&temp_remote_path),
        dest = remote_path_expr(&remote_ctx_bin),
        download_cmd = download_cmd,
        validate_cmd = validate_cmd,
    );
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
        .arg(format!("sh -lc {}", shell_escape(&install_cmd)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("waiting for remote daemon install ssh command")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            anyhow::bail!("remote daemon install failed");
        }
        anyhow::bail!("remote daemon install failed: {stderr}");
    }
    Ok(())
}

pub(super) fn sync_remote_bundle_metadata_over_ssh(
    _app: &tauri::AppHandle,
    host: &str,
    user: Option<&str>,
    remote_data_dir: Option<&str>,
    remote_arch: &str,
    channel: &str,
) -> Result<()> {
    let artifact = resolve_managed_remote_bundle_appimage_artifact(remote_arch, channel)?;
    let data_dir = remote_data_dir
        .filter(|d| !d.trim().is_empty())
        .unwrap_or("~/.ctx");
    let remote_bundle_dir = remote_bundle_dir_for_data_dir(data_dir);
    let remote_bundle_backup_dir = remote_bundle_backup_dir_for_data_dir(data_dir);
    let remote_tmp_root = join_remote_path(
        data_dir,
        &format!(".bundle-sync.tmp-{}", std::process::id()),
    );
    let remote_appimage_path = join_remote_path(&remote_tmp_root, "ctx.AppImage");
    let remote_extract_root = join_remote_path(&remote_tmp_root, "extract");
    let remote_staged_bundle_dir = join_remote_path(&remote_tmp_root, "bundles");
    let target = ssh_target(host, user);
    let remote_cmd = render_remote_bundle_sync_cmd(
        &artifact,
        data_dir,
        &remote_bundle_dir,
        &remote_bundle_backup_dir,
        &remote_tmp_root,
        &remote_appimage_path,
        &remote_extract_root,
        &remote_staged_bundle_dir,
    );
    let ssh_output = new_ssh_command()
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=15")
        .arg("-o")
        .arg("ServerAliveInterval=15")
        .arg("-o")
        .arg("ServerAliveCountMax=2")
        .arg(target)
        .arg(format!("sh -lc {}", shell_escape(&remote_cmd)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("waiting for remote bundle metadata ssh command")?;
    if !ssh_output.status.success() {
        let stderr = String::from_utf8_lossy(&ssh_output.stderr)
            .trim()
            .to_string();
        anyhow::bail!("remote bundle metadata sync failed: {stderr}");
    }
    Ok(())
}

pub(super) fn restore_remote_bundle_backup_over_ssh(
    host: &str,
    user: Option<&str>,
    remote_data_dir: Option<&str>,
) -> Result<()> {
    let data_dir = remote_data_dir
        .filter(|d| !d.trim().is_empty())
        .unwrap_or("~/.ctx");
    let bundle_dir = remote_bundle_dir_for_data_dir(data_dir);
    let backup_dir = remote_bundle_backup_dir_for_data_dir(data_dir);
    let cmd = format!(
        "bundle={bundle}; backup={backup}; no_previous_marker=\"$backup/.ctx-no-previous-bundle\"; \
if [ -e \"$no_previous_marker\" ]; then rm -rf \"$bundle\" \"$backup\"; \
elif [ -e \"$backup\" ]; then rm -rf \"$bundle\" && mv \"$backup\" \"$bundle\"; \
else echo 'remote bundle backup missing after failed update' >&2; exit 127; fi",
        bundle = remote_path_expr(&bundle_dir),
        backup = remote_path_expr(&backup_dir),
    );
    let output =
        run_remote_ssh_shell(host, user, &cmd).context("restoring remote bundle backup")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    anyhow::bail!("remote bundle restore failed: {detail}");
}

pub(super) fn cleanup_remote_bundle_backup_over_ssh(
    host: &str,
    user: Option<&str>,
    remote_data_dir: Option<&str>,
) -> Result<()> {
    let data_dir = remote_data_dir
        .filter(|d| !d.trim().is_empty())
        .unwrap_or("~/.ctx");
    let backup_dir = remote_bundle_backup_dir_for_data_dir(data_dir);
    let cmd = format!("rm -rf {}", remote_path_expr(&backup_dir));
    let output =
        run_remote_ssh_shell(host, user, &cmd).context("cleaning up remote bundle backup")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    anyhow::bail!("remote bundle backup cleanup failed: {detail}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_bundle_artifact_prefers_appimage_then_desktop() {
        let manifest: ReleaseManifest = serde_json::from_str(
            r#"{
              "platforms": {
                "linux-x64": {
                  "desktop": { "url_path": "/desktop", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
                  "appimage": { "url_path": "/appimage", "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }
                },
                "linux-arm64": {
                  "desktop": { "url_path": "/desktop-arm", "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc" }
                }
              }
            }"#,
        )
        .expect("parse manifest");

        let x64 = manifest.platforms.get("linux-x64").expect("x64 entry");
        let arm64 = manifest.platforms.get("linux-arm64").expect("arm64 entry");
        assert_eq!(
            release_bundle_artifact_for_platform(x64)
                .expect("x64 bundle artifact")
                .url_path,
            "/appimage"
        );
        assert_eq!(
            release_bundle_artifact_for_platform(arm64)
                .expect("arm64 bundle artifact")
                .url_path,
            "/desktop-arm"
        );
    }

    #[test]
    fn remote_bundle_sync_command_extracts_appimage_on_remote() {
        let artifact = ResolvedRemoteReleaseArtifact {
            url: "https://api.ctx.rs/functions/v1/download/stable/1.2.3/ctx.AppImage".to_string(),
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        };
        let cmd = render_remote_bundle_sync_cmd(
            &artifact,
            "~/.ctx",
            "~/.ctx/bundles",
            "~/.ctx/bundles.pre-update-backup",
            "~/.ctx/.bundle-sync.tmp-99",
            "~/.ctx/.bundle-sync.tmp-99/ctx.AppImage",
            "~/.ctx/.bundle-sync.tmp-99/extract",
            "~/.ctx/.bundle-sync.tmp-99/bundles",
        );
        assert!(cmd.contains("--appimage-extract"));
        assert!(cmd.contains("find \"$extract_root\"/squashfs-root -type d -path '*/bundles'"));
        assert!(cmd.contains("managed remote desktop artifact missing bundles directory"));
        assert!(cmd.contains("mv \"$staged_bundle\" \"$dest\""));
        assert!(cmd.contains("backup=\"$HOME/.ctx/bundles.pre-update-backup\""));
        assert!(cmd.contains("touch \"$no_previous_marker\""));
        assert!(
            cmd.contains("if [ -e \"$no_previous_marker\" ]; then rm -rf \"$dest\" \"$backup\";")
        );
        assert!(!cmd.contains("rm -rf \"$HOME/.ctx/bundles\""));
        assert!(cmd.contains("curl -fL --retry 3"));
        assert!(cmd.contains("wget -q -O"));
        assert!(cmd.contains("sha256sum -c -"));
        assert!(cmd.contains(
            "download_url='https://api.ctx.rs/functions/v1/download/stable/1.2.3/ctx.AppImage'"
        ));
        assert!(!cmd.contains("cat > \"$appimage\""));
        assert!(!cmd.contains("desktop_bundle_dir"));
    }

    #[test]
    fn remote_daemon_install_command_validates_headless_daemon() {
        let cmd = render_remote_daemon_artifact_validation_cmd("~/.ctx/bin/ctx.tmp-42");
        assert!(cmd.contains("serve --help"));
        assert!(cmd.contains("timeout 20s"));
        assert!(cmd.contains("managed remote daemon artifact is not a headless ctx daemon"));
    }

    #[test]
    fn remote_existing_managed_binary_check_requires_headless_daemon() {
        let cmd =
            render_remote_ctx_bin_usable_check_cmd("~/.ctx/bin/ctx").expect("valid remote path");
        assert!(cmd.contains("if [ ! -x \"$HOME/.ctx/bin/ctx\" ]; then exit 1; fi"));
        assert!(cmd.contains("timeout 20s"));
        assert!(cmd.contains("\"$HOME/.ctx/bin/ctx\" serve --help"));
        assert!(cmd.contains("remote managed ctx binary is not a headless daemon"));
    }
}

pub(super) fn remote_ctx_bin_exists_over_ssh(
    host: &str,
    user: Option<&str>,
    remote_ctx_bin: &str,
) -> Result<bool> {
    let ctx_bin = validate_remote_ctx_bin(remote_ctx_bin)?;
    let check_cmd = format!(
        "if [ -x {ctx_bin} ]; then exit 0; else exit 1; fi",
        ctx_bin = remote_path_expr(&ctx_bin),
    );
    let output = run_remote_ssh_shell(host, user, &check_cmd)
        .context("checking remote managed daemon binary")?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    anyhow::bail!("checking remote managed daemon binary failed: {detail}");
}

pub(super) fn remote_ctx_bin_usable_over_ssh(
    host: &str,
    user: Option<&str>,
    remote_ctx_bin: &str,
) -> Result<bool> {
    let check_cmd = render_remote_ctx_bin_usable_check_cmd(remote_ctx_bin)?;
    let output = run_remote_ssh_shell(host, user, &check_cmd)
        .context("validating remote managed daemon binary")?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    anyhow::bail!("validating remote managed daemon binary failed: {detail}");
}

pub(super) fn start_remote_daemon_over_ssh(
    host: &str,
    user: Option<&str>,
    remote_port: u16,
    remote_data_dir: Option<&str>,
    remote_ctx_bin: &str,
    update_channel: Option<&str>,
    update_base_url: Option<&str>,
) -> Result<()> {
    let target = ssh_target(host, user);
    let data_dir = remote_data_dir
        .filter(|d| !d.trim().is_empty())
        .unwrap_or("~/.ctx");
    let bundle_dir = remote_bundle_dir_for_data_dir(data_dir);
    let log_dir = format!("{}/logs", data_dir.trim_end_matches('/'));
    let log_dir_expr = remote_path_expr(&log_dir);
    let log_file = format!("{}/daemon.log", log_dir.trim_end_matches('/'));
    let log_file_expr = remote_path_expr(&log_file);
    let ctx_bin = validate_remote_ctx_bin(remote_ctx_bin)?;
    let exec_cmd = render_remote_daemon_exec_cmd(
        &ctx_bin,
        remote_port,
        data_dir,
        &bundle_dir,
        update_channel,
        update_base_url,
    )?;
    let log_cmd = format!(
        "mkdir -p {log_dir} && {exec_cmd} > {log_file} 2>&1",
        log_dir = log_dir_expr,
        log_file = log_file_expr,
    );
    let remote_cmd = format!(
        "mkdir -p {log_dir} && nohup /bin/sh -lc {cmd} >/dev/null 2>&1 < /dev/null &",
        log_dir = log_dir_expr,
        cmd = shell_escape(&log_cmd),
    );

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
        .arg(format!("sh -lc {}", shell_escape(&remote_cmd)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("starting remote daemon over ssh")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "ssh start failed: {stderr}; remote_cmd={remote_cmd}"
        ));
    }
    Ok(())
}

pub(super) fn render_remote_daemon_exec_cmd(
    remote_ctx_bin: &str,
    remote_port: u16,
    remote_data_dir: &str,
    remote_bundle_dir: &str,
    update_channel: Option<&str>,
    update_base_url: Option<&str>,
) -> Result<String> {
    let ctx_bin = validate_remote_ctx_bin(remote_ctx_bin)?;
    let ctx_bin_expr = remote_path_expr(&ctx_bin);
    let linux_sandbox_env_prefix = remote_linux_sandbox_daemon_env_prefix(remote_data_dir);
    let update_env = match (
        update_channel.filter(|value| !value.trim().is_empty()),
        update_base_url.filter(|value| !value.trim().is_empty()),
    ) {
        (Some(channel), Some(base_url)) => format!(
            "CTX_MANAGED_DAEMON_AUTO_UPDATE=1 CTX_DAEMON_UPDATE_CHANNEL={channel} CTX_DAEMON_UPDATE_BASE_URL={base_url} ",
            channel = shell_escape(channel),
            base_url = shell_escape(base_url),
        ),
        _ => String::new(),
    };
    Ok(format!(
        "{linux_sandbox_env_prefix} if [ -x {ctx_bin} ]; then CTX_BUNDLE_DIR={bundle_dir} {update_env}{ctx_bin} serve --bind 127.0.0.1:{remote_port} --data-dir {dir}; else echo 'ctx not executable at configured remote path' >&2; exit 127; fi",
        linux_sandbox_env_prefix = linux_sandbox_env_prefix,
        ctx_bin = ctx_bin_expr,
        bundle_dir = remote_path_expr(remote_bundle_dir),
        update_env = update_env,
        dir = remote_path_expr(remote_data_dir),
    ))
}
