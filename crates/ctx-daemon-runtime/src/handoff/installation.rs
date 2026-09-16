use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use crate::{
    daemon_lock_path, observe_pid_advisory_lock, open_or_create_pid_lock_file, process_state,
    secure_private_file_permissions, ProcessState,
};

pub struct InstallationQuiescence {
    lock: fs::File,
}

impl Drop for InstallationQuiescence {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.lock);
    }
}

pub fn open_installation_quiescence_lock(path: &Path) -> Result<fs::File> {
    let (file, _) = open_or_create_pid_lock_file(path)
        .with_context(|| format!("open ctx installation daemon lock {}", path.display()))?;
    secure_private_file_permissions(path)?;
    Ok(file)
}

pub fn try_acquire_installation_quiescence(path: &Path) -> Result<Option<InstallationQuiescence>> {
    let lock = open_installation_quiescence_lock(path)?;
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => Ok(Some(InstallationQuiescence { lock })),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error).context("acquire ctx installation daemon quiescence"),
    }
}

pub fn wait_for_installation_quiescence(
    lock_path: &Path,
    registration_root: &Path,
    attempt_id: &str,
    timeout: Duration,
    poll_interval: Duration,
    loop_interval_cap: u64,
) -> Result<()> {
    let lock = open_installation_quiescence_lock(lock_path)?;
    let deadline = Instant::now() + timeout;
    loop {
        match fs2::FileExt::try_lock_exclusive(&lock) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "timed out waiting for all ctx daemons to acknowledge installation quiescence"
                    ));
                }
                std::thread::sleep(poll_interval);
            }
            Err(error) => {
                return Err(error).context("acquire ctx installation daemon quiescence");
            }
        }
    }
    let result =
        read_installation_restart_records(registration_root, attempt_id, true, loop_interval_cap)
            .and_then(|_| {
                // Validation proved these owners stopped. The exclusive lease
                // prevents a daemon from publishing a new live registration.
                for (path, value) in read_installation_registrations(registration_root)? {
                    if value["status"] == "live" {
                        // Cleanup failure cannot make a stopped owner live again.
                        let _ = fs::remove_file(path);
                    }
                }
                Ok(())
            });
    let _ = fs2::FileExt::unlock(&lock);
    result
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct InstallationRestartRecord {
    pub registration_path: PathBuf,
    pub data_root: PathBuf,
    pub opaque_trigger: String,
    pub loop_interval_seconds: Option<u64>,
}

pub fn read_installation_restart_records(
    root: &Path,
    attempt_id: &str,
    fail_on_live: bool,
    loop_interval_cap: u64,
) -> Result<Vec<InstallationRestartRecord>> {
    let registrations = read_installation_registrations(root)?;
    let mut restarts = Vec::new();
    for (path, value) in registrations {
        let status = value["status"].as_str().unwrap_or_default();
        let registration_attempt = value.get("attempt_id").and_then(Value::as_str);
        if status == "quiescing" && registration_attempt == Some(attempt_id) {
            return Err(anyhow!(
                "ctx daemon exited without completing its quiescence acknowledgement"
            ));
        }
        if status == "live" {
            let pid = value["pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
                .unwrap_or_default();
            if fail_on_live
                && matches!(
                    process_state(pid),
                    ProcessState::Running | ProcessState::Unknown
                )
                // An unlocked daemon guard proves ownership ended even if an
                // unrelated process has since reused the recorded PID.
                && !observe_pid_advisory_lock(&daemon_lock_path(Path::new(
                    value["data_root"].as_str().unwrap_or_default(),
                )))
                .is_some_and(|owner| !owner.held)
            {
                return Err(anyhow!(
                    "ctx daemon registration remains live after installation quiescence"
                ));
            }
            continue;
        }
        if status != "acknowledged" || registration_attempt != Some(attempt_id) {
            continue;
        }
        // The acknowledged root and trigger remain restart authority across
        // the lifecycle migration. Legacy lifetime fields are ignored, while
        // an explicitly selected maintenance cadence remains in force.
        let data_root = PathBuf::from(value["data_root"].as_str().unwrap_or_default());
        let opaque_trigger = value["trigger_command"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("ctx daemon acknowledgement has an invalid trigger"))?
            .to_owned();
        let legacy_persistent = value.get("persistent").and_then(Value::as_bool);
        let loop_interval_explicit = value
            .get("loop_interval_explicit")
            .and_then(Value::as_bool)
            .unwrap_or(legacy_persistent == Some(false));
        let loop_interval_seconds = match value.get("loop_interval_seconds") {
            Some(value) => {
                let interval = value
                    .as_u64()
                    .filter(|value| *value > 0 && *value <= loop_interval_cap)
                    .ok_or_else(|| {
                        anyhow!("ctx daemon acknowledgement has an invalid loop interval")
                    })?;
                loop_interval_explicit.then_some(interval)
            }
            None if !loop_interval_explicit => None,
            None => {
                return Err(anyhow!(
                    "ctx daemon acknowledgement has an invalid loop interval"
                ));
            }
        };
        restarts.push(InstallationRestartRecord {
            registration_path: path,
            data_root,
            opaque_trigger,
            loop_interval_seconds,
        });
    }
    restarts.sort_by(|left, right| left.data_root.cmp(&right.data_root));
    restarts.dedup_by(|left, right| left.data_root == right.data_root);
    Ok(restarts)
}

pub fn read_installation_registrations(root: &Path) -> Result<Vec<(PathBuf, Value)>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read ctx daemon acknowledgements {}", root.display()));
        }
    };
    let mut registrations = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("inspect ctx daemon acknowledgement {}", path.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(anyhow!(
                "ctx daemon acknowledgement is not a regular file: {}",
                path.display()
            ));
        }
        let value = fs::read(&path)
            .with_context(|| format!("read ctx daemon acknowledgement {}", path.display()))
            .and_then(|bytes| {
                serde_json::from_slice::<Value>(&bytes)
                    .with_context(|| format!("parse ctx daemon acknowledgement {}", path.display()))
            })?;
        validate_installation_registration(&value, &path)?;
        registrations.push((path, value));
    }
    registrations.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(registrations)
}

fn validate_installation_registration(value: &Value, path: &Path) -> Result<()> {
    let valid_status = matches!(
        value.get("status").and_then(Value::as_str),
        Some("live" | "released" | "quiescing" | "acknowledged")
    );
    let valid_root = value
        .get("data_root")
        .and_then(Value::as_str)
        .map(Path::new)
        .is_some_and(Path::is_absolute);
    if value.get("schema_version").and_then(Value::as_u64) != Some(1)
        || value
            .get("registration_id")
            .and_then(Value::as_str)
            .is_none_or(|id| id.is_empty())
        || !valid_status
        || !valid_root
    {
        return Err(anyhow!(
            "invalid ctx daemon acknowledgement at {}",
            path.display()
        ));
    }
    Ok(())
}

pub fn registered_installation_roots(root: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = read_installation_registrations(root)?
        .into_iter()
        .map(|(_, value)| PathBuf::from(value["data_root"].as_str().unwrap_or_default()))
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_registration(root: &Path, name: &str, value: Value) {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn registration(data_root: &Path, persistent: Option<bool>) -> Value {
        let mut value = json!({
            "schema_version": 1,
            "registration_id": "registration",
            "status": "acknowledged",
            "attempt_id": "attempt",
            "pid": 42,
            "data_root": data_root,
            "trigger_command": "search",
            "idle_exit_seconds": 5,
            "loop_interval_seconds": 7,
            "loop_interval_explicit": true,
            "updated_at_ms": 1,
        });
        if let Some(persistent) = persistent {
            value["persistent"] = Value::Bool(persistent);
        }
        value
    }

    #[test]
    fn restart_records_accept_legacy_timing_and_replay_every_root_persistently() {
        let temp = tempfile::tempdir().unwrap();
        let registrations = temp.path().join("registrations");
        let persistent_root = temp.path().join("persistent");
        let finite_root = temp.path().join("finite");
        let unspecified_root = temp.path().join("unspecified");
        write_registration(
            &registrations,
            "persistent.json",
            registration(&persistent_root, Some(true)),
        );
        write_registration(
            &registrations,
            "finite.json",
            registration(&finite_root, Some(false)),
        );
        write_registration(
            &registrations,
            "unspecified.json",
            registration(&unspecified_root, None),
        );

        let records =
            read_installation_restart_records(&registrations, "attempt", false, 3_600).unwrap();

        assert_eq!(records.len(), 3);
        assert_eq!(
            records
                .iter()
                .map(|record| record.data_root.clone())
                .collect::<Vec<_>>(),
            [finite_root, persistent_root, unspecified_root]
        );
        assert!(records
            .iter()
            .all(|record| record.opaque_trigger == "search"));
        assert!(records
            .iter()
            .all(|record| record.loop_interval_seconds == Some(7)));
    }

    #[test]
    fn restart_records_accept_new_timing_free_persistent_registration() {
        let temp = tempfile::tempdir().unwrap();
        let registrations = temp.path().join("registrations");
        let data_root = temp.path().join("data");
        write_registration(
            &registrations,
            "persistent.json",
            json!({
                "schema_version": 1,
                "registration_id": "registration",
                "status": "acknowledged",
                "attempt_id": "attempt",
                "pid": 42,
                "data_root": data_root,
                "trigger_command": "setup",
                "persistent": true,
                "updated_at_ms": 1,
            }),
        );

        let records =
            read_installation_restart_records(&registrations, "attempt", false, 3_600).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].data_root, data_root);
        assert_eq!(records[0].opaque_trigger, "setup");
        assert_eq!(records[0].loop_interval_seconds, None);
    }

    #[test]
    fn quiescence_prunes_crash_registration_with_reused_pid_without_restarting_it() {
        let temp = tempfile::tempdir().unwrap();
        let registrations = temp.path().join("registrations");
        let data_root = temp.path().join("crashed");
        let lock_path = temp.path().join("installation.lock");
        crate::create_private_dir_all(&crate::daemon_root_path(&data_root)).unwrap();
        let daemon_lock = crate::daemon_lock_path(&data_root);
        let (owner, _) =
            open_or_create_pid_lock_file(&crate::pid_lock_guard_path(&daemon_lock)).unwrap();
        fs2::FileExt::lock_exclusive(&owner).unwrap();
        assert!(crate::publish_pid_lock_metadata(
            &daemon_lock,
            &crate::pid_lock_payload(json!({})),
        )
        .unwrap());
        let mut crashed = registration(&data_root, None);
        crashed["status"] = json!("live");
        crashed["attempt_id"] = Value::Null;
        // The test process stands in for an unrelated process reusing the PID.
        crashed["pid"] = json!(std::process::id());
        write_registration(&registrations, "crashed.json", crashed);
        write_registration(
            &registrations,
            "current.json",
            registration(&temp.path().join("current"), None),
        );
        let mut stale = registration(&temp.path().join("stale"), None);
        stale["attempt_id"] = json!("previous-attempt");
        write_registration(&registrations, "stale.json", stale);

        // A still-held daemon guard cannot be dismissed on PID evidence alone.
        assert!(read_installation_restart_records(&registrations, "attempt", true, 3_600).is_err());
        // A crash releases the OS lock without updating its ownership metadata.
        drop(owner);
        assert_eq!(
            crate::read_pid_lock_json(&daemon_lock).unwrap()["released"],
            false
        );
        assert_eq!(process_state(std::process::id()), ProcessState::Running);
        assert_eq!(
            read_installation_restart_records(&registrations, "attempt", true, 3_600)
                .unwrap()
                .len(),
            1
        );
        assert!(registrations.join("crashed.json").exists());
        wait_for_installation_quiescence(
            &lock_path,
            &registrations,
            "attempt",
            Duration::ZERO,
            Duration::ZERO,
            3_600,
        )
        .unwrap();

        assert!(!registrations.join("crashed.json").exists());
        let records =
            read_installation_restart_records(&registrations, "attempt", false, 3_600).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].data_root, temp.path().join("current"));
        assert_eq!(process_state(std::process::id()), ProcessState::Running);
    }

    #[cfg(unix)]
    #[test]
    fn quiescence_prunes_stopped_pid_without_advisory_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let registrations = temp.path().join("registrations");
        let mut crashed = registration(&temp.path().join("data"), None);
        crashed["status"] = json!("live");
        crashed["attempt_id"] = Value::Null;
        crashed["pid"] = json!(u32::MAX);
        write_registration(&registrations, "crashed.json", crashed);

        wait_for_installation_quiescence(
            &temp.path().join("installation.lock"),
            &registrations,
            "attempt",
            Duration::ZERO,
            Duration::ZERO,
            3_600,
        )
        .unwrap();
        assert!(read_installation_registrations(&registrations)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn quiescence_preserves_registration_while_a_daemon_holds_its_installation_lease() {
        let temp = tempfile::tempdir().unwrap();
        let registrations = temp.path().join("registrations");
        let lock_path = temp.path().join("installation.lock");
        let lease = open_installation_quiescence_lock(&lock_path).unwrap();
        fs2::FileExt::lock_shared(&lease).unwrap();
        let mut live = registration(&temp.path().join("data"), None);
        live["status"] = json!("live");
        live["attempt_id"] = Value::Null;
        live["pid"] = json!(std::process::id());
        write_registration(&registrations, "live.json", live.clone());

        assert!(wait_for_installation_quiescence(
            &lock_path,
            &registrations,
            "attempt",
            Duration::ZERO,
            Duration::ZERO,
            3_600,
        )
        .is_err());
        assert_eq!(
            read_installation_registrations(&registrations).unwrap()[0].1,
            live
        );

        drop(lease);
        // Without advisory ownership evidence, a running PID remains uncertain.
        assert!(wait_for_installation_quiescence(
            &lock_path,
            &registrations,
            "attempt",
            Duration::ZERO,
            Duration::ZERO,
            3_600,
        )
        .is_err());
        assert_eq!(
            read_installation_registrations(&registrations).unwrap()[0].1,
            live
        );
    }
}
