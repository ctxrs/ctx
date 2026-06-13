use super::*;

const STAGING_FAILURE_RETRY_DELAY_MS: u64 = 15 * 60 * 1000;

use tauri_plugin_updater::UpdaterExt;

pub(super) fn should_bypass_remote_bootstrap_freshness_check(channel: &str) -> bool {
    support::resolve_native_updater_config(channel)
        .ok()
        .map(|config| {
            support::should_allow_remote_bootstrap_insecure_loopback_updater(
                support::remote_bootstrap_insecure_loopback_override_enabled(),
                &config.endpoint,
            )
        })
        .unwrap_or(false)
}

pub(super) async fn ensure_desktop_app_current_for_remote_bootstrap_impl(
    app: &tauri::AppHandle,
    channel: &str,
) -> Result<(), String> {
    if should_bypass_remote_bootstrap_freshness_check(channel) {
        // EXCEPTION: desktop remote-bootstrap automation uses a loopback HTTP release fixture to
        // prove the real UI/bootstrap/sandbox flow without depending on a public HTTPS updater
        // feed. Skip the native updater check entirely here so automation never touches the
        // platform updater plugin on this loopback path.
        return Ok(());
    }
    let state = match resolve_desktop_update_state(app, channel).await {
        Ok(state) => state,
        Err(err) => {
            return Err(format!(
                "{REMOTE_BOOTSTRAP_FRESHNESS_UNVERIFIED_PREFIX} {err} Update the desktop app, then try again."
            ));
        }
    };
    validate_remote_bootstrap_desktop_freshness(&state, channel)
}

pub(super) fn validate_remote_bootstrap_desktop_freshness(
    state: &DesktopAppUpdateStateResp,
    channel: &str,
) -> Result<(), String> {
    if !state.configured {
        let detail = state
            .last_error
            .as_deref()
            .and_then(support::normalize_nonempty)
            .or_else(|| {
                state
                    .message
                    .as_deref()
                    .and_then(support::normalize_nonempty)
            })
            .unwrap_or_else(|| {
                "Native updater is not configured for this desktop build.".to_string()
            });
        return Err(format!(
            "{REMOTE_BOOTSTRAP_FRESHNESS_UNVERIFIED_PREFIX} {detail} Install or update the desktop app for channel `{channel}`, then try again."
        ));
    }

    if state.restart_required {
        let target = state
            .latest_version
            .as_deref()
            .and_then(support::normalize_nonempty)
            .unwrap_or_else(|| "the staged update".to_string());
        return Err(format!(
            "{REMOTE_BOOTSTRAP_UPDATE_REQUIRED_PREFIX} Restart ctx to finish applying desktop version `{target}`, then try again."
        ));
    }

    let latest = state
        .latest_version
        .as_deref()
        .and_then(support::normalize_nonempty);
    if let Some(latest) = latest {
        return Err(format!(
            "{REMOTE_BOOTSTRAP_UPDATE_REQUIRED_PREFIX} Current desktop version `{}` is stale for channel `{channel}`; latest is `{latest}`. Use the desktop updater banner, then try again.",
            state.current_version
        ));
    }

    Ok(())
}

pub(super) async fn resolve_desktop_update_state(
    app: &tauri::AppHandle,
    channel: &str,
) -> Result<DesktopAppUpdateStateResp, String> {
    let current_version = app.package_info().version.to_string();
    let config = support::resolve_native_updater_config(channel)?;
    staged::clear_staged_update_if_current_version_is_new_enough(app, &current_version)?;
    let pending_restart_version = restart::reconcile_restart_marker_for_app(app, &current_version)?;
    let restart_required = pending_restart_version.is_some();
    let last_attempt = attempts::read_last_attempt_for_app(app)?;
    let last_attempt_id = last_attempt.as_ref().map(|entry| entry.attempt_id.clone());
    let mut last_error = last_attempt
        .as_ref()
        .and_then(attempts::last_failed_stage_message)
        .map(|value| value.to_string());
    let message = if config.pubkey.is_none() {
        Some(support::MISSING_EMBEDDED_UPDATER_PUBKEY_MESSAGE_SENTENCE.to_string())
    } else {
        None
    };

    if restart_required {
        return Ok(transaction::restart_required_state(
            &config,
            current_version,
            pending_restart_version,
            last_attempt_id,
            last_error,
        ));
    }

    #[cfg(target_os = "windows")]
    {
        return Ok(transaction::unconfigured_state(
            &config,
            current_version,
            pending_restart_version,
            Some("Desktop background update apply is not supported on Windows yet.".to_string()),
            last_attempt_id,
            last_error,
        ));
    }

    let Some(pubkey) = config.pubkey.as_deref() else {
        return Ok(transaction::unconfigured_state(
            &config,
            current_version,
            pending_restart_version,
            message,
            last_attempt_id,
            last_error,
        ));
    };

    let endpoint_url = support::endpoint_with_download_id(&config.endpoint, None)?;
    let updater = app
        .updater_builder()
        .target(config.target.clone())
        .pubkey(pubkey)
        .endpoints(vec![endpoint_url])
        .map_err(to_err)?
        .build()
        .map_err(to_err)?;
    let update = updater
        .check()
        .await
        .map_err(|e| support::updater_stage_error("check", e))?;
    let raw_latest_version = update.as_ref().map(|v| v.version.clone());
    let latest_from_feed = support::normalize_latest_version(
        &current_version,
        raw_latest_version.as_deref(),
        pending_restart_version.as_deref(),
    );
    let latest = raw_latest_version
        .as_deref()
        .filter(|latest| support::version_is_strictly_newer(latest, &current_version))
        .map(|value| value.to_string());

    if latest.is_none() {
        staged::clear_staged_update_for_app(app)?;
        return Ok(transaction::idle_state(
            &config,
            current_version,
            latest_from_feed,
            last_attempt_id,
            None,
        ));
    }

    let latest = latest.unwrap_or_default();
    let staged_ready = match update.as_ref() {
        Some(update) => staged::has_matching_staged_update(
            app,
            channel,
            &latest,
            &config,
            &update.signature,
            update.download_url.as_str(),
        )?,
        None => false,
    };
    if staged_ready {
        return Ok(transaction::staged_ready_state(
            &config,
            current_version,
            latest,
            last_attempt_id,
            None,
        ));
    }

    if let Some(attempt) = &last_attempt {
        if attempt.result == DesktopUpdateAttemptResult::Failed
            && attempt.target_version.as_deref() == Some(latest.as_str())
            && attempt
                .finished_at_ms
                .map(|finished_at| {
                    now_ms().saturating_sub(finished_at) < STAGING_FAILURE_RETRY_DELAY_MS
                })
                .unwrap_or(true)
        {
            last_error =
                attempts::last_failed_stage_message(attempt).map(|value| value.to_string());
            return Ok(transaction::failed_state(
                &config,
                current_version,
                latest,
                last_attempt_id,
                last_error,
            ));
        }
    }

    if !STAGING_IN_PROGRESS.swap(true, Ordering::SeqCst) {
        let app_handle = app.clone();
        let channel_owned = channel.to_string();
        tauri::async_runtime::spawn(async move {
            let result = staged::stage_update_in_background(app_handle, &channel_owned).await;
            if let Err(err) = result {
                eprintln!("warn: background desktop updater staging failed: {err}");
            }
            STAGING_IN_PROGRESS.store(false, Ordering::SeqCst);
        });
    }

    if let Some(attempt) = &last_attempt {
        if attempt.result == DesktopUpdateAttemptResult::Failed
            && attempt.target_version.as_deref() == Some(latest.as_str())
        {
            last_error =
                attempts::last_failed_stage_message(attempt).map(|value| value.to_string());
        }
    }

    Ok(transaction::staging_state(
        &config,
        current_version,
        latest,
        last_attempt_id,
        last_error,
    ))
}
