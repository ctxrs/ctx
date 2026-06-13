use anyhow::{Context, Result};
use serde::Serialize;

pub const RECOMMENDED_DAEMON_OPEN_FILE_SOFT_LIMIT: u64 = 65_535;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OpenFileLimitSnapshot {
    pub soft: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hard: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFileLimitAdjustment {
    pub before: OpenFileLimitSnapshot,
    pub after: OpenFileLimitSnapshot,
    pub target_soft: u64,
    pub changed: bool,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
struct RawOpenFileLimit {
    soft: libc::rlim_t,
    hard: libc::rlim_t,
}

#[cfg(unix)]
pub fn ensure_min_open_file_limit(min_soft: u64) -> Result<Option<OpenFileLimitAdjustment>> {
    let before_raw = read_raw_open_file_limit()?;
    let before = snapshot_from_raw(before_raw);
    let target_soft = desired_soft_limit(before, min_soft);
    if before.soft >= target_soft {
        return Ok(Some(OpenFileLimitAdjustment {
            before,
            after: before,
            target_soft,
            changed: false,
        }));
    }

    let updated = libc::rlimit {
        rlim_cur: target_soft as libc::rlim_t,
        rlim_max: before_raw.hard,
    };
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &updated) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context(format!(
            "raising RLIMIT_NOFILE soft limit from {} to {}",
            before.soft, target_soft
        ));
    }

    let after = read_open_file_limit()?;
    Ok(Some(OpenFileLimitAdjustment {
        before,
        after,
        target_soft,
        changed: after.soft != before.soft,
    }))
}

#[cfg(not(unix))]
pub fn ensure_min_open_file_limit(_min_soft: u64) -> Result<Option<OpenFileLimitAdjustment>> {
    Ok(None)
}

#[cfg(unix)]
pub fn current_open_file_limit() -> Option<OpenFileLimitSnapshot> {
    read_open_file_limit().ok()
}

#[cfg(not(unix))]
pub fn current_open_file_limit() -> Option<OpenFileLimitSnapshot> {
    None
}

#[cfg(unix)]
fn read_open_file_limit() -> Result<OpenFileLimitSnapshot> {
    read_raw_open_file_limit().map(snapshot_from_raw)
}

#[cfg(unix)]
fn read_raw_open_file_limit() -> Result<RawOpenFileLimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("reading RLIMIT_NOFILE");
    }
    Ok(RawOpenFileLimit {
        soft: limit.rlim_cur,
        hard: limit.rlim_max,
    })
}

#[cfg(unix)]
fn snapshot_from_raw(limit: RawOpenFileLimit) -> OpenFileLimitSnapshot {
    OpenFileLimitSnapshot {
        soft: limit.soft,
        hard: rlim_to_u64(limit.hard),
    }
}

#[cfg(unix)]
fn desired_soft_limit(current: OpenFileLimitSnapshot, min_soft: u64) -> u64 {
    current
        .hard
        .map(|hard| min_soft.min(hard))
        .unwrap_or(min_soft)
}

#[cfg(unix)]
fn rlim_to_u64(value: libc::rlim_t) -> Option<u64> {
    if value == libc::RLIM_INFINITY {
        None
    } else {
        Some(value)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn desired_soft_limit_caps_at_hard_limit() {
        let current = OpenFileLimitSnapshot {
            soft: 256,
            hard: Some(4096),
        };
        assert_eq!(desired_soft_limit(current, 65_535), 4096);
    }

    #[test]
    fn desired_soft_limit_uses_requested_target_when_hard_is_unbounded() {
        let current = OpenFileLimitSnapshot {
            soft: 256,
            hard: None,
        };
        assert_eq!(desired_soft_limit(current, 65_535), 65_535);
    }
}
