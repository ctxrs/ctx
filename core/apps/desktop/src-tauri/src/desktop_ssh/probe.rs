use super::*;

fn normalize_remote_arch_token(raw: &str) -> Option<&'static str> {
    match raw.trim() {
        "x86_64" | "amd64" => Some("x86_64"),
        "aarch64" | "arm64" => Some("aarch64"),
        _ => None,
    }
}

fn is_windows_os_token(raw: &str) -> bool {
    let lowered = raw.trim().to_ascii_lowercase();
    lowered.contains("windows")
        || lowered.contains("mingw")
        || lowered.contains("msys")
        || lowered.contains("cygwin")
}

fn looks_like_windows_shell_error(raw: &str) -> bool {
    let lowered = raw.to_ascii_lowercase();
    lowered.contains("is not recognized as an internal or external command")
        || lowered.contains("'sh' is not recognized")
        || lowered.contains("'uname' is not recognized")
        || lowered.contains("cmd.exe")
        || lowered.contains("powershell")
}

pub(super) fn probe_remote_linux_platform_with_optional_password(
    host: &str,
    user: Option<&str>,
    password_once: Option<&str>,
) -> Result<(RemoteLinuxPlatform, RemoteAuthBootstrap)> {
    match probe_remote_linux_platform(host, user) {
        Ok(platform) => Ok((platform, RemoteAuthBootstrap::None)),
        Err(err) => {
            let Some(password_once) = password_once else {
                return Err(err.context(
                    "key-based SSH probe failed and no one-time password was provided for bootstrap retry",
                ));
            };
            let err_text = err.to_string();
            if err_text.contains(WINDOWS_REMOTE_UNSUPPORTED_MSG) {
                return Err(err.context("platform probe failed on unsupported windows host"));
            }
            let probe_kind = if looks_like_ssh_auth_failure(&err_text) {
                "auth"
            } else {
                "probe"
            };
            bootstrap_ssh_key_auth_with_password(host, user, password_once).with_context(|| {
                format!("after initial {probe_kind} SSH probe failed: {err_text}")
            })?;
            Ok((
                probe_remote_linux_platform(host, user)?,
                RemoteAuthBootstrap::PasswordOncePubkeyInstall,
            ))
        }
    }
}

pub(super) fn probe_remote_linux_platform(
    host: &str,
    user: Option<&str>,
) -> Result<RemoteLinuxPlatform> {
    let target = ssh_target(host, user);
    let probe_cmd = format!(
        "printf '{}%s\\n' \"$(uname -s 2>/dev/null || true)\"; printf '{}%s\\n' \"$(uname -m 2>/dev/null || true)\"",
        PLATFORM_PROBE_OS_MARKER, PLATFORM_PROBE_ARCH_MARKER,
    );
    let remote_probe_cmd = format!("sh -lc {}", shell_escape(&probe_cmd));
    let output = new_ssh_command()
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=8")
        .arg("-o")
        .arg("ConnectionAttempts=1")
        .arg("-o")
        .arg("ServerAliveInterval=5")
        .arg("-o")
        .arg("ServerAliveCountMax=1")
        .arg(&target)
        .arg(remote_probe_cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("probing remote platform over ssh")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if looks_like_windows_shell_error(&stderr) || detect_windows_remote_with_cmd(&target) {
            anyhow::bail!(WINDOWS_REMOTE_UNSUPPORTED_MSG);
        }
        if stderr.is_empty() {
            anyhow::bail!("ssh failed to probe remote platform");
        }
        anyhow::bail!("ssh failed to probe remote platform: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some((os, arch_raw)) = parse_remote_platform_probe_stdout(&stdout) else {
        anyhow::bail!("ssh probe returned incomplete platform details");
    };
    if is_windows_os_token(&os) {
        anyhow::bail!(WINDOWS_REMOTE_UNSUPPORTED_MSG);
    }
    if os != "Linux" {
        anyhow::bail!("Unsupported remote OS `{os}`. Use a Linux host (x86_64 or arm64).");
    }
    let Some(arch) = normalize_remote_arch_token(&arch_raw) else {
        anyhow::bail!("Unsupported remote architecture `{arch_raw}`. Use Linux x86_64 or arm64.");
    };
    Ok(RemoteLinuxPlatform { arch })
}

pub(super) fn parse_remote_platform_probe_stdout(stdout: &str) -> Option<(String, String)> {
    let mut os: Option<String> = None;
    let mut arch: Option<String> = None;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix(PLATFORM_PROBE_OS_MARKER) {
            let value = value.trim();
            if !value.is_empty() {
                os = Some(value.to_string());
            }
            continue;
        }
        if let Some(value) = trimmed.strip_prefix(PLATFORM_PROBE_ARCH_MARKER) {
            let value = value.trim();
            if !value.is_empty() {
                arch = Some(value.to_string());
            }
        }
    }
    match (os, arch) {
        (Some(os), Some(arch)) => Some((os, arch)),
        _ => None,
    }
}

fn detect_windows_remote_with_cmd(target: &str) -> bool {
    let output = new_ssh_command()
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=8")
        .arg("-o")
        .arg("ConnectionAttempts=1")
        .arg("-o")
        .arg("ServerAliveInterval=5")
        .arg("-o")
        .arg("ServerAliveCountMax=1")
        .arg(target)
        .arg("cmd /c ver")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let combined = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    combined.to_ascii_lowercase().contains("windows")
}
