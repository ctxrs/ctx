use super::super::app_server::{fetch_codex_account_details, wait_for_codex_login_completion};
use super::{CodexLoginCompletion, CodexLoginProcess};
use crate::daemon::providers::login_deps::ProviderLoginDeps;
use crate::daemon::providers::{accounts, login_sessions};

pub(in crate::daemon::providers::codex_app_login) async fn monitor_codex_login(
    deps: ProviderLoginDeps,
    account_id: String,
    label: String,
    mut login: CodexLoginProcess,
) {
    let completion = wait_for_codex_login_completion(&mut login.reader, &login.login_id).await;
    let mut status = match completion {
        Ok(completion) => completion,
        Err(err) => CodexLoginCompletion {
            success: false,
            error: Some(err.to_string()),
        },
    };

    if status.success {
        let (email, plan_type) = fetch_codex_account_details(&mut login.stdin, &mut login.reader)
            .await
            .unwrap_or((None, None));
        if let Err(err) = accounts::persist_successful_codex_login(
            deps.data_root(),
            deps.providers(),
            &account_id,
            label,
            email,
            plan_type,
        )
        .await
        {
            status.success = false;
            status.error = Some(err.to_string());
            let _ = tokio::fs::remove_dir_all(&login.account_dir).await;
        }
    } else {
        let _ = tokio::fs::remove_dir_all(&login.account_dir).await;
    }

    login_sessions::finish_codex_login_session(
        deps.providers(),
        &account_id,
        status.success,
        status.error,
    )
    .await;

    let _ = login.child.kill().await;
}
