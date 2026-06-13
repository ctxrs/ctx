use super::super::*;

pub(in crate::daemon::providers::claude_setup_token_login) async fn kill_claude_login_process(
    killer: Arc<StdMutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || {
        let mut guard = killer
            .lock()
            .map_err(|_| anyhow::anyhow!("claude setup-token killer lock poisoned"))?;
        guard.kill().context("killing claude setup-token process")
    })
    .await
    .context("joining claude setup-token kill task")?
}

pub(super) async fn terminate_claude_login_after_error(
    killer: Arc<StdMutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
    exit_rx: &mut oneshot::Receiver<anyhow::Result<portable_pty::ExitStatus>>,
    terminal_error: &mut Option<String>,
    exit_result: &mut Option<anyhow::Result<portable_pty::ExitStatus>>,
) {
    if let Err(err) = kill_claude_login_process(killer).await {
        let suffix = format!("; failed to terminate setup-token process cleanly: {err}");
        *terminal_error = Some(match terminal_error.take() {
            Some(base) => format!("{base}{suffix}"),
            None => suffix,
        });
    }
    if exit_result.is_none() {
        if let Ok(exit) = tokio::time::timeout(CLAUDE_LOGIN_EXIT_GRACE_WAIT, exit_rx).await {
            *exit_result = Some(match exit {
                Ok(result) => result,
                Err(err) => Err(anyhow::anyhow!(
                    "claude setup-token exit channel closed: {err}"
                )),
            });
        }
    }
}
