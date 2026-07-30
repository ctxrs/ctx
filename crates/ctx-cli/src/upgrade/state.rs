use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::{
    platform_security::{
        establish_private_data_root, restrict_private_file_handle, verify_private_directory,
        verify_private_file_handle,
    },
    utc_now,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{
    install::{validate_recovery_observation, InstallationLock, PendingRecovery},
    UpgradePlan,
};

pub(super) const STATE_FILE: &str = "upgrade-state.json";
pub(super) const STATE_SCHEMA_VERSION: u64 = 1;
const DAEMON_QUIESCENCE_LOCK_FILE: &str = "daemon-quiescence.lock";
const DAEMON_QUIESCENCE_ACK_DIR: &str = "daemon-quiescence-acks";
const INITIAL_FAILURE_BACKOFF: Duration = Duration::from_secs(60);
const MAX_FAILURE_BACKOFF: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UpgradeAttempt {
    id: String,
}

impl UpgradeAttempt {
    pub(super) fn id(&self) -> &str {
        &self.id
    }
}

pub(crate) fn is_valid_upgrade_attempt_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

pub(super) enum AutoUpgradeClaim {
    Claimed {
        attempt: UpgradeAttempt,
        lock: UpgradeLock,
    },
    NotDue,
    Contended,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallationUpgradeObservation {
    Active,
    Inactive,
    Missing,
    Untrusted,
}

/// One scheduler record beside the installed executable. The live
/// executable-scoped OS lock, not a persisted lease or generation, is the
/// authority for an in-progress attempt.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct UpgradeState {
    #[serde(default)]
    schema_version: u64,
    #[serde(default)]
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempt_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_attempt_finished_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checked_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_checked_unix_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_successful_check_unix_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_check_unix_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_retry_unix_s: Option<u64>,
    #[serde(default)]
    consecutive_failures: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(flatten)]
    plan: Map<String, Value>,
}

impl UpgradeState {
    fn valid_or_default(self) -> Self {
        if self.schema_version == STATE_SCHEMA_VERSION {
            self
        } else {
            Self::default()
        }
    }

    fn begin(&mut self, source: &str) -> UpgradeAttempt {
        let attempt = UpgradeAttempt {
            id: format!("ua_{}", Uuid::now_v7()),
        };
        self.schema_version = STATE_SCHEMA_VERSION;
        self.status = "checking".to_owned();
        self.attempt_id = Some(attempt.id.clone());
        self.attempt_source = Some(source.to_owned());
        self.last_attempt_at = Some(utc_now());
        self.last_attempt_finished_at = None;
        self.error = None;
        attempt
    }

    fn is_current(&self, attempt: &UpgradeAttempt) -> bool {
        self.attempt_id.as_deref() == Some(attempt.id())
    }

    fn begin_recovery(&mut self, attempt_id: &str, source: &str) -> UpgradeAttempt {
        let same_attempt = self.attempt_id.as_deref() == Some(attempt_id);
        self.schema_version = STATE_SCHEMA_VERSION;
        self.status = "recovering".to_owned();
        self.attempt_id = Some(attempt_id.to_owned());
        if !same_attempt || self.attempt_source.is_none() {
            self.attempt_source = Some(source.to_owned());
        }
        if !same_attempt || self.last_attempt_at.is_none() {
            self.last_attempt_at = Some(utc_now());
        }
        self.last_attempt_finished_at = None;
        self.error = None;
        UpgradeAttempt {
            id: attempt_id.to_owned(),
        }
    }

    fn terminal(&mut self, attempt: &UpgradeAttempt, status: &str, interval: Duration, now: u64) {
        self.status = status.to_owned();
        self.attempt_id = Some(attempt.id.clone());
        self.last_attempt_finished_at = Some(utc_now());
        self.error = None;
        if self.attempt_source.as_deref() == Some("daemon") {
            self.last_successful_check_unix_s = Some(now);
            self.next_check_unix_s = Some(now.saturating_add(interval.as_secs()));
            self.next_retry_unix_s = None;
            self.consecutive_failures = 0;
        }
    }

    fn fail(&mut self, attempt: &UpgradeAttempt, error: &str, now: u64) {
        self.status = "error".to_owned();
        self.attempt_id = Some(attempt.id.clone());
        self.last_attempt_finished_at = Some(utc_now());
        self.checked_at = Some(utc_now());
        self.last_checked_unix_s = Some(now);
        self.error = Some(error.to_owned());
        if self.attempt_source.as_deref() == Some("daemon") {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
            self.next_retry_unix_s =
                Some(now.saturating_add(failure_backoff(self.consecutive_failures).as_secs()));
        }
    }
}

pub(super) fn claim_daemon_auto_upgrade(interval: Duration) -> Result<AutoUpgradeClaim> {
    let Some(lock) = UpgradeLock::try_acquire()? else {
        return Ok(AutoUpgradeClaim::Contended);
    };
    let mut state = read_state_object(&lock.install_path);
    let now = now_unix_s();
    if !auto_check_due(&state, interval, now) {
        return Ok(AutoUpgradeClaim::NotDue);
    }
    let attempt = state.begin("daemon");
    write_state_object_locked(&lock, state)?;
    Ok(AutoUpgradeClaim::Claimed { attempt, lock })
}

pub(crate) fn installation_upgrade_is_active() -> Result<bool> {
    let install_path = super::install::current_install_path()?;
    installation_upgrade_is_active_for(&install_path)
}

fn installation_upgrade_is_active_for(install_path: &Path) -> Result<bool> {
    if observe_installation_upgrade(install_path) == InstallationUpgradeObservation::Active {
        return Ok(true);
    }
    if InstallationLock::try_acquire(install_path)?.is_some() {
        return Ok(false);
    }
    // Close the state/lock race with one observation after contention. A
    // current-format owner publishes an active phase while holding this lock
    // before daemon quiescence or installation mutation. A record that remains
    // absent therefore describes either that pre-mutation window or another
    // read-only checker, while malformed and unknown records remain fail-closed.
    Ok(matches!(
        observe_installation_upgrade(install_path),
        InstallationUpgradeObservation::Active | InstallationUpgradeObservation::Untrusted
    ))
}

fn observe_installation_upgrade(install_path: &Path) -> InstallationUpgradeObservation {
    let bytes = match fs::read(state_path(install_path)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return InstallationUpgradeObservation::Missing;
        }
        Err(_) => return InstallationUpgradeObservation::Untrusted,
    };
    let Ok(state) = serde_json::from_slice::<UpgradeState>(&bytes) else {
        return InstallationUpgradeObservation::Untrusted;
    };
    if state.schema_version != STATE_SCHEMA_VERSION {
        return InstallationUpgradeObservation::Untrusted;
    }
    if is_active_upgrade_status(&state.status) {
        InstallationUpgradeObservation::Active
    } else {
        InstallationUpgradeObservation::Inactive
    }
}

pub(crate) fn active_installation_upgrade_attempt_id() -> Result<Option<String>> {
    let install_path = super::install::current_install_path()?;
    let state = fs::read(state_path(&install_path))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<UpgradeState>(&bytes).ok())
        .filter(|state| state.schema_version == STATE_SCHEMA_VERSION);
    let active_attempt = state.as_ref().and_then(|state| {
        is_active_upgrade_status(&state.status)
            .then(|| state.attempt_id.clone())
            .flatten()
    });
    if active_attempt.is_some() {
        return Ok(active_attempt);
    }
    if InstallationLock::try_acquire(&install_path)?.is_none() {
        return Err(anyhow!(
            "ctx installation is locked without readable active upgrade state"
        ));
    }
    Ok(None)
}

fn is_active_upgrade_status(status: &str) -> bool {
    matches!(
        status,
        "staged" | "quiescing" | "applying" | "scheduled" | "recovering"
    )
}

pub(crate) fn terminal_installation_upgrade_attempt_id() -> Result<Option<String>> {
    let install_path = super::install::current_install_path()?;
    let state = read_state_object(&install_path);
    Ok((state.schema_version == STATE_SCHEMA_VERSION
        && matches!(
            state.status.as_str(),
            "applied" | "disabled" | "dry_run" | "error" | "up_to_date"
        ))
    .then_some(state.attempt_id)
    .flatten())
}

pub(crate) fn installation_daemon_coordination_paths() -> Result<(PathBuf, PathBuf)> {
    let install_path = super::install::current_install_path()?;
    Ok(installation_daemon_coordination_paths_for(&install_path))
}

pub(crate) fn installation_executable_path() -> Result<PathBuf> {
    super::install::current_install_path()
}

pub(crate) fn installation_daemon_coordination_paths_for(
    install_path: &Path,
) -> (PathBuf, PathBuf) {
    let name = install_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("ctx");
    (
        install_path.with_file_name(format!(".{name}.{DAEMON_QUIESCENCE_LOCK_FILE}")),
        install_path.with_file_name(format!(".{name}.{DAEMON_QUIESCENCE_ACK_DIR}")),
    )
}

pub(super) fn begin_manual_attempt_locked(
    _data_root: &Path,
    lock: &UpgradeLock,
    source: &str,
) -> Result<UpgradeAttempt> {
    let mut state = read_state_object(&lock.install_path);
    let attempt = state.begin(source);
    write_state_object_locked(lock, state)?;
    Ok(attempt)
}

pub(super) fn begin_recovery_attempt_locked(
    lock: &UpgradeLock,
    attempt_id: &str,
    source: &str,
) -> Result<UpgradeAttempt> {
    if !is_valid_upgrade_attempt_id(attempt_id) {
        return Err(anyhow!("invalid interrupted upgrade attempt identity"));
    }
    let mut state = read_state_object(&lock.install_path);
    let attempt = state.begin_recovery(attempt_id, source);
    write_state_object_locked(lock, state)?;
    Ok(attempt)
}

pub(super) fn write_state_phase_locked(
    lock: &UpgradeLock,
    attempt: &UpgradeAttempt,
    phase: &str,
) -> Result<bool> {
    if phase == "applying"
        && crate::upgrade::test_harness_enabled()
        && super::env_flag("CTX_UPGRADE_FAIL_APPLYING_STATE_WRITE_FOR_TESTS")
    {
        return Err(anyhow!("injected applying-state write failure"));
    }
    let mut state = read_state_object(&lock.install_path);
    if !state.is_current(attempt) {
        return Ok(false);
    }
    state.status = phase.to_owned();
    write_state_object_locked(lock, state)?;
    Ok(true)
}

pub(super) fn write_state_checked_locked(
    _data_root: &Path,
    lock: &UpgradeLock,
    attempt: &UpgradeAttempt,
    plan: &UpgradePlan,
    status: &str,
    interval: Duration,
) -> Result<bool> {
    if crate::upgrade::test_harness_enabled()
        && super::env_flag("CTX_UPGRADE_FAIL_STATE_WRITE_FOR_TESTS")
    {
        return Err(anyhow!("injected upgrade state write failure"));
    }
    let mut state = read_state_object(&lock.install_path);
    if !state.is_current(attempt) {
        return Ok(false);
    }
    let now = now_unix_s();
    state.checked_at = Some(utc_now());
    state.last_checked_unix_s = Some(now);
    if is_active_upgrade_status(status) {
        state.status = status.to_owned();
    } else {
        state.terminal(attempt, status, interval, now);
    }
    write_plan(&mut state, plan, status == "applied");
    write_state_object_locked(lock, state)?;
    Ok(true)
}

pub(super) fn write_state_error_locked(
    _data_root: &Path,
    lock: &UpgradeLock,
    attempt: &UpgradeAttempt,
    _outcome: &str,
    error: &str,
) -> Result<bool> {
    let mut state = read_state_object(&lock.install_path);
    if !state.is_current(attempt) {
        return Ok(false);
    }
    state.fail(attempt, error, now_unix_s());
    write_state_object_locked(lock, state)?;
    Ok(true)
}

pub(super) fn reconcile_replacement_terminal_locked(
    lock: &UpgradeLock,
    attempt_id: &str,
    applied: bool,
    warning_or_error: Option<&str>,
    interval: Duration,
) -> Result<bool> {
    let mut state = read_state_object(&lock.install_path);
    let automatic = state.attempt_id.as_deref() == Some(attempt_id)
        && state.attempt_source.as_deref() == Some("daemon");
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

fn write_plan(state: &mut UpgradeState, plan: &UpgradePlan, applied: bool) {
    state.plan.insert(
        "current_version".to_owned(),
        json!(if applied {
            &plan.latest_version
        } else {
            &plan.current_version
        }),
    );
    state
        .plan
        .insert("latest_version".to_owned(), json!(plan.latest_version));
    state
        .plan
        .insert("metadata_url".to_owned(), json!(plan.metadata_url));
    state
        .plan
        .insert("artifact_url".to_owned(), json!(plan.artifact_url));
    state.plan.insert(
        "update_available".to_owned(),
        json!(!applied && plan.update_available),
    );
    state.plan.insert(
        "update_was_available".to_owned(),
        json!(plan.update_available),
    );
    state.plan.insert("channel".to_owned(), json!(plan.channel));
    state
        .plan
        .insert("platform".to_owned(), json!(plan.platform));
    state
        .plan
        .insert("install_path".to_owned(), json!(plan.install_path));
    state.plan.insert("managed".to_owned(), json!(plan.managed));
    state.plan.insert(
        "self_upgrade_allowed".to_owned(),
        json!(plan.metadata.self_upgrade_allowed),
    );
    state.plan.insert(
        "auto_upgrade_allowed".to_owned(),
        json!(plan.metadata.auto_upgrade_allowed),
    );
}

fn auto_check_due(state: &UpgradeState, interval: Duration, now: u64) -> bool {
    if state
        .next_retry_unix_s
        .is_some_and(|deadline| now < deadline)
    {
        return false;
    }
    if state.consecutive_failures > 0 {
        return true;
    }
    if interval.is_zero() {
        return true;
    }
    !state
        .next_check_unix_s
        .is_some_and(|deadline| now < deadline)
}

fn failure_backoff(consecutive_failures: u64) -> Duration {
    let exponent = consecutive_failures.saturating_sub(1).min(16) as u32;
    let seconds = INITIAL_FAILURE_BACKOFF
        .as_secs()
        .saturating_mul(1_u64 << exponent)
        .min(MAX_FAILURE_BACKOFF.as_secs());
    Duration::from_secs(seconds)
}

fn state_path(install_path: &Path) -> PathBuf {
    let name = install_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("ctx");
    install_path.with_file_name(format!(".{name}.{STATE_FILE}"))
}

fn read_state_object(install_path: &Path) -> UpgradeState {
    fs::read(state_path(install_path))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<UpgradeState>(&bytes).ok())
        .map(UpgradeState::valid_or_default)
        .unwrap_or_default()
}

pub(super) fn read_state_json() -> Option<Value> {
    let install_path = super::install::current_install_path().ok()?;
    read_state_json_for_path(&install_path)
}

fn read_state_json_for_path(install_path: &Path) -> Option<Value> {
    read_json_file(&state_path(install_path))
}

fn write_state_object_locked(lock: &UpgradeLock, state: UpgradeState) -> Result<()> {
    atomic_write_json(
        &state_path(&lock.install_path),
        &serde_json::to_value(state)?,
    )
}

pub(super) fn read_json_file(path: &Path) -> Option<Value> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

pub(super) fn atomic_write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", Uuid::new_v4().simple()));
    let body = serde_json::to_vec_pretty(value)?;
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        file.write_all(&body)
            .with_context(|| format!("write {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", tmp.display()))?;
        drop(file);
        replace_state_file(&tmp, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(not(windows))]
fn replace_state_file(temporary: &Path, target: &Path) -> Result<()> {
    fs::rename(temporary, target)
        .with_context(|| format!("rename {} to {}", temporary.display(), target.display()))
}

#[cfg(windows)]
fn replace_state_file(temporary: &Path, target: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temporary_wide = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let moved = unsafe {
        MoveFileExW(
            temporary_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("replace {}", target.display()));
    }
    Ok(())
}

pub(super) struct UpgradeLock {
    install_path: PathBuf,
    installation: InstallationLock,
}

impl UpgradeLock {
    pub(super) fn acquire(_data_root: &Path) -> Result<Self> {
        let install_path = super::install::current_install_path()?;
        let installation = InstallationLock::try_acquire(&install_path)?.ok_or_else(|| {
            anyhow!(
                "ctx installation upgrade lock is held for {}",
                install_path.display()
            )
        })?;
        Ok(Self {
            install_path,
            installation,
        })
    }

    pub(super) fn try_acquire() -> Result<Option<Self>> {
        let install_path = super::install::current_install_path()?;
        let Some(installation) = InstallationLock::try_acquire(&install_path)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            install_path,
            installation,
        }))
    }

    pub(super) fn acquire_recovery(expected: &PendingRecovery) -> Result<Self> {
        Self::try_acquire_recovery(expected)?.ok_or_else(|| {
            anyhow!("ctx installation upgrade lock is held for interrupted recovery")
        })
    }

    pub(super) fn try_acquire_recovery(expected: &PendingRecovery) -> Result<Option<Self>> {
        pause_after_recovery_discovery_for_test()?;
        let Some(installation) =
            InstallationLock::try_acquire_for_recovery(&expected.install_path)?
        else {
            return Ok(None);
        };
        if let Err(error) = validate_recovery_observation(expected, false, &installation) {
            pause_after_stale_recovery_rejection_for_test()?;
            return Err(error);
        }
        Ok(Some(Self {
            install_path: expected.install_path.clone(),
            installation,
        }))
    }

    pub(super) fn acquire_terminal_recovery(expected: &PendingRecovery) -> Result<Self> {
        Self::try_acquire_terminal_recovery(expected)?
            .ok_or_else(|| anyhow!("ctx installation upgrade lock is held for terminal recovery"))
    }

    pub(super) fn try_acquire_terminal_recovery(
        expected: &PendingRecovery,
    ) -> Result<Option<Self>> {
        let Some(installation) =
            InstallationLock::try_acquire_for_recovery(&expected.install_path)?
        else {
            return Ok(None);
        };
        validate_recovery_observation(expected, true, &installation)?;
        Ok(Some(Self {
            install_path: expected.install_path.clone(),
            installation,
        }))
    }

    pub(super) fn installation(&self) -> &InstallationLock {
        &self.installation
    }
}

fn pause_after_recovery_discovery_for_test() -> Result<()> {
    if !crate::upgrade::test_harness_enabled() {
        return Ok(());
    }
    let Some(path) = std::env::var_os("CTX_UPGRADE_PAUSE_AFTER_RECOVERY_DISCOVERY_FOR_TESTS")
    else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    fs::write(&path, b"ready\n")?;
    let release = path.with_extension("continue");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !release.exists() {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting to continue after test recovery discovery"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

fn pause_after_stale_recovery_rejection_for_test() -> Result<()> {
    if !crate::upgrade::test_harness_enabled() {
        return Ok(());
    }
    let Some(path) = std::env::var_os("CTX_UPGRADE_PAUSE_AFTER_STALE_RECOVERY_FOR_TESTS") else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    fs::write(&path, b"rejected\n")?;
    let release = path.with_extension("continue");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !release.exists() {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting to continue after test stale recovery rejection"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

pub(super) fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn set_auto_mode(data_root: &Path, mode: &str) -> Result<()> {
    establish_private_data_root(data_root)
        .with_context(|| format!("protect private upgrade data root {}", data_root.display()))?;
    verify_private_directory(data_root)
        .with_context(|| format!("verify private upgrade data root {}", data_root.display()))?;
    let config_path = data_root.join(crate::config::CONFIG_FILE);
    let existing = read_upgrade_config(&config_path)?;
    let next = set_toml_section_value(&existing, "upgrade", "auto", &format!("\"{mode}\""));
    write_private_config(&config_path, next.as_bytes())?;
    Ok(())
}

fn read_upgrade_config(path: &Path) -> Result<String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        };

        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
    };
    verify_private_file_handle(&file)
        .with_context(|| format!("verify private upgrade config {}", path.display()))?;
    let mut existing = String::new();
    file.read_to_string(&mut existing)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(existing)
}

fn write_private_config(path: &Path, body: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("tmp.{}", Uuid::new_v4().simple()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;

            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            use windows_sys::Win32::{
                Foundation::{GENERIC_READ, GENERIC_WRITE},
                Storage::FileSystem::{
                    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, READ_CONTROL, WRITE_DAC,
                },
            };

            options
                .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        restrict_private_file_handle(&file)
            .with_context(|| format!("protect {}", temporary.display()))?;
        verify_private_file_handle(&file)
            .with_context(|| format!("verify {}", temporary.display()))?;
        file.write_all(body)
            .with_context(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", temporary.display()))?;
        drop(file);
        replace_state_file(&temporary, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn set_toml_section_value(input: &str, section: &str, key: &str, value: &str) -> String {
    let mut lines = Vec::new();
    let mut in_section = false;
    let mut saw_section = false;
    let mut wrote_key = false;
    for raw in input.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_section && !wrote_key {
                lines.push(format!("{key} = {value}"));
                wrote_key = true;
            }
            in_section = trimmed == format!("[{section}]");
            saw_section |= in_section;
            lines.push(raw.to_owned());
            continue;
        }
        if in_section
            && (trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")))
        {
            lines.push(format!("{key} = {value}"));
            wrote_key = true;
        } else {
            lines.push(raw.to_owned());
        }
    }
    if saw_section {
        if in_section && !wrote_key {
            lines.push(format!("{key} = {value}"));
        }
    } else {
        if !lines.is_empty() && lines.last().is_some_and(|line| !line.is_empty()) {
            lines.push(String::new());
        }
        lines.push(format!("[{section}]"));
        lines.push(format!("{key} = {value}"));
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests;
