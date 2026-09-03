use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context as _, Result};
use serde_json::{json, Map, Value};

use super::{
    is_active_upgrade_status, is_automatic_attempt_source, is_valid_upgrade_attempt_id,
    read_state_object, read_state_object_bounded, write_state_checked_locked,
    write_state_object_locked, UpgradeAttempt, UpgradeLock, UpgradeState,
};
use crate::upgrade::install::InstallationLock;
use crate::upgrade::UpgradePlan;

const ATTEMPT_KEY: &str = "managed_pair_apply";
const DATA_ROOT_KEY: &str = "managed_pair_data_root";
const INTERVAL_KEY: &str = "managed_pair_interval_seconds";
const CORE_SHA256_KEY: &str = "managed_pair_core_sha256";
const ENVELOPE_SHA256_KEY: &str = "managed_pair_envelope_sha256";
const RESTART_TRIGGER_KEY: &str = "managed_pair_restart_trigger";
const RESTART_INTERVAL_KEY: &str = "managed_pair_restart_interval_seconds";
#[cfg(windows)]
const HELPER_PATH_KEY: &str = "managed_pair_helper_path";
#[cfg(windows)]
const HELPER_PARENT_PID_KEY: &str = "managed_pair_helper_parent_pid";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::upgrade) struct ManagedPairRecovery {
    pub(in crate::upgrade) attempt_id: String,
    pub(in crate::upgrade) data_root: PathBuf,
    pub(in crate::upgrade) install_path: PathBuf,
    pub(in crate::upgrade) channel: String,
    pub(in crate::upgrade) interval: Duration,
    pub(in crate::upgrade) automatic: bool,
    pub(in crate::upgrade) restart_trigger: Option<String>,
    pub(in crate::upgrade) restart_interval_seconds: Option<u64>,
    pub(in crate::upgrade) core_sha256: String,
    pub(in crate::upgrade) envelope_sha256: String,
    #[cfg(windows)]
    pub(in crate::upgrade) helper_path: Option<PathBuf>,
    #[cfg(windows)]
    pub(in crate::upgrade) helper_parent_pid: Option<u32>,
}

/// Returns only a bounded scheduler hint. The caller revalidates it after
/// acquiring the canonical installation lock before any mutation.
pub(in crate::upgrade) fn recovery_hint() -> Result<Option<String>> {
    let install_path = crate::upgrade::install::current_install_path()?;
    let Some(state) = read_state_object_bounded(&install_path) else {
        return Ok(None);
    };
    Ok((is_active_upgrade_status(&state.status)
        && state.plan.get(ATTEMPT_KEY) == Some(&Value::Bool(true)))
    .then_some(state.attempt_id)
    .flatten()
    .filter(|attempt_id| is_valid_upgrade_attempt_id(attempt_id)))
}

pub(in crate::upgrade) fn recovery_locked(
    lock: &UpgradeLock,
    expected_attempt_id: &str,
) -> Result<ManagedPairRecovery> {
    recovery_from_state(
        &read_state_object(&lock.install_path),
        &lock.install_path,
        expected_attempt_id,
    )
}

pub(in crate::upgrade) fn acquire_recovery_lock(expected_attempt_id: &str) -> Result<UpgradeLock> {
    let install_path = crate::upgrade::install::current_install_path()?;
    let installation = InstallationLock::try_acquire(&install_path)?.ok_or_else(|| {
        anyhow!("ctx installation upgrade lock is held for pending managed-pair recovery")
    })?;
    let lock = UpgradeLock {
        install_path,
        installation,
    };
    recovery_locked(&lock, expected_attempt_id)?;
    Ok(lock)
}

pub(in crate::upgrade) fn try_acquire_recovery_lock(
    expected_attempt_id: &str,
) -> Result<Option<UpgradeLock>> {
    let install_path = crate::upgrade::install::current_install_path()?;
    let Some(installation) = InstallationLock::try_acquire(&install_path)? else {
        return Ok(None);
    };
    let lock = UpgradeLock {
        install_path,
        installation,
    };
    recovery_locked(&lock, expected_attempt_id)?;
    Ok(Some(lock))
}

#[cfg(windows)]
pub(in crate::upgrade) fn acquire_helper_recovery_lock(
    install_path: &Path,
    expected_attempt_id: &str,
) -> Result<UpgradeLock> {
    let installation = InstallationLock::acquire_for_recovery(install_path)?;
    let lock = UpgradeLock {
        install_path: install_path.to_path_buf(),
        installation,
    };
    recovery_locked(&lock, expected_attempt_id)?;
    Ok(lock)
}

#[cfg(windows)]
pub(in crate::upgrade) fn helper_recovery_hint(
    install_path: &Path,
    expected_attempt_id: &str,
    parent_pid: u32,
) -> Result<Option<ManagedPairRecovery>> {
    let Some(state) = read_state_object_bounded(install_path) else {
        return Ok(None);
    };
    if !is_active_upgrade_status(&state.status)
        || state.plan.get(ATTEMPT_KEY) != Some(&Value::Bool(true))
    {
        return Ok(None);
    }
    let recovery = recovery_from_state(&state, install_path, expected_attempt_id)?;
    if recovery.helper_parent_pid != Some(parent_pid) {
        return Err(anyhow!(
            "Windows managed-pair helper parent does not match scheduler state"
        ));
    }
    let expected_helper = recovery
        .helper_path
        .as_deref()
        .ok_or_else(|| anyhow!("Windows managed-pair recovery has no helper path"))?;
    let current = std::env::current_exe().context("resolve Windows managed-pair helper path")?;
    if !crate::upgrade::install::managed_install_path_identity_matches(&current, expected_helper) {
        return Err(anyhow!(
            "Windows managed-pair helper was not launched from its scheduler-owned copy"
        ));
    }
    validate_helper_file(&current, &recovery.core_sha256)?;
    Ok(Some(recovery))
}

fn recovery_from_state(
    state: &UpgradeState,
    install_path_identity: &Path,
    expected_attempt_id: &str,
) -> Result<ManagedPairRecovery> {
    if !is_active_upgrade_status(&state.status)
        || state.attempt_id.as_deref() != Some(expected_attempt_id)
        || state.plan.get(ATTEMPT_KEY) != Some(&Value::Bool(true))
    {
        return Err(anyhow!(
            "pending managed-pair upgrade does not match its scheduler state"
        ));
    }
    let data_root = required_path(&state.plan, DATA_ROOT_KEY)?;
    let canonical_data_root = fs::canonicalize(&data_root).with_context(|| {
        format!(
            "canonicalize managed-pair data root {}",
            data_root.display()
        )
    })?;
    if canonical_data_root != data_root {
        return Err(anyhow!("managed-pair data root is not canonical"));
    }
    let install_path = required_path(&state.plan, "install_path")?;
    if install_path != install_path_identity {
        return Err(anyhow!(
            "pending managed-pair upgrade targets a different installation"
        ));
    }
    let channel = required_string(&state.plan, "channel")?;
    if !matches!(channel.as_str(), "stable" | "staging") {
        return Err(anyhow!("pending managed-pair upgrade channel is invalid"));
    }
    let interval = Duration::from_secs(required_u64(&state.plan, INTERVAL_KEY)?);
    let restart_trigger = optional_string(&state.plan, RESTART_TRIGGER_KEY)?;
    let restart_interval_seconds = state
        .plan
        .get(RESTART_INTERVAL_KEY)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| anyhow!("pending managed-pair daemon restart interval is invalid"))
        })
        .transpose()?;
    let core_sha256 = required_string(&state.plan, CORE_SHA256_KEY)?;
    if !is_sha256(&core_sha256) {
        return Err(anyhow!("pending managed-pair Core identity is invalid"));
    }
    let envelope_sha256 = required_string(&state.plan, ENVELOPE_SHA256_KEY)?;
    if !is_sha256(&envelope_sha256) {
        return Err(anyhow!(
            "pending managed-pair signed-envelope identity is invalid"
        ));
    }
    #[cfg(windows)]
    let helper_path = state
        .plan
        .get(HELPER_PATH_KEY)
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("pending managed-pair helper path is invalid"))
        })
        .transpose()?;
    #[cfg(windows)]
    let helper_parent_pid = state
        .plan
        .get(HELPER_PARENT_PID_KEY)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value != 0)
                .ok_or_else(|| anyhow!("pending managed-pair helper parent PID is invalid"))
        })
        .transpose()?;
    Ok(ManagedPairRecovery {
        attempt_id: expected_attempt_id.to_owned(),
        data_root,
        install_path,
        channel,
        interval,
        automatic: state
            .attempt_source
            .as_deref()
            .is_some_and(is_automatic_attempt_source),
        restart_trigger,
        restart_interval_seconds,
        core_sha256,
        envelope_sha256,
        #[cfg(windows)]
        helper_path,
        #[cfg(windows)]
        helper_parent_pid,
    })
}

#[cfg(any(windows, test))]
pub(in crate::upgrade) fn validate_helper_file(path: &Path, expected_sha256: &str) -> Result<()> {
    let bytes = crate::upgrade::install::read_stable_file(
        path,
        "Windows managed-pair helper",
        256 * 1024 * 1024,
        crate::upgrade::install::StableFileKind::Executable,
    )?
    .ok_or_else(|| anyhow!("Windows managed-pair helper is absent"))?;
    if crate::upgrade::sha256_hex(&bytes) != expected_sha256 {
        return Err(anyhow!(
            "Windows managed-pair helper does not match the signed candidate Core"
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::upgrade) fn write_attempt_locked(
    data_root: &Path,
    lock: &UpgradeLock,
    attempt: &UpgradeAttempt,
    plan: &UpgradePlan,
    status: &str,
    interval: Duration,
    restart: Option<(&str, Option<u64>)>,
    envelope_sha256: &str,
    #[cfg(windows)] helper_path: Option<&Path>,
) -> Result<bool> {
    if !is_sha256(envelope_sha256) {
        return Err(anyhow!("managed-pair signed-envelope identity is invalid"));
    }
    if !write_state_checked_locked(data_root, lock, attempt, plan, status, interval)? {
        return Ok(false);
    }
    let mut state = read_state_object(&lock.install_path);
    if !state.is_current(attempt) {
        return Ok(false);
    }
    let data_root = fs::canonicalize(data_root).with_context(|| {
        format!(
            "canonicalize managed-pair data root {}",
            data_root.display()
        )
    })?;
    state.plan.insert(ATTEMPT_KEY.to_owned(), Value::Bool(true));
    state
        .plan
        .insert(DATA_ROOT_KEY.to_owned(), json!(data_root));
    state
        .plan
        .insert(INTERVAL_KEY.to_owned(), json!(interval.as_secs()));
    let release = plan
        .managed_pair_release
        .as_ref()
        .ok_or_else(|| anyhow!("managed-pair scheduler state requires pair metadata"))?;
    state
        .plan
        .insert(CORE_SHA256_KEY.to_owned(), json!(&release.core_sha256));
    state.plan.insert(
        ENVELOPE_SHA256_KEY.to_owned(),
        json!(envelope_sha256.to_ascii_lowercase()),
    );
    set_optional_string(
        &mut state.plan,
        RESTART_TRIGGER_KEY,
        restart.map(|(trigger, _)| trigger),
    );
    match restart.and_then(|(_, interval)| interval) {
        Some(interval) => {
            state
                .plan
                .insert(RESTART_INTERVAL_KEY.to_owned(), json!(interval));
        }
        None => {
            state.plan.remove(RESTART_INTERVAL_KEY);
        }
    }
    #[cfg(windows)]
    match helper_path {
        Some(helper_path) => {
            state
                .plan
                .insert(HELPER_PATH_KEY.to_owned(), json!(helper_path));
            state
                .plan
                .insert(HELPER_PARENT_PID_KEY.to_owned(), json!(std::process::id()));
        }
        None => {
            state.plan.remove(HELPER_PATH_KEY);
            state.plan.remove(HELPER_PARENT_PID_KEY);
        }
    }
    write_state_object_locked(lock, state)?;
    Ok(true)
}

#[cfg(windows)]
pub(in crate::upgrade) fn update_helper_parent_locked(
    lock: &UpgradeLock,
    attempt: &UpgradeAttempt,
) -> Result<()> {
    let mut state = read_state_object(&lock.install_path);
    if !state.is_current(attempt)
        || state.plan.get(ATTEMPT_KEY) != Some(&Value::Bool(true))
        || !state.plan.contains_key(HELPER_PATH_KEY)
    {
        return Err(anyhow!(
            "pending managed-pair helper does not match its scheduler attempt"
        ));
    }
    state.status = "scheduled".to_owned();
    state
        .plan
        .insert(HELPER_PARENT_PID_KEY.to_owned(), json!(std::process::id()));
    write_state_object_locked(lock, state)
}

pub(super) fn clear_attempt(plan: &mut Map<String, Value>) {
    for key in [
        ATTEMPT_KEY,
        DATA_ROOT_KEY,
        INTERVAL_KEY,
        CORE_SHA256_KEY,
        ENVELOPE_SHA256_KEY,
        RESTART_TRIGGER_KEY,
        RESTART_INTERVAL_KEY,
        #[cfg(windows)]
        HELPER_PATH_KEY,
        #[cfg(windows)]
        HELPER_PARENT_PID_KEY,
    ] {
        plan.remove(key);
    }
}

fn required_string(plan: &Map<String, Value>, key: &str) -> Result<String> {
    plan.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("pending managed-pair scheduler field {key} is invalid"))
}

fn optional_string(plan: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    plan.get(key)
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty() && value.len() <= 128)
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("pending managed-pair scheduler field {key} is invalid"))
        })
        .transpose()
}

fn required_path(plan: &Map<String, Value>, key: &str) -> Result<PathBuf> {
    required_string(plan, key).map(PathBuf::from)
}

fn required_u64(plan: &Map<String, Value>, key: &str) -> Result<u64> {
    plan.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("pending managed-pair scheduler field {key} is invalid"))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn set_optional_string(plan: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    match value {
        Some(value) => {
            plan.insert(key.to_owned(), json!(value));
        }
        None => {
            plan.remove(key);
        }
    }
}
