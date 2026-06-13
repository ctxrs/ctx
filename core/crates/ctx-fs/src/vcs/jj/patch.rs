use super::*;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug)]
pub(super) enum JjApplyOutcome {
    Applied,
    Unsupported,
}

pub(super) fn jj_command_unsupported(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("unrecognized subcommand")
        || lower.contains("unknown subcommand")
        || lower.contains("unknown command")
}

fn jj_command_usage(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("usage:")
        || lower.contains("required arguments")
        || lower.contains("unexpected argument")
}

pub(super) async fn jj_apply_patch(root: &Path, patch: &str) -> Result<JjApplyOutcome> {
    ensure_jj_usable().await?;
    let output = jj_apply_patch_stdin(root, patch).await?;
    if output.status.success() {
        return Ok(JjApplyOutcome::Applied);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if jj_command_unsupported(&stderr) {
        return Ok(JjApplyOutcome::Unsupported);
    }
    if jj_command_usage(&stderr) {
        return jj_apply_patch_file(root, patch).await;
    }
    bail!("jj apply failed: {}", stderr.trim());
}

async fn jj_apply_patch_stdin(root: &Path, patch: &str) -> Result<std::process::Output> {
    let mut cmd = jj_command(root);
    cmd.arg("apply");
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning jj apply")?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(patch.as_bytes())
            .await
            .context("writing patch to jj apply stdin")?;
    }
    let output = child
        .wait_with_output()
        .await
        .context("waiting for jj apply")?;
    Ok(output)
}

async fn jj_apply_patch_file(root: &Path, patch: &str) -> Result<JjApplyOutcome> {
    let path = temp_patch_path();
    fs::write(&path, patch)
        .await
        .context("writing patch for jj apply")?;
    let output = jj_command(root)
        .arg("apply")
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running jj apply")?;
    let _ = fs::remove_file(&path).await;
    if output.status.success() {
        return Ok(JjApplyOutcome::Applied);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if jj_command_unsupported(&stderr) || jj_command_usage(&stderr) {
        return Ok(JjApplyOutcome::Unsupported);
    }
    bail!("jj apply failed: {}", stderr.trim());
}

fn temp_patch_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("ctx-jj-apply-{nanos}.patch"))
}
