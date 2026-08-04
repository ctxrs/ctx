use std::{
    path::{Path, PathBuf},
    time::{Duration as StdDuration, Instant},
};

use anyhow::Result;
use serde_json::{json, Value};

use crate::compact_json;

use super::{
    daemon_core_refresh_job_path, read_daemon_job_status, source_route_ledger_now_ms,
    write_daemon_job_status, DaemonRuntime,
};

pub(in crate::semantic) const DAEMON_BACKGROUND_REFRESH_MIN_REST: StdDuration =
    StdDuration::from_secs(5);
pub(in crate::semantic) const DAEMON_BACKGROUND_REFRESH_MAX_REST: StdDuration =
    StdDuration::from_secs(15 * 60);
const DAEMON_BACKGROUND_REFRESH_RECOVERY_FILE: &str = "background-refresh-recovery.json";

#[derive(Debug)]
struct DaemonBackgroundRefreshRecoveryProvenance {
    request_id: String,
    recovery_started_at_ms: u64,
}

impl DaemonBackgroundRefreshRecoveryProvenance {
    fn from_automatic_status(status: &Value, recovery_started_at_ms: u64) -> Option<Self> {
        if status.get("operation").and_then(Value::as_str) != Some("refresh")
            || status.get("trigger").and_then(Value::as_str) != Some("periodic")
            || status.get("trigger_provenance").and_then(Value::as_str) != Some("daemon_scheduler")
        {
            return None;
        }
        let request_id = status
            .get("request_id")
            .and_then(Value::as_str)
            .filter(|request_id| !request_id.is_empty())?
            .to_owned();
        Some(Self {
            request_id,
            recovery_started_at_ms,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        if value.get("schema_version").and_then(Value::as_u64) != Some(1)
            || value.get("kind").and_then(Value::as_str)
                != Some("background_refresh_recovery_provenance")
            || value.get("trigger").and_then(Value::as_str) != Some("periodic")
            || value.get("trigger_provenance").and_then(Value::as_str) != Some("daemon_scheduler")
        {
            return None;
        }
        let request_id = value
            .get("request_id")
            .and_then(Value::as_str)
            .filter(|request_id| !request_id.is_empty())?
            .to_owned();
        let recovery_started_at_ms = value.get("recovery_started_at_ms")?.as_u64()?;
        Some(Self {
            request_id,
            recovery_started_at_ms,
        })
    }

    fn to_json(&self) -> Value {
        compact_json(json!({
            "schema_version": 1,
            "kind": "background_refresh_recovery_provenance",
            "request_id": self.request_id,
            "recovery_started_at_ms": self.recovery_started_at_ms,
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
        }))
    }

    fn matches_recovered_publication(&self, status: &Value) -> bool {
        status.get("request_id").and_then(Value::as_str) == Some(self.request_id.as_str())
            && status.get("request_state").and_then(Value::as_str) == Some("published")
            && status.get("status").and_then(Value::as_str) == Some("completed")
            && status.get("trigger").and_then(Value::as_str) == Some("recovery")
            && status.get("trigger_provenance").and_then(Value::as_str) == Some("commit_payload")
            && status
                .get("published_generation")
                .and_then(Value::as_str)
                .is_some_and(|generation| !generation.is_empty())
            && status.get("receipt").is_some_and(Value::is_object)
    }
}

/// Monotonic, duration-aware cadence for automatic provider capture.
///
/// Explicit requests are admitted before this policy is consulted. Automatic
/// work rests for at least five seconds and, up to the cap, for as long as the
/// previous capture took. This prevents a continuously dirty route from
/// turning the daemon into a tight publisher loop while keeping the tuning
/// independent from route-local failure backoff.
#[derive(Debug, Default)]
pub(in crate::semantic) struct DaemonBackgroundRefreshCadence {
    not_before: Option<Instant>,
}

impl DaemonBackgroundRefreshCadence {
    pub(in crate::semantic) fn ready(&self, now: Instant) -> bool {
        self.not_before.is_none_or(|not_before| now >= not_before)
    }

    pub(in crate::semantic) fn remaining(&self, now: Instant) -> Option<StdDuration> {
        self.not_before
            .and_then(|not_before| not_before.checked_duration_since(now))
    }

    pub(in crate::semantic) fn record_completion(&mut self, started: Instant, completed: Instant) {
        let capture_duration = completed.saturating_duration_since(started);
        let rest = background_refresh_rest(capture_duration);
        self.not_before = completed.checked_add(rest).or(Some(completed));
    }

    pub(in crate::semantic) fn restore(
        &mut self,
        status: Option<&Value>,
        recovered_automatic_at_ms: Option<u64>,
        wall_now_ms: u64,
        now: Instant,
    ) {
        let Some(status) = status else {
            return;
        };
        let periodic = status.get("operation").and_then(Value::as_str) == Some("refresh")
            && status.get("trigger").and_then(Value::as_str) == Some("periodic")
            && status.get("trigger_provenance").and_then(Value::as_str) == Some("daemon_scheduler");
        if !periodic && recovered_automatic_at_ms.is_none() {
            return;
        }
        let Some(finished_at_ms) =
            status_timestamp_ms(status, "finished_at_ms").or(recovered_automatic_at_ms)
        else {
            return;
        };
        let started_at_ms = status_timestamp_ms(status, "started_at_ms")
            .or_else(|| status_timestamp_ms(status, "last_run_at_ms"))
            .unwrap_or(finished_at_ms);
        let maximum_rest_ms =
            u64::try_from(DAEMON_BACKGROUND_REFRESH_MAX_REST.as_millis()).unwrap_or(u64::MAX);
        let capture_duration = StdDuration::from_millis(
            finished_at_ms
                .saturating_sub(started_at_ms)
                .min(maximum_rest_ms),
        );
        let rest_ms = u64::try_from(background_refresh_rest(capture_duration).as_millis())
            .unwrap_or(u64::MAX);
        let not_before_at_ms = finished_at_ms.saturating_add(rest_ms);
        // Wall clocks may move backward or persisted status may be malformed.
        // Recovery never extends the monotonic cooldown beyond its normal cap.
        let remaining_ms = not_before_at_ms
            .saturating_sub(wall_now_ms)
            .min(maximum_rest_ms);
        if remaining_ms == 0 {
            self.not_before = None;
            return;
        }
        self.not_before = now
            .checked_add(StdDuration::from_millis(remaining_ms))
            .or(Some(now));
    }
}

pub(super) fn background_refresh_rest(capture_duration: StdDuration) -> StdDuration {
    capture_duration
        .max(DAEMON_BACKGROUND_REFRESH_MIN_REST)
        .min(DAEMON_BACKGROUND_REFRESH_MAX_REST)
}

fn status_timestamp_ms(status: &Value, field: &str) -> Option<u64> {
    status
        .get(field)
        .and_then(Value::as_i64)
        .and_then(|value| u64::try_from(value).ok())
}

pub(in crate::semantic) fn restore_daemon_background_refresh_cadence(
    runtime: &mut DaemonRuntime,
    data_root: &Path,
) {
    let status = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
    let recovered_automatic_at_ms = read_daemon_job_status(
        &daemon_background_refresh_recovery_provenance_path(data_root),
    )
    .as_ref()
    .and_then(DaemonBackgroundRefreshRecoveryProvenance::from_json)
    .zip(status.as_ref())
    .and_then(|(provenance, status)| {
        provenance
            .matches_recovered_publication(status)
            .then_some(provenance.recovery_started_at_ms)
    });
    runtime.background_refresh_cadence.restore(
        status.as_ref(),
        recovered_automatic_at_ms,
        source_route_ledger_now_ms(),
        Instant::now(),
    );
}

pub(in crate::semantic) fn preserve_daemon_background_refresh_recovery_provenance(
    data_root: &Path,
) -> Result<()> {
    let status = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
    let recovery_started_at_ms = source_route_ledger_now_ms();
    let existing = read_daemon_job_status(&daemon_background_refresh_recovery_provenance_path(
        data_root,
    ))
    .as_ref()
    .and_then(DaemonBackgroundRefreshRecoveryProvenance::from_json);
    let Some(provenance) = status.as_ref().and_then(|status| {
        DaemonBackgroundRefreshRecoveryProvenance::from_automatic_status(
            status,
            recovery_started_at_ms,
        )
        .or_else(|| {
            existing.and_then(|existing| {
                existing.matches_recovered_publication(status).then_some(
                    DaemonBackgroundRefreshRecoveryProvenance {
                        request_id: existing.request_id,
                        recovery_started_at_ms,
                    },
                )
            })
        })
    }) else {
        return Ok(());
    };
    write_daemon_job_status(
        &daemon_background_refresh_recovery_provenance_path(data_root),
        &provenance.to_json(),
    )
}

fn daemon_background_refresh_recovery_provenance_path(data_root: &Path) -> PathBuf {
    daemon_core_refresh_job_path(data_root).with_file_name(DAEMON_BACKGROUND_REFRESH_RECOVERY_FILE)
}
