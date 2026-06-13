use super::*;

pub(super) fn find_binary_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

pub(super) fn redact_sensitive(input: &str) -> String {
    ctx_core::redaction::redact_sensitive(input)
}

pub fn command_output_message(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    format!("{stderr}\n{stdout}").trim().to_string()
}

pub(super) async fn command_output_with_timeout(
    mut command: Command,
    timeout: Duration,
) -> Result<std::process::Output> {
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.kill_on_drop(true);
    let child = command.spawn().context("spawning command")?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(res) => Ok(res?),
        Err(_) => anyhow::bail!("command timed out after {}s", timeout.as_secs()),
    }
}
