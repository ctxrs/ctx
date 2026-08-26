use std::{
    io,
    os::unix::process::ExitStatusExt,
    process::{Child, ExitStatus},
};

#[derive(Debug)]
pub(super) struct IsolatedProcessUsage {
    pub(super) status: ExitStatus,
    pub(super) peak_rss_bytes: u64,
    pub(super) process_cpu_seconds: f64,
    pub(super) filesystem_read_block_operations: u64,
    pub(super) filesystem_write_block_operations: u64,
}

fn timeval_seconds(value: libc::timeval) -> f64 {
    value.tv_sec as f64 + value.tv_usec as f64 / 1_000_000.0
}

/// Reaps a dedicated Linux operation child and returns its process-scoped
/// resource usage. The prepared index and all output/accounting work live in
/// the parent; only symmetric harness setup and the tiny report surround the
/// measured republish/republish operation in the child lifetime.
pub(super) fn wait4_operation(child: &Child) -> io::Result<IsolatedProcessUsage> {
    let pid = libc::pid_t::try_from(child.id())
        .map_err(|_| io::Error::other("qualification child PID does not fit pid_t"))?;
    let mut status = 0;
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `pid` names the live child created by this process, `status` and
    // `usage` are writable, and no other code waits for this child.
    let waited = unsafe { libc::wait4(pid, &mut status, 0, usage.as_mut_ptr()) };
    if waited == -1 {
        return Err(io::Error::last_os_error());
    }
    if waited != pid {
        return Err(io::Error::other("wait4 reaped an unexpected child"));
    }
    // SAFETY: successful `wait4` initialized the complete rusage value.
    let usage = unsafe { usage.assume_init() };
    let peak_rss_kib = u64::try_from(usage.ru_maxrss)
        .map_err(|_| io::Error::other("wait4 returned negative peak RSS"))?;
    Ok(IsolatedProcessUsage {
        status: ExitStatus::from_raw(status),
        peak_rss_bytes: peak_rss_kib.saturating_mul(1024),
        process_cpu_seconds: timeval_seconds(usage.ru_utime) + timeval_seconds(usage.ru_stime),
        filesystem_read_block_operations: u64::try_from(usage.ru_inblock)
            .map_err(|_| io::Error::other("wait4 returned negative input block operations"))?,
        filesystem_write_block_operations: u64::try_from(usage.ru_oublock)
            .map_err(|_| io::Error::other("wait4 returned negative output block operations"))?,
    })
}
