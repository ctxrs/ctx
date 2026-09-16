use super::*;

/// Complete only the initiating manual command. Automatic attempts retain their
/// daemon-owned cadence and terminal telemetry finalization.
#[cfg(any(windows, test))]
pub(in crate::upgrade) fn finish_manual_replacement_locked(
    lock: &UpgradeLock,
    attempt_id: &str,
    applied: bool,
    detail: Option<&str>,
) -> Result<()> {
    let state = read_state_object(&lock.install_path);
    if state.attempt_id.as_deref() == Some(attempt_id)
        && matches!(
            state.attempt_source.as_deref(),
            Some("manual_apply" | "manual_recovery")
        )
    {
        // Manual terminal recording leaves automatic cadence/backoff untouched.
        reconcile_replacement_terminal_locked(lock, attempt_id, applied, detail, Duration::ZERO)?;
    }
    Ok(())
}

pub(in crate::upgrade) fn reconcile_replacement_terminal_locked(
    lock: &UpgradeLock,
    attempt_id: &str,
    applied: bool,
    warning_or_error: Option<&str>,
    interval: Duration,
) -> Result<bool> {
    if applied && applied_state_write_failure_injected(attempt_id) {
        return Err(anyhow!("injected applied-state write failure"));
    }
    let mut state = read_state_object(&lock.install_path);
    let automatic = state.attempt_id.as_deref() == Some(attempt_id)
        && state
            .attempt_source
            .as_deref()
            .is_some_and(is_automatic_attempt_source);
    if state.attempt_id.as_deref() != Some(attempt_id) {
        state.schema_version = STATE_SCHEMA_VERSION;
        state.attempt_id = Some(attempt_id.to_owned());
        state.attempt_source = Some("recovery".to_owned());
        state.last_attempt_at = Some(utc_now());
    }
    let attempt = UpgradeAttempt {
        id: attempt_id.to_owned(),
    };
    if applied {
        state.terminal(&attempt, "applied", interval, now_unix_s());
        if let Some(latest) = state.plan.get("latest_version").cloned() {
            state.plan.insert("current_version".to_owned(), latest);
        }
        state
            .plan
            .insert("update_available".to_owned(), Value::Bool(false));
        if let Some(warning) = warning_or_error {
            state.plan.insert("warning".to_owned(), json!(warning));
        }
    } else {
        state.fail(
            &attempt,
            warning_or_error.unwrap_or("replacement failed"),
            now_unix_s(),
        );
    }
    write_state_object_locked(lock, state)?;
    Ok(automatic)
}

pub(super) fn applied_state_write_failure_injected(attempt_id: &str) -> bool {
    crate::upgrade::test_harness_enabled()
        && std::env::var("CTX_UPGRADE_FAIL_APPLIED_STATE_WRITE_FOR_TESTS").is_ok_and(|value| {
            value == attempt_id
                || (!value.starts_with("ua_")
                    && !["", "0", "false", "no", "off"]
                        .contains(&value.trim().to_ascii_lowercase().as_str()))
        })
}
