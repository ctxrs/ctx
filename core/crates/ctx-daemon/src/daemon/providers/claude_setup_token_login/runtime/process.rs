use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use anyhow::Context;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use tokio::sync::{mpsc, oneshot};

use super::shim::create_claude_browser_open_shim;
use ctx_provider_runtime::provider_login_runtime::ProviderLoginRuntimeCommand;
use output::pump_claude_login_output;

#[path = "process/output.rs"]
mod output;

pub(in crate::daemon::providers::claude_setup_token_login) struct ClaudeLoginSpawn {
    pub(in crate::daemon::providers::claude_setup_token_login) line_rx:
        mpsc::UnboundedReceiver<String>,
    pub(in crate::daemon::providers::claude_setup_token_login) exit_rx:
        oneshot::Receiver<anyhow::Result<portable_pty::ExitStatus>>,
    pub(in crate::daemon::providers::claude_setup_token_login) killer:
        Arc<StdMutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
    pub(in crate::daemon::providers::claude_setup_token_login) browser_open_capture_path: PathBuf,
    pub(in crate::daemon::providers::claude_setup_token_login) browser_open_shim_dir:
        tempfile::TempDir,
}

fn scrub_daemon_auth_env(cmd: &mut CommandBuilder) {
    for key in ctx_core::env::DAEMON_AUTH_ENV_VARS {
        cmd.env_remove(key);
    }
}

pub(in crate::daemon::providers::claude_setup_token_login) fn spawn_claude_setup_token_command(
    runtime: &ProviderLoginRuntimeCommand,
) -> anyhow::Result<ClaudeLoginSpawn> {
    let pty = NativePtySystem::default();
    let pair = pty
        .openpty(PtySize {
            rows: 40,
            cols: 400,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("opening pty for claude setup-token")?;

    let mut cmd = CommandBuilder::new(&runtime.command_abs_path);
    let (browser_open_shim_dir, browser_open_shim_path, browser_open_capture_path) =
        create_claude_browser_open_shim()?;
    for arg in &runtime.args {
        cmd.arg(arg);
    }
    cmd.arg("setup-token");
    scrub_daemon_auth_env(&mut cmd);
    cmd.env("NO_COLOR", "1");
    cmd.env("TERM", "xterm-256color");
    cmd.env("BROWSER", browser_open_shim_path);
    cmd.env(
        "CTX_CLAUDE_AUTH_URL_CAPTURE_PATH",
        &browser_open_capture_path,
    );

    let mut child = pair.slave.spawn_command(cmd).with_context(|| {
        format!(
            "spawning claude setup-token via {}",
            runtime.command_abs_path
        )
    })?;
    let killer = Arc::new(StdMutex::new(child.clone_killer()));
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .context("cloning pty reader for claude setup-token")?;
    let (line_tx, line_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        pump_claude_login_output(reader, line_tx);
    });

    let (exit_tx, exit_rx) = oneshot::channel();
    std::thread::spawn(move || {
        let result = child
            .wait()
            .context("waiting for claude setup-token process");
        let _ = exit_tx.send(result);
    });

    Ok(ClaudeLoginSpawn {
        line_rx,
        exit_rx,
        killer,
        browser_open_capture_path,
        browser_open_shim_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScopedEnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl ScopedEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = &self.previous {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    fn scrub_daemon_auth_env_removes_sensitive_tokens_from_claude_login_command() {
        let _auth = ScopedEnvVar::set("CTX_AUTH_TOKEN", "daemon-token");
        let _mcp = ScopedEnvVar::set("CTX_MCP_TOKEN", "mcp-token");
        let _shutdown = ScopedEnvVar::set("CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN", "shutdown-token");
        let mut cmd = CommandBuilder::new("/bin/sh");

        scrub_daemon_auth_env(&mut cmd);

        for key in ctx_core::env::DAEMON_AUTH_ENV_VARS {
            assert_eq!(cmd.get_env(key), None, "expected {key} to be removed");
        }
    }
}
