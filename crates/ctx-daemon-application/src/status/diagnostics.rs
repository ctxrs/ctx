//! Passive, bounded observations. This path never initializes application or
//! supervisor services and never opens provider history or Core generations.

use std::{fs, io::Read, path::Path, time::Instant};

use ctx_daemon_runtime::{
    daemon_lock_path, daemon_root_path, daemon_status_path, observe_pid_advisory_guard,
    observe_process_cpu, process_state, ProcessCpuObservation, ProcessCpuUnavailable, ProcessState,
    PID_LOCK_PROTOCOL,
};
use ctx_daemon_service::{daemon_core_refresh_job_path, daemon_semantic_job_path};
use serde_json::{json, Value};

const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;
const COUNTERS: [&str; 7] = [
    "filesystem_signals",
    "ipc_signals",
    "timeout_wakeups",
    "scheduled_retry_wakeups",
    "scheduled_refresh_wakeups",
    "work_cycles",
    "no_work_cycles",
];

#[derive(Debug, PartialEq, Eq)]
struct Owner {
    id: String,
    pid: u32,
    started_at_ms: u64,
}

impl Owner {
    fn from_lock(value: &Value) -> Option<Self> {
        if value["lock_protocol"].as_str() != Some(PID_LOCK_PROTOCOL)
            || value["released"].as_bool() != Some(false)
        {
            return None;
        }
        Self::from_json(value)
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            id: value["owner_id"]
                .as_str()
                .filter(|s| !s.is_empty())?
                .to_owned(),
            pid: u32::try_from(value["pid"].as_u64()?)
                .ok()
                .filter(|pid| *pid > 0)?,
            started_at_ms: value["started_at_ms"].as_u64().filter(|value| *value > 0)?,
        })
    }
}

// Snapshot and owner types have no serialization implementation. The
// CLI's closed report model is the only outward-facing serializer.
pub struct DaemonDiagnosticSnapshot {
    owner: Option<Owner>,
    daemon: Value,
    refresh: Value,
    wakeup: Value,
    observed_at: Instant,
    running: ProcessState,
    cpu: Result<ProcessCpuObservation, ProcessCpuUnavailable>,
    cpu_at: Instant,
}

impl DaemonDiagnosticSnapshot {
    /// A retained, unbound receipt for advisory diagnostics only. Use the same
    /// bounded reader as other sampled metadata; this does not establish work
    /// in the current daemon or invoke the semantic runtime.
    pub fn last_recorded_semantic_job(data_root: &Path) -> Value {
        read_record(&daemon_semantic_job_path(data_root))
    }

    pub fn observe(data_root: &Path) -> Self {
        let lock_path = daemon_lock_path(data_root);
        let before = read_record(&lock_path);
        // A coherent metadata tuple and a live PID do not establish ownership:
        // stale crash records can name a PID now used by an unrelated process.
        // This observer opens only the existing guard and never creates it.
        let owner = Owner::from_lock(&before).filter(|_| advisory_lock_held(&lock_path));
        let daemon = read_record(&daemon_status_path(data_root));
        let refresh = read_record(&daemon_core_refresh_job_path(data_root));
        let wakeup = read_record(&daemon_root_path(data_root).join("wakeup.json"));
        let observed_at = Instant::now();
        let running = owner
            .as_ref()
            .map_or(ProcessState::Unknown, |owner| process_state(owner.pid));
        let cpu = owner
            .as_ref()
            .map_or(Err(ProcessCpuUnavailable::Unavailable), |owner| {
                observe_process_cpu(owner.pid)
            });
        let cpu_at = Instant::now();
        let still_held = owner.is_some() && advisory_lock_held(&lock_path);
        let after = read_record(&lock_path);
        let owner = owner.filter(|owner| {
            still_held
                && Some(owner) == Owner::from_lock(&after).as_ref()
                && daemon["pid"].as_u64() == Some(u64::from(owner.pid))
                && daemon["started_at_ms"].as_u64() == Some(owner.started_at_ms)
                // Cleanup can hold the guard over stale owner metadata. Bind
                // all owner authority, including scheduler rates, to the birth
                // recorded when the actual daemon acquired its lock.
                && cpu.is_ok_and(|cpu| {
                    cpu.identity.matches_private_json_token(&before["process_creation_token"])
                        && cpu.identity.matches_private_json_token(&after["process_creation_token"])
                })
        });
        Self {
            owner,
            daemon,
            refresh,
            wakeup,
            observed_at,
            running,
            cpu,
            cpu_at,
        }
    }

    /// Observation input for the CLI's allowlisted diagnostic projection, not a
    /// shareable document. The retained job may contain private source paths.
    pub fn observation_since(&self, first: &Self) -> Value {
        let elapsed = self.observed_at.checked_duration_since(first.observed_at);
        let daemon_state = match (self.owner.as_ref(), self.running) {
            (Some(_), _) if self.cpu == Err(ProcessCpuUnavailable::NotRunning) => "stopped",
            (Some(_), ProcessState::NotRunning) => "stopped",
            (Some(_), ProcessState::Running) => "running",
            _ => "unknown",
        };
        let heartbeat_age_ms = (daemon_state == "running")
            .then(|| {
                let heartbeat = self.daemon["heartbeat_at_ms"].as_u64()?;
                let now = u64::try_from(ctx_history_core::utc_now().timestamp_millis()).ok()?;
                now.checked_sub(heartbeat)
            })
            .flatten();
        let continuity = self.continuity(first);
        let mut scheduler_status = continuity;
        let mut deltas = [None; 7];
        if continuity == "observed" {
            let first_binding = first.wakeup_owner_status();
            let last_binding = self.wakeup_owner_status();
            if first_binding == "daemon_restarted" || last_binding == "daemon_restarted" {
                scheduler_status = "daemon_restarted";
            } else if first_binding != "observed" || last_binding != "observed" {
                scheduler_status = "identity_unknown";
            } else if elapsed.is_none_or(|duration| duration.is_zero()) {
                scheduler_status = "invalid_record";
            }
        }
        if scheduler_status == "observed" {
            for (slot, key) in deltas.iter_mut().zip(COUNTERS) {
                if let Some((end, start)) = self.wakeup["wakeup"][key]
                    .as_u64()
                    .zip(first.wakeup["wakeup"][key].as_u64())
                {
                    *slot = end.checked_sub(start);
                    if slot.is_none() {
                        scheduler_status = "counter_reset";
                    }
                }
            }
            if scheduler_status == "counter_reset" {
                deltas.fill(None);
            } else if deltas.iter().all(Option::is_none) {
                scheduler_status = "unavailable";
            }
        }
        let mut scheduler: serde_json::Map<String, Value> = COUNTERS
            .into_iter()
            .zip(deltas)
            .map(|(key, delta)| (key.to_owned(), json!(delta)))
            .collect();
        scheduler.insert("status".to_owned(), json!(scheduler_status));
        let cycles_per_second = deltas[5]
            .zip(elapsed)
            .filter(|(_, duration)| !duration.is_zero())
            .map(|(count, duration)| count as f64 / duration.as_secs_f64());
        scheduler.insert(
            "work_cycles_per_second".to_owned(),
            json!(cycles_per_second),
        );
        json!({
            "elapsed_ms": elapsed.and_then(|duration| u64::try_from(duration.as_millis()).ok()),
            "daemon": {"state": daemon_state, "heartbeat_age_ms": heartbeat_age_ms},
            "refresh": self.refresh,
            "observed": {
                "cpu": self.cpu_since(first, continuity),
                "scheduler": scheduler,
            },
        })
    }

    fn wakeup_owner_status(&self) -> &'static str {
        match (&self.owner, Owner::from_json(&self.wakeup["daemon_owner"])) {
            (Some(expected), Some(recorded)) if expected == &recorded => "observed",
            (Some(_), Some(_)) => "daemon_restarted",
            _ => "identity_unknown",
        }
    }

    fn cpu_since(&self, first: &Self, continuity: &str) -> Value {
        let unavailable = |status: &str| json!({"status": status, "elapsed_ms": null, "cpu_ms": null, "percent_one_core": null});
        if !matches!(continuity, "observed" | "identity_unknown") {
            return unavailable(continuity);
        }
        let (before, after) = match (first.cpu, self.cpu) {
            (_, Err(error)) | (Err(error), _) => {
                return unavailable(match error {
                    ProcessCpuUnavailable::Unsupported => "unsupported",
                    ProcessCpuUnavailable::PermissionDenied => "permission_denied",
                    ProcessCpuUnavailable::NotRunning => "process_exited",
                    ProcessCpuUnavailable::Unavailable if continuity == "identity_unknown" => {
                        "identity_unknown"
                    }
                    ProcessCpuUnavailable::Unavailable => "unavailable",
                })
            }
            (Ok(before), Ok(after)) => (before, after),
        };
        if continuity != "observed" {
            return unavailable(continuity);
        }
        if before.identity != after.identity {
            return unavailable("daemon_restarted");
        }
        let cpu_us = after
            .user_cpu_us
            .checked_sub(before.user_cpu_us)
            .zip(after.system_cpu_us.checked_sub(before.system_cpu_us))
            .and_then(|(user, system)| user.checked_add(system));
        let Some(cpu_us) = cpu_us else {
            return unavailable("counter_reset");
        };
        let Some(elapsed) = self
            .cpu_at
            .checked_duration_since(first.cpu_at)
            .filter(|value| !value.is_zero())
        else {
            return unavailable("invalid_record");
        };
        json!({
            "status": "observed",
            "elapsed_ms": u64::try_from(elapsed.as_millis()).ok(),
            "cpu_ms": cpu_us as f64 / 1000.0,
            "percent_one_core": 100.0 * (cpu_us as f64 / 1_000_000.0) / elapsed.as_secs_f64(),
        })
    }

    fn continuity(&self, first: &Self) -> &'static str {
        match (&first.owner, &self.owner) {
            (Some(before), Some(after)) if before != after => "daemon_restarted",
            (Some(_), Some(_))
                if self.cpu == Err(ProcessCpuUnavailable::NotRunning)
                    || first.cpu == Err(ProcessCpuUnavailable::NotRunning) =>
            {
                "process_exited"
            }
            (Some(_), Some(_)) if matches!((first.cpu, self.cpu), (Ok(before), Ok(after)) if before.identity != after.identity) => {
                "daemon_restarted"
            }
            (Some(_), Some(_)) if self.running == ProcessState::NotRunning => "process_exited",
            (Some(_), Some(_))
                if self.running == ProcessState::Running
                    && first.running == ProcessState::Running =>
            {
                "observed"
            }
            _ => "identity_unknown",
        }
    }
}

fn advisory_lock_held(path: &Path) -> bool {
    observe_pid_advisory_guard(path) == Some(true)
}

fn read_record(path: &Path) -> Value {
    read_record_inner(path).unwrap_or(Value::Null)
}

fn read_record_inner(path: &Path) -> Option<Value> {
    if !fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value.is_object().then_some(value)
}

#[cfg(test)]
mod tests;
