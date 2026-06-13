use super::*;

fn build_state_response(
    config: &DesktopNativeUpdaterConfig,
    current_version: String,
    configured: bool,
    available: bool,
    restart_required: bool,
    phase: DesktopAppUpdatePhase,
    staged: bool,
    latest_version: Option<String>,
    message: Option<String>,
    last_attempt_id: Option<String>,
    last_error: Option<String>,
) -> DesktopAppUpdateStateResp {
    DesktopAppUpdateStateResp {
        configured,
        available,
        restart_required,
        phase: phase.as_str().to_string(),
        staged,
        current_version,
        latest_version,
        target: config.target.clone(),
        endpoint: config.endpoint.clone(),
        message,
        last_attempt_id,
        last_error,
    }
}

pub(super) fn restart_required_state(
    config: &DesktopNativeUpdaterConfig,
    current_version: String,
    latest_version: Option<String>,
    last_attempt_id: Option<String>,
    last_error: Option<String>,
) -> DesktopAppUpdateStateResp {
    build_state_response(
        config,
        current_version,
        config.pubkey.is_some(),
        true,
        true,
        DesktopAppUpdatePhase::RestartRequired,
        false,
        latest_version,
        Some(RESTART_READY_MESSAGE.to_string()),
        last_attempt_id,
        last_error,
    )
}

pub(super) fn unconfigured_state(
    config: &DesktopNativeUpdaterConfig,
    current_version: String,
    latest_version: Option<String>,
    message: Option<String>,
    last_attempt_id: Option<String>,
    last_error: Option<String>,
) -> DesktopAppUpdateStateResp {
    build_state_response(
        config,
        current_version,
        false,
        false,
        false,
        DesktopAppUpdatePhase::Idle,
        false,
        latest_version,
        message,
        last_attempt_id,
        last_error,
    )
}

pub(super) fn idle_state(
    config: &DesktopNativeUpdaterConfig,
    current_version: String,
    latest_version: Option<String>,
    last_attempt_id: Option<String>,
    last_error: Option<String>,
) -> DesktopAppUpdateStateResp {
    build_state_response(
        config,
        current_version,
        true,
        false,
        false,
        DesktopAppUpdatePhase::Idle,
        false,
        latest_version,
        None,
        last_attempt_id,
        last_error,
    )
}

pub(super) fn staged_ready_state(
    config: &DesktopNativeUpdaterConfig,
    current_version: String,
    latest_version: String,
    last_attempt_id: Option<String>,
    last_error: Option<String>,
) -> DesktopAppUpdateStateResp {
    build_state_response(
        config,
        current_version,
        true,
        true,
        false,
        DesktopAppUpdatePhase::StagedReady,
        true,
        Some(latest_version),
        None,
        last_attempt_id,
        last_error,
    )
}

pub(super) fn staging_state(
    config: &DesktopNativeUpdaterConfig,
    current_version: String,
    latest_version: String,
    last_attempt_id: Option<String>,
    last_error: Option<String>,
) -> DesktopAppUpdateStateResp {
    build_state_response(
        config,
        current_version,
        true,
        false,
        false,
        DesktopAppUpdatePhase::Staging,
        false,
        Some(latest_version),
        Some("Downloading update in background.".to_string()),
        last_attempt_id,
        last_error,
    )
}

pub(super) fn failed_state(
    config: &DesktopNativeUpdaterConfig,
    current_version: String,
    latest_version: String,
    last_attempt_id: Option<String>,
    last_error: Option<String>,
) -> DesktopAppUpdateStateResp {
    build_state_response(
        config,
        current_version,
        true,
        true,
        false,
        DesktopAppUpdatePhase::Failed,
        false,
        Some(latest_version),
        Some("Desktop update failed while installing in background.".to_string()),
        last_attempt_id,
        last_error,
    )
}

pub(super) fn short_circuit_apply(
    pre_state: &DesktopAppUpdateStateResp,
) -> Result<Option<DesktopAppUpdateApplyResp>, String> {
    if pre_state.restart_required {
        return Ok(Some(DesktopAppUpdateApplyResp {
            applied: false,
            needs_restart: true,
            up_to_date: false,
            latest_version: pre_state.latest_version.clone(),
            message: RESTART_READY_MESSAGE.to_string(),
        }));
    }

    if !pre_state.configured {
        return Err(support::MISSING_EMBEDDED_UPDATER_PUBKEY_MESSAGE.to_string());
    }

    Ok(None)
}

pub(super) fn up_to_date_apply_response(
    latest_version: Option<String>,
) -> DesktopAppUpdateApplyResp {
    DesktopAppUpdateApplyResp {
        applied: false,
        needs_restart: false,
        up_to_date: true,
        latest_version,
        message: "No desktop app update is currently available.".to_string(),
    }
}
