use super::*;

use tauri_plugin_updater::UpdaterExt;

pub(super) async fn apply_app_update(
    app: tauri::AppHandle,
    req: DesktopAppUpdateApplyReq,
) -> Result<DesktopAppUpdateApplyResp, String> {
    let _apply_guard = APPLY_IN_PROGRESS
        .get_or_init(|| AsyncMutex::new(()))
        .lock()
        .await;
    if !req.confirm {
        return Err("confirm required".to_string());
    }
    #[cfg(target_os = "windows")]
    {
        return Err("Desktop background update apply is not supported on Windows yet.".to_string());
    }
    let channel = resolve_app_update_channel(&app, req.channel.as_deref())?;
    let download_id = support::normalize_download_id(req.download_id.as_deref());
    let pre_state = recovery::resolve_desktop_update_state(&app, &channel).await?;

    if let Some(response) = transaction::short_circuit_apply(&pre_state)? {
        return Ok(response);
    }

    let mut attempt = attempts::begin_update_attempt(&channel, &pre_state.current_version);
    let config = support::resolve_native_updater_config(&channel)?;
    let pubkey = config
        .pubkey
        .as_deref()
        .ok_or_else(|| support::MISSING_EMBEDDED_UPDATER_PUBKEY_MESSAGE.to_string())?;
    let endpoint_url =
        support::endpoint_with_download_id(&config.endpoint, download_id.as_deref())?;
    let build_stage = attempts::begin_attempt_stage(&mut attempt, "build");
    let updater = app
        .updater_builder()
        .target(config.target.clone())
        .pubkey(pubkey)
        .endpoints(vec![endpoint_url])
        .map_err(|e| {
            let err = support::updater_stage_error("build", e);
            attempts::fail_attempt_stage(&mut attempt, build_stage, "build", &err);
            attempts::persist_attempt_failure_best_effort(&app, &mut attempt, err)
        })?
        .build()
        .map_err(|e| {
            let err = support::updater_stage_error("build", e);
            attempts::fail_attempt_stage(&mut attempt, build_stage, "build", &err);
            attempts::persist_attempt_failure_best_effort(&app, &mut attempt, err)
        })?;
    attempts::complete_attempt_stage(&mut attempt, build_stage);

    let check_stage = attempts::begin_attempt_stage(&mut attempt, "check");
    let Some(update) = updater.check().await.map_err(|e| {
        let err = support::updater_stage_error("check", e);
        attempts::fail_attempt_stage(&mut attempt, check_stage, "check", &err);
        attempts::persist_attempt_failure_best_effort(&app, &mut attempt, err)
    })?
    else {
        attempts::complete_attempt_stage(&mut attempt, check_stage);
        attempts::persist_attempt_success_best_effort(&app, &mut attempt);
        return Ok(transaction::up_to_date_apply_response(None));
    };
    attempts::complete_attempt_stage(&mut attempt, check_stage);
    let latest_version = update.version.clone();
    attempt.target_version = Some(latest_version.clone());
    eprintln!(
        "native updater apply start: target={} version={latest_version}",
        config.target
    );

    let verify_stage = attempts::begin_attempt_stage(&mut attempt, "verify");
    if !support::version_is_strictly_newer(&latest_version, &pre_state.current_version) {
        attempts::complete_attempt_stage(&mut attempt, verify_stage);
        attempts::persist_attempt_success_best_effort(&app, &mut attempt);
        return Ok(transaction::up_to_date_apply_response(Some(latest_version)));
    }
    attempts::complete_attempt_stage(&mut attempt, verify_stage);

    let download_stage = attempts::begin_attempt_stage(&mut attempt, "download");
    let bytes = if let Some(staged_bytes) = staged::read_verified_staged_update_bytes_if_matching(
        &app,
        &channel,
        &latest_version,
        &config,
        &update.signature,
        update.download_url.as_str(),
        pubkey,
    )? {
        attempts::complete_attempt_stage(&mut attempt, download_stage);
        staged_bytes
    } else {
        let fresh = update.download(|_, _| {}, || {}).await.map_err(|e| {
            let err = support::updater_stage_error("download", e);
            attempts::fail_attempt_stage(&mut attempt, download_stage, "download", &err);
            attempts::persist_attempt_failure_best_effort(&app, &mut attempt, err)
        })?;
        attempts::complete_attempt_stage(&mut attempt, download_stage);
        fresh
    };

    let install_stage = attempts::begin_attempt_stage(&mut attempt, "install");
    update.install(&bytes).map_err(|e| {
        let err = support::updater_stage_error("install", e);
        attempts::fail_attempt_stage(&mut attempt, install_stage, "install", &err);
        attempts::persist_attempt_failure_best_effort(&app, &mut attempt, err)
    })?;
    attempts::complete_attempt_stage(&mut attempt, install_stage);

    let marker_stage = attempts::begin_attempt_stage(&mut attempt, "marker");
    restart::write_restart_marker_for_app(&app, &latest_version).map_err(|e| {
        let err = support::updater_stage_error("marker_write", e);
        attempts::fail_attempt_stage(&mut attempt, marker_stage, "marker_write", &err);
        attempts::persist_attempt_failure_best_effort(&app, &mut attempt, err)
    })?;
    attempts::complete_attempt_stage(&mut attempt, marker_stage);
    if let Err(err) = staged::clear_staged_update_for_app(&app) {
        eprintln!("warn: failed to clear staged updater payload after install: {err}");
    }
    eprintln!("native updater apply success: version={latest_version}");
    attempts::persist_attempt_success_best_effort(&app, &mut attempt);

    Ok(DesktopAppUpdateApplyResp {
        applied: true,
        needs_restart: true,
        up_to_date: false,
        latest_version: Some(latest_version),
        message: RESTART_READY_MESSAGE.to_string(),
    })
}
