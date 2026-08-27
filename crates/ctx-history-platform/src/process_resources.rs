//! Best-effort process resource headroom for bounded ctx operations.

#[cfg(unix)]
use std::sync::Mutex;

#[cfg(unix)]
const OPEN_FILE_SOFT_LIMIT_TARGET: libc::rlim_t = 4_096;

#[cfg(unix)]
static OPEN_FILE_LIMIT_LOCK: Mutex<()> = Mutex::new(());

/// Best-effort raises the process open-file soft limit to bounded headroom.
///
/// Unix raises are capped by the existing hard limit. Failures are ignored so
/// environments that prohibit `setrlimit` can still run imports that fit their
/// configured limit. Other platforms require no process-level adjustment.
pub fn raise_open_file_soft_limit() {
    #[cfg(unix)]
    raise_unix_open_file_soft_limit();
}

#[cfg(unix)]
fn raise_unix_open_file_soft_limit() {
    let _guard = match OPEN_FILE_LIMIT_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limits) } != 0 {
        return;
    }
    let Some(raised_soft_limit) = raised_open_file_soft_limit(limits.rlim_cur, limits.rlim_max)
    else {
        return;
    };
    limits.rlim_cur = raised_soft_limit;
    let _ = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limits) };
}

#[cfg(unix)]
fn raised_open_file_soft_limit(
    current_soft_limit: libc::rlim_t,
    hard_limit: libc::rlim_t,
) -> Option<libc::rlim_t> {
    let target = hard_limit.min(OPEN_FILE_SOFT_LIMIT_TARGET);
    (current_soft_limit < target).then_some(target)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn open_file_soft_limit_raise_is_capped_by_the_hard_limit() {
        assert_eq!(raised_open_file_soft_limit(64, 1_024), Some(1_024));
        assert_eq!(raised_open_file_soft_limit(64, 8_192), Some(4_096));
    }

    #[test]
    fn open_file_soft_limit_raise_never_lowers_or_exceeds_a_fixed_limit() {
        assert_eq!(raised_open_file_soft_limit(64, 64), None);
        assert_eq!(raised_open_file_soft_limit(4_096, 8_192), None);
        assert_eq!(raised_open_file_soft_limit(8_192, 8_192), None);
    }
}
