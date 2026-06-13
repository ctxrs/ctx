use super::*;

pub(super) fn begin_update_attempt(channel: &str, current_version: &str) -> DesktopUpdateAttempt {
    DesktopUpdateAttempt {
        attempt_id: format!("desktop-updater-{}-{}", now_ms(), std::process::id()),
        channel: channel.to_string(),
        current_version: current_version.to_string(),
        target_version: None,
        started_at_ms: now_ms(),
        finished_at_ms: None,
        result: DesktopUpdateAttemptResult::InProgress,
        stages: Vec::new(),
    }
}

pub(super) fn begin_attempt_stage(attempt: &mut DesktopUpdateAttempt, stage: &str) -> usize {
    attempt.stages.push(DesktopUpdateAttemptStage {
        stage: stage.to_string(),
        started_at_ms: now_ms(),
        finished_at_ms: None,
        result: DesktopUpdateAttemptResult::InProgress,
        error_code: None,
        error_message: None,
    });
    attempt.stages.len() - 1
}

pub(super) fn complete_attempt_stage(attempt: &mut DesktopUpdateAttempt, index: usize) {
    if let Some(stage) = attempt.stages.get_mut(index) {
        stage.finished_at_ms = Some(now_ms());
        stage.result = DesktopUpdateAttemptResult::Succeeded;
        stage.error_code = None;
        stage.error_message = None;
    }
}

pub(super) fn fail_attempt_stage(
    attempt: &mut DesktopUpdateAttempt,
    index: usize,
    code: &str,
    message: &str,
) {
    if let Some(stage) = attempt.stages.get_mut(index) {
        stage.finished_at_ms = Some(now_ms());
        stage.result = DesktopUpdateAttemptResult::Failed;
        stage.error_code = Some(code.to_string());
        stage.error_message = Some(message.to_string());
    }
}

fn mark_attempt_succeeded(attempt: &mut DesktopUpdateAttempt) {
    attempt.finished_at_ms = Some(now_ms());
    attempt.result = DesktopUpdateAttemptResult::Succeeded;
}

fn mark_attempt_failed(attempt: &mut DesktopUpdateAttempt) {
    attempt.finished_at_ms = Some(now_ms());
    attempt.result = DesktopUpdateAttemptResult::Failed;
}

pub(super) fn persist_attempt_success_best_effort(
    app: &tauri::AppHandle,
    attempt: &mut DesktopUpdateAttempt,
) {
    mark_attempt_succeeded(attempt);
    if let Err(write_err) = write_last_attempt_for_app(app, attempt) {
        eprintln!("warn: failed to persist updater success attempt: {write_err}");
    }
}

pub(super) fn persist_attempt_failure_best_effort(
    app: &tauri::AppHandle,
    attempt: &mut DesktopUpdateAttempt,
    err: String,
) -> String {
    mark_attempt_failed(attempt);
    if let Err(write_err) = write_last_attempt_for_app(app, attempt) {
        eprintln!("warn: failed to persist updater failure attempt: {write_err}");
    }
    err
}

pub(super) fn write_last_attempt_for_app(
    app: &tauri::AppHandle,
    attempt: &DesktopUpdateAttempt,
) -> Result<(), String> {
    let path = last_attempt_path_for_app(app)?;
    let encoded = serde_json::to_string_pretty(attempt)
        .map_err(|e| format!("encoding desktop updater attempt: {e}"))?;
    std::fs::write(&path, format!("{encoded}\n"))
        .map_err(|e| format!("writing desktop updater attempt '{}': {e}", path.display()))
}

pub(super) fn read_last_attempt_for_app(
    app: &tauri::AppHandle,
) -> Result<Option<DesktopUpdateAttempt>, String> {
    let path = last_attempt_path_for_app(app)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("reading desktop updater attempt '{}': {e}", path.display()))?;
    let parsed = serde_json::from_str::<DesktopUpdateAttempt>(&raw)
        .map_err(|e| format!("parsing desktop updater attempt '{}': {e}", path.display()))?;
    Ok(Some(parsed))
}

pub(super) fn last_failed_stage_message(attempt: &DesktopUpdateAttempt) -> Option<&str> {
    if attempt.result != DesktopUpdateAttemptResult::Failed {
        return None;
    }
    attempt
        .stages
        .iter()
        .rev()
        .find_map(|stage| stage.error_message.as_deref())
}
