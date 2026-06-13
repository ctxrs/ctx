use std::process::Stdio;

use tokio::process::{Child, Command};

use super::workspace::CursorLoginWorkspace;
use ctx_provider_runtime::provider_login_runtime::ProviderLoginRuntimeCommand;

fn cursor_login_node_options(workspace: &CursorLoginWorkspace) -> String {
    let hook_require = format!("--require {}", workspace.hook_path.to_string_lossy());
    match std::env::var("NODE_OPTIONS") {
        Ok(existing) if !existing.trim().is_empty() => {
            format!("{} {}", existing.trim(), hook_require)
        }
        _ => hook_require,
    }
}

pub(super) fn spawn_cursor_login_child(
    cursor_runtime: &ProviderLoginRuntimeCommand,
    workspace: &CursorLoginWorkspace,
) -> std::io::Result<Child> {
    let mut cmd = Command::new(&cursor_runtime.command_abs_path);
    for key in ctx_core::env::DAEMON_AUTH_ENV_VARS {
        cmd.env_remove(key);
    }
    for arg in &cursor_runtime.args {
        cmd.arg(arg);
    }
    cmd.arg("login");
    cmd.current_dir(&workspace.workdir);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.env("NO_OPEN_BROWSER", "1");
    cmd.env(
        "CTX_CURSOR_CAPTURE_FILE",
        workspace.capture_path.to_string_lossy().to_string(),
    );
    cmd.env("NODE_OPTIONS", cursor_login_node_options(workspace));
    cmd.spawn()
}
