use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use ctx_history_core::utc_now;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::semantic::{
    health_search::{create_private_dir_all, secure_private_file_permissions},
    query_service::daemon_source_refresh_request,
};
use crate::{compact_json, pro::selected_helper_artifact_sha256};

use super::{
    daemon_jobs_path, write_daemon_job_status, SOURCE_BACKED_PRO_CATCH_UP_WAKE_RESPONSE_MAX_BYTES,
    SOURCE_BACKED_PRO_CATCH_UP_WAKE_TIMEOUT,
};

const RECHECK_FILE: &str = "pro-catch-up-recheck.json";
const RECHECK_LOCK_FILE: &str = "pro-catch-up-recheck.lock";
const RECHECK_SCHEMA_VERSION: u16 = 2;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HelperRecheckIntent {
    schema_version: u16,
    request_id: Uuid,
    target_helper_sha256: String,
    requested_at_ms: i64,
}

impl HelperRecheckIntent {
    fn new(target_helper_sha256: &str) -> Result<Self> {
        validate_sha256(target_helper_sha256)?;
        Ok(Self {
            schema_version: RECHECK_SCHEMA_VERSION,
            request_id: Uuid::now_v7(),
            target_helper_sha256: target_helper_sha256.to_owned(),
            requested_at_ms: utc_now().timestamp_millis(),
        })
    }

    fn validate(self) -> Result<Self> {
        if self.schema_version != RECHECK_SCHEMA_VERSION || self.request_id.is_nil() {
            anyhow::bail!("invalid source-backed Pro catch-up recheck identity");
        }
        validate_sha256(&self.target_helper_sha256)?;
        Ok(self)
    }

    pub(super) fn target_helper_sha256(&self) -> &str {
        &self.target_helper_sha256
    }
}

pub(crate) struct HelperRecheckSchedule {
    pub(crate) attempt_key: String,
    pub(crate) target_ready: bool,
}

pub(crate) fn publish(data_root: &Path, target_helper_sha256: &str) -> Result<()> {
    let next = HelperRecheckIntent::new(target_helper_sha256)?;
    with_lock(data_root, || {
        if read_unlocked(data_root)?
            .is_some_and(|current| current.target_helper_sha256 == next.target_helper_sha256)
        {
            return Ok(());
        }
        write_daemon_job_status(&path(data_root), &compact_json(serde_json::to_value(next)?))
    })
}

pub(crate) fn targets(data_root: &Path, target_helper_sha256: &str) -> Result<bool> {
    validate_sha256(target_helper_sha256)?;
    Ok(read(data_root)?.is_some_and(|intent| intent.target_helper_sha256 == target_helper_sha256))
}

pub(crate) fn wake(data_root: &Path) {
    let _ = daemon_source_refresh_request(
        data_root,
        json!({"schema_version": 1, "op": "lifecycle_wakeup"}),
        SOURCE_BACKED_PRO_CATCH_UP_WAKE_TIMEOUT,
        SOURCE_BACKED_PRO_CATCH_UP_WAKE_RESPONSE_MAX_BYTES,
    );
}

pub(crate) fn schedule(data_root: &Path) -> Result<Option<HelperRecheckSchedule>> {
    let Some(intent) = read(data_root)? else {
        return Ok(None);
    };
    let installed = selected_helper_artifact_sha256(data_root)?;
    let target_ready = installed.as_deref() == Some(intent.target_helper_sha256());
    Ok(Some(HelperRecheckSchedule {
        attempt_key: format!(
            "{}:{}",
            intent.request_id,
            installed.as_deref().unwrap_or("missing")
        ),
        target_ready,
    }))
}

pub(super) fn complete(
    data_root: &Path,
    observed: Option<&HelperRecheckIntent>,
    completed_helper_sha256: &str,
) -> Result<bool> {
    let Some(observed) = observed else {
        return Ok(false);
    };
    with_lock(data_root, || {
        let current = read_unlocked(data_root)?;
        let matches = current.as_ref().is_some_and(|current| {
            current.request_id == observed.request_id
                && current.target_helper_sha256 == completed_helper_sha256
                && observed.target_helper_sha256 == completed_helper_sha256
        });
        if matches {
            fs::remove_file(path(data_root)).with_context(|| {
                format!(
                    "complete Pro catch-up recheck {}",
                    path(data_root).display()
                )
            })?;
        }
        Ok(matches)
    })
}

pub(super) fn read(data_root: &Path) -> Result<Option<HelperRecheckIntent>> {
    if !path(data_root).exists() {
        return Ok(None);
    }
    with_lock(data_root, || read_unlocked(data_root))
}

pub(super) fn read_unlocked(data_root: &Path) -> Result<Option<HelperRecheckIntent>> {
    let path = path(data_root);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("read Pro recheck {}", path.display()))
        }
    };
    serde_json::from_slice::<HelperRecheckIntent>(&bytes)
        .with_context(|| format!("parse Pro recheck {}", path.display()))?
        .validate()
        .map(Some)
}

pub(super) fn with_lock<T>(data_root: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let jobs = daemon_jobs_path(data_root);
    create_private_dir_all(&jobs)?;
    let lock_path = lock_path(data_root);
    let (lock, _) = super::super::paths_status::open_or_create_pid_lock_file(&lock_path)
        .with_context(|| format!("open Pro recheck lock {}", lock_path.display()))?;
    secure_private_file_permissions(&lock_path)?;
    fs2::FileExt::lock_exclusive(&lock)
        .with_context(|| format!("lock Pro recheck {}", lock_path.display()))?;
    let result = operation();
    let unlock = fs2::FileExt::unlock(&lock)
        .with_context(|| format!("unlock Pro recheck {}", lock_path.display()));
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

pub(super) fn path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(RECHECK_FILE)
}

fn lock_path(data_root: &Path) -> PathBuf {
    daemon_jobs_path(data_root).join(RECHECK_LOCK_FILE)
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        anyhow::bail!("invalid Pro helper SHA-256 identity");
    }
    Ok(())
}
