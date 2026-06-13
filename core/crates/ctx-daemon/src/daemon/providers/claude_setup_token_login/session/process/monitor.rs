use super::*;

#[path = "monitor/finalization.rs"]
mod finalization;
#[path = "monitor/output.rs"]
mod output;
#[path = "monitor/termination.rs"]
mod termination;
#[path = "monitor/wait_loop.rs"]
mod wait_loop;

use finalization::finalize_claude_login;
pub(in crate::daemon::providers::claude_setup_token_login) use termination::kill_claude_login_process;
use termination::terminate_claude_login_after_error;
use wait_loop::wait_for_claude_login_observation;

use crate::daemon::providers::login_deps::ProviderLoginDeps;
use std::sync::Arc;

pub(in crate::daemon::providers::claude_setup_token_login) async fn monitor_claude_login(
    deps: ProviderLoginDeps,
    login_id: String,
    label: Option<String>,
    mut login: ClaudeLoginProcess,
) {
    let mut observation =
        wait_for_claude_login_observation(deps.providers(), &login_id, &mut login).await;

    if observation.terminal_error.is_some() {
        terminate_claude_login_after_error(
            Arc::clone(&login.killer),
            &mut login.exit_rx,
            &mut observation.terminal_error,
            &mut observation.exit_result,
        )
        .await;
    }

    finalize_claude_login(
        &deps,
        &login_id,
        label,
        observation.observed_auth_url,
        observation.terminal_error,
        observation.exit_result,
        &observation.transcript,
    )
    .await;
}
