use super::capture::parse_cursor_captured_tokens;
use super::*;
use crate::daemon::providers::login_deps::ProviderLoginDeps;
use crate::daemon::providers::{accounts, login_sessions};
use ctx_provider_runtime::provider_login_runtime::ProviderLoginRuntimeCommand;

mod command;
mod completion;
mod output_loop;
mod progress;
#[path = "session/workspace.rs"]
mod workspace;

use command::spawn_cursor_login_child;
use completion::complete_cursor_login;
use output_loop::collect_cursor_login_output;
use workspace::prepare_cursor_login_workspace;

pub(super) async fn monitor_cursor_login(
    deps: ProviderLoginDeps,
    cursor_runtime: ProviderLoginRuntimeCommand,
    login_id: String,
    label: Option<String>,
) {
    let workspace = match prepare_cursor_login_workspace(deps.data_root(), &login_id).await {
        Ok(workspace) => workspace,
        Err(err) => {
            let login_home = err.login_home().to_path_buf();
            login_sessions::set_cursor_login_error(
                deps.providers(),
                &login_id,
                err.into_status_error(),
            )
            .await;
            let _ = tokio::fs::remove_dir_all(&login_home).await;
            return;
        }
    };

    let mut child = match spawn_cursor_login_child(&cursor_runtime, &workspace) {
        Ok(child) => child,
        Err(err) => {
            login_sessions::set_cursor_login_error(
                deps.providers(),
                &login_id,
                format!("failed to launch cursor-agent login: {err}"),
            )
            .await;
            let _ = tokio::fs::remove_dir_all(&workspace.login_home).await;
            return;
        }
    };

    let output = collect_cursor_login_output(deps.providers(), &login_id, &mut child).await;

    let completion = complete_cursor_login(
        &deps,
        label,
        &workspace.capture_path,
        output.observed_email,
        output.timeout_error,
        output.exit_result,
    )
    .await;

    let _ = tokio::fs::remove_dir_all(&workspace.login_home).await;
    login_sessions::finish_cursor_login_session(
        deps.providers(),
        &login_id,
        completion.status,
        completion.account_id,
        completion.error,
        output.observed_auth_url,
    )
    .await;
}
