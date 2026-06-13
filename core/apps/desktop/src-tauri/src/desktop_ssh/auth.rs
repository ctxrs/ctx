use super::*;

pub(super) fn looks_like_ssh_auth_failure(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("permission denied")
        || lowered.contains("publickey")
        || lowered.contains("authentication failed")
        || lowered.contains("too many authentication failures")
}

pub(super) fn bootstrap_ssh_key_auth_with_password(
    host: &str,
    user: Option<&str>,
    password_once: &str,
) -> Result<()> {
    let public_key = ensure_ssh_identity_public_key(host, user)?;
    let install_cmd = ssh_authorized_keys_install_command();
    let key_payload = format!("{public_key}\n");
    let output = run_ssh_shell_with_password_once(
        host,
        user,
        password_once,
        install_cmd,
        Some(key_payload.as_bytes()),
    )
    .context("running password-once SSH bootstrap")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    if detail.is_empty() {
        anyhow::bail!("password-once SSH bootstrap failed");
    }
    anyhow::bail!("password-once SSH bootstrap failed: {detail}");
}

pub(super) fn ssh_authorized_keys_install_command() -> &'static str {
    "umask 077; \
mkdir -p \"$HOME/.ssh\"; \
chmod 700 \"$HOME/.ssh\"; \
touch \"$HOME/.ssh/authorized_keys\"; \
chmod 600 \"$HOME/.ssh/authorized_keys\"; \
key=\"$(cat)\"; \
grep -qxF \"$key\" \"$HOME/.ssh/authorized_keys\" || printf '%s\\n' \"$key\" >> \"$HOME/.ssh/authorized_keys\""
}

pub(super) fn parse_ssh_identity_files_from_expanded_config(stdout: &str) -> Vec<PathBuf> {
    let mut identity_files = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let key = parts.next().unwrap_or("");
        if !key.eq_ignore_ascii_case("identityfile") {
            continue;
        }
        let value = parts.collect::<Vec<_>>().join(" ");
        let Some(path) = normalized_ssh_identity_file(&value) else {
            continue;
        };
        identity_files.push(path);
    }
    identity_files
}

fn normalized_ssh_identity_file(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim().trim_matches(|c| c == '"' || c == '\'');
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.eq_ignore_ascii_case("none") {
        return None;
    }
    let normalized = trimmed.replace("\\ ", " ");
    if let Some(expanded) = expand_tilde(&normalized) {
        return Some(expanded);
    }
    Some(PathBuf::from(normalized))
}

fn resolve_ssh_primary_identity_file(host: &str, user: Option<&str>) -> Result<PathBuf> {
    let target = ssh_target(host, user);
    let output = new_ssh_command()
        .arg("-G")
        .arg(&target)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("resolving SSH config for target {target}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            anyhow::bail!("ssh -G failed for target {target}");
        }
        anyhow::bail!("ssh -G failed for target {target}: {stderr}");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let identity_files = parse_ssh_identity_files_from_expanded_config(&stdout);
    identity_files
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("ssh -G reported no identityfile entries for target {target}"))
}

pub(super) fn private_key_path_for_identity(identity_file: &Path) -> PathBuf {
    let value = identity_file.to_string_lossy();
    if value.ends_with(".pub") {
        return PathBuf::from(value.trim_end_matches(".pub"));
    }
    identity_file.to_path_buf()
}

pub(super) fn public_key_path_for_private_key(private_key_path: &Path) -> PathBuf {
    let mut value = private_key_path.as_os_str().to_os_string();
    value.push(".pub");
    PathBuf::from(value)
}

fn read_ssh_public_key(path: &Path) -> Result<String> {
    let public_key =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let trimmed = public_key.trim();
    if trimmed.is_empty() {
        anyhow::bail!("local SSH public key is empty at {}", path.display());
    }
    Ok(trimmed.to_string())
}

fn ensure_ssh_identity_public_key(host: &str, user: Option<&str>) -> Result<String> {
    let identity_file = resolve_ssh_primary_identity_file(host, user)?;
    let private_key_path = private_key_path_for_identity(&identity_file);
    let public_key_path = public_key_path_for_private_key(&private_key_path);
    let parent = private_key_path
        .parent()
        .ok_or_else(|| anyhow!("invalid SSH identity path: {}", private_key_path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating SSH identity directory {}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("setting SSH identity directory mode {}", parent.display()))?;
    }

    if public_key_path.exists() {
        return read_ssh_public_key(&public_key_path);
    }

    if private_key_path.exists() {
        let derive_output = Command::new("ssh-keygen")
            .arg("-y")
            .arg("-f")
            .arg(&private_key_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| {
                format!(
                    "deriving public key from existing {}",
                    private_key_path.display()
                )
            })?;
        if !derive_output.status.success() {
            let stderr = String::from_utf8_lossy(&derive_output.stderr)
                .trim()
                .to_string();
            if stderr.is_empty() {
                anyhow::bail!(
                    "unable to derive SSH public key for {}",
                    private_key_path.display()
                );
            }
            anyhow::bail!(
                "unable to derive SSH public key for {}: {stderr}",
                private_key_path.display()
            );
        }
        let derived = String::from_utf8_lossy(&derive_output.stdout)
            .trim()
            .to_string();
        if derived.is_empty() {
            anyhow::bail!(
                "derived SSH public key is empty for {}",
                private_key_path.display()
            );
        }
        std::fs::write(&public_key_path, format!("{derived}\n"))
            .with_context(|| format!("writing {}", public_key_path.display()))?;
        return Ok(derived);
    }

    let generate_output = Command::new("ssh-keygen")
        .arg("-t")
        .arg("ed25519")
        .arg("-N")
        .arg("")
        .arg("-f")
        .arg(&private_key_path)
        .arg("-C")
        .arg("ctx-desktop")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("generating SSH identity {}", private_key_path.display()))?;
    if !generate_output.status.success() {
        let stderr = String::from_utf8_lossy(&generate_output.stderr)
            .trim()
            .to_string();
        if stderr.is_empty() {
            anyhow::bail!(
                "unable to generate SSH identity {}",
                private_key_path.display()
            );
        }
        anyhow::bail!(
            "unable to generate SSH identity {}: {stderr}",
            private_key_path.display()
        );
    }
    read_ssh_public_key(&public_key_path)
}

fn write_ssh_askpass_script() -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!("ctx-ssh-askpass-{}.sh", uuid::Uuid::new_v4()));
    std::fs::write(
        &path,
        "#!/bin/sh\nprintf '%s\\n' \"$CTX_SSH_PASSWORD_ONCE\"\n",
    )
    .with_context(|| format!("writing SSH askpass helper at {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("setting SSH askpass helper mode at {}", path.display()))?;
    }
    Ok(path)
}

fn run_ssh_shell_with_password_once(
    host: &str,
    user: Option<&str>,
    password_once: &str,
    cmd: &str,
    stdin_payload: Option<&[u8]>,
) -> Result<std::process::Output> {
    let target = ssh_target(host, user);
    let remote_cmd = format!("sh -lc {}", shell_escape(cmd));
    let askpass_script = write_ssh_askpass_script()?;
    let mut command = new_ssh_command();
    command
        .arg("-o")
        .arg("BatchMode=no")
        .arg("-o")
        .arg("ConnectTimeout=15")
        .arg("-o")
        .arg("ConnectionAttempts=1")
        .arg("-o")
        .arg("NumberOfPasswordPrompts=1")
        .arg("-o")
        .arg("PreferredAuthentications=password,keyboard-interactive")
        .arg("-o")
        .arg("PasswordAuthentication=yes")
        .arg("-o")
        .arg("KbdInteractiveAuthentication=yes")
        .arg("-o")
        .arg("PubkeyAuthentication=no")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg(target)
        .arg(remote_cmd)
        .env("SSH_ASKPASS", &askpass_script)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", "ctx-desktop")
        .env("CTX_SSH_PASSWORD_ONCE", password_once)
        .stdin(if stdin_payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = (|| -> Result<std::process::Output> {
        let mut child = command
            .spawn()
            .context("running ssh with password-once credentials")?;
        if let Some(payload) = stdin_payload {
            let Some(mut stdin) = child.stdin.take() else {
                return Err(reap_password_once_child_after_input_error(
                    child,
                    "ssh stdin unavailable for password-once command",
                ));
            };
            if let Err(err) = stdin.write_all(payload) {
                drop(stdin);
                return Err(reap_password_once_child_after_input_error(
                    child,
                    &format!("writing password-once SSH stdin payload: {err}"),
                ));
            }
        }
        child
            .wait_with_output()
            .context("waiting for password-once SSH command")
    })();
    let _ = std::fs::remove_file(&askpass_script);
    output
}

fn format_password_once_output_detail(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(super) fn reap_password_once_child_after_input_error(
    child: Child,
    reason: &str,
) -> anyhow::Error {
    match child.wait_with_output() {
        Ok(output) => {
            let detail = format_password_once_output_detail(&output);
            if detail.is_empty() {
                anyhow!(
                    "{reason}; password-once ssh exited with status {}",
                    output.status
                )
            } else {
                anyhow!(
                    "{reason}; password-once ssh exited with status {}: {detail}",
                    output.status
                )
            }
        }
        Err(wait_err) => anyhow!(
            "{reason}; additionally failed waiting for password-once SSH command: {wait_err}"
        ),
    }
}
