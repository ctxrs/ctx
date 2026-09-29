//! Passive accounting for one process, excluding its children.

/// An OS process incarnation. Raw fields and general serialization are private;
/// a daemon lock token can be checked without exposing them to report consumers.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ProcessCreationIdentity {
    pid: u32,
    started: u64,
    boot_id: Option<uuid::Uuid>,
}

impl ProcessCreationIdentity {
    /// Compare against the internal token captured in a daemon lock. Missing,
    /// malformed, foreign or boot-unbound tokens never authenticate ownership.
    /// This performs no I/O; boot identity was captured with the observation.
    pub fn matches_private_json_token(&self, token: &serde_json::Value) -> bool {
        self.private_json_token()
            .is_some_and(|expected| expected == *token)
    }

    /// Persistence is restricted to the daemon's own lock payload, not reports.
    pub(crate) fn private_json_token(&self) -> Option<serde_json::Value> {
        if !cfg!(any(target_os = "linux", target_os = "macos", windows))
            || (cfg!(any(target_os = "linux", target_os = "macos")) && self.boot_id.is_none())
        {
            return None;
        }
        Some(serde_json::json!({
            "schema_version": 1,
            "platform": std::env::consts::OS,
            "pid": self.pid,
            "started": self.started,
            "boot_id": self.boot_id.map(|id| id.to_string()),
        }))
    }
}

impl std::fmt::Debug for ProcessCreationIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessCreationIdentity")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessCpuObservation {
    pub identity: ProcessCreationIdentity,
    pub user_cpu_us: u64,
    pub system_cpu_us: u64,
}

/// Closed reasons only: OS messages, process names and paths never escape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessCpuUnavailable {
    Unsupported,
    PermissionDenied,
    NotRunning,
    Unavailable,
}

/// Read cumulative user and system CPU microseconds with their creation identity.
/// Callers must compare identities and use checked subtraction on both counters
/// before combining deltas. A smaller counter is a reset, never measured zero.
pub fn observe_process_cpu(pid: u32) -> Result<ProcessCpuObservation, ProcessCpuUnavailable> {
    if pid == 0 {
        return Err(ProcessCpuUnavailable::NotRunning);
    }
    let mut observation = observe(pid)?;
    observation.identity.boot_id = current_boot_id();
    Ok(observation)
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn parse_boot_id(bytes: &[u8]) -> Option<uuid::Uuid> {
    if bytes.len() > 64 {
        return None;
    }
    uuid::Uuid::parse_str(std::str::from_utf8(bytes).ok()?.trim())
        .ok()
        .filter(|id| !id.is_nil())
}

#[cfg(target_os = "linux")]
fn current_boot_id() -> Option<uuid::Uuid> {
    use std::io::Read;

    let mut bytes = Vec::new();
    std::fs::File::open("/proc/sys/kernel/random/boot_id")
        .ok()?
        .take(65)
        .read_to_end(&mut bytes)
        .ok()?;
    parse_boot_id(&bytes)
}

#[cfg(target_os = "macos")]
fn current_boot_id() -> Option<uuid::Uuid> {
    let mut bytes = [0_u8; 64];
    let mut length = bytes.len();
    // SAFETY: the name is NUL-terminated, output has the declared capacity, and
    // null newp/zero newlen make this a read-only sysctl query.
    if unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || length > bytes.len()
    {
        return None;
    }
    parse_boot_id(
        std::ffi::CStr::from_bytes_with_nul(&bytes[..length])
            .ok()?
            .to_bytes(),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn current_boot_id() -> Option<uuid::Uuid> {
    // Windows creation FILETIME already identifies an absolute native birth;
    // boot-relative Linux/macOS counters require the actual boot UUID above.
    None
}

#[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
fn cpu_units_to_us(
    value: u64,
    numerator: u64,
    denominator: u64,
) -> Result<u64, ProcessCpuUnavailable> {
    if numerator == 0 || denominator == 0 {
        return Err(ProcessCpuUnavailable::Unavailable);
    }
    u64::try_from(u128::from(value) * u128::from(numerator) / u128::from(denominator))
        .map_err(|_| ProcessCpuUnavailable::Unavailable)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unix_error(error: std::io::Error) -> ProcessCpuUnavailable {
    match error.raw_os_error() {
        Some(libc::EACCES | libc::EPERM) => ProcessCpuUnavailable::PermissionDenied,
        Some(libc::ENOENT | libc::ESRCH) => ProcessCpuUnavailable::NotRunning,
        Some(libc::ENOSYS | libc::ENOTSUP) => ProcessCpuUnavailable::Unsupported,
        _ => ProcessCpuUnavailable::Unavailable,
    }
}

#[cfg(target_os = "linux")]
fn observe(pid: u32) -> Result<ProcessCpuObservation, ProcessCpuUnavailable> {
    use std::io::Read;

    const MAX_STAT_BYTES: u64 = 16 * 1024;
    // SAFETY: sysconf takes no pointers and _SC_CLK_TCK is a supported query.
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let ticks_per_second =
        u64::try_from(ticks_per_second).map_err(|_| ProcessCpuUnavailable::Unavailable)?;
    let file = std::fs::File::open(format!("/proc/{pid}/stat")).map_err(|error| {
        // An absent procfs is missing accounting, not evidence of process exit.
        if error.kind() == std::io::ErrorKind::NotFound
            && !std::path::Path::new("/proc/self/stat").exists()
        {
            ProcessCpuUnavailable::Unavailable
        } else {
            unix_error(error)
        }
    })?;
    let mut stat = Vec::new();
    file.take(MAX_STAT_BYTES + 1)
        .read_to_end(&mut stat)
        .map_err(unix_error)?;
    if stat.len() as u64 > MAX_STAT_BYTES {
        return Err(ProcessCpuUnavailable::Unavailable);
    }
    parse_linux_stat(pid, &stat, ticks_per_second)
}

#[cfg(target_os = "linux")]
fn parse_linux_stat(
    pid: u32,
    stat: &[u8],
    ticks_per_second: u64,
) -> Result<ProcessCpuObservation, ProcessCpuUnavailable> {
    use ProcessCpuUnavailable::Unavailable;

    // comm may contain spaces, parentheses, newlines and non-UTF-8 bytes.
    // Every field following its final ')' is an ASCII scalar.
    let open = stat
        .iter()
        .position(|byte| *byte == b'(')
        .ok_or(Unavailable)?;
    let close = stat
        .iter()
        .rposition(|byte| *byte == b')')
        .ok_or(Unavailable)?;
    if close <= open || open < 2 || stat[open - 1] != b' ' {
        return Err(Unavailable);
    }
    let number = |value: &[u8]| -> Result<u64, ProcessCpuUnavailable> {
        if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
            return Err(Unavailable);
        }
        std::str::from_utf8(value)
            .ok()
            .and_then(|value| value.parse().ok())
            .ok_or(Unavailable)
    };
    if number(&stat[..open - 1])? != u64::from(pid) || stat.get(close + 1) != Some(&b' ') {
        return Err(Unavailable);
    }
    let mut fields = stat[close + 2..]
        .split(u8::is_ascii_whitespace)
        .filter(|field| !field.is_empty());
    match fields.next().ok_or(Unavailable)? {
        b"Z" | b"X" | b"x" => return Err(ProcessCpuUnavailable::NotRunning),
        b"R" | b"S" | b"D" | b"T" | b"t" | b"W" | b"K" | b"P" | b"I" => {}
        _ => return Err(Unavailable),
    }
    // Fields 14, 15 and 22; deliberately exclude child CPU fields 16 and 17.
    let user = number(fields.nth(10).ok_or(Unavailable)?)?;
    let system = number(fields.next().ok_or(Unavailable)?)?;
    let started = number(fields.nth(6).ok_or(Unavailable)?)?;
    Ok(ProcessCpuObservation {
        identity: ProcessCreationIdentity {
            pid,
            started,
            boot_id: None,
        },
        user_cpu_us: cpu_units_to_us(user, 1_000_000, ticks_per_second)?,
        system_cpu_us: cpu_units_to_us(system, 1_000_000, ticks_per_second)?,
    })
}

#[cfg(target_os = "macos")]
fn observe(pid: u32) -> Result<ProcessCpuObservation, ProcessCpuUnavailable> {
    let native_pid = libc::pid_t::try_from(pid).map_err(|_| ProcessCpuUnavailable::NotRunning)?;
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v0>::zeroed();
    // SAFETY: V0 writes exactly rusage_info_v0 bytes into this aligned buffer.
    // libproc's pointer-to-rusage_info_t ABI expects the struct buffer itself.
    if unsafe { libc::proc_pid_rusage(native_pid, libc::RUSAGE_INFO_V0, usage.as_mut_ptr().cast()) }
        != 0
    {
        return Err(unix_error(std::io::Error::last_os_error()));
    }
    // SAFETY: the successful call initialized the complete V0 buffer.
    let usage = unsafe { usage.assume_init() };
    if usage.ri_proc_exit_abstime != 0 {
        return Err(ProcessCpuUnavailable::NotRunning);
    }
    let mut timebase = libc::mach_timebase_info { numer: 0, denom: 0 };
    // SAFETY: timebase points to a writable mach_timebase_info.
    if unsafe { libc::mach_timebase_info(&mut timebase) } != libc::KERN_SUCCESS {
        return Err(ProcessCpuUnavailable::Unavailable);
    }
    // rusage CPU times are Mach absolute ticks, including on Apple Silicon.
    let denominator = u64::from(timebase.denom) * 1_000;
    Ok(ProcessCpuObservation {
        identity: ProcessCreationIdentity {
            pid,
            started: usage.ri_proc_start_abstime,
            boot_id: None,
        },
        user_cpu_us: cpu_units_to_us(usage.ri_user_time, u64::from(timebase.numer), denominator)?,
        system_cpu_us: cpu_units_to_us(
            usage.ri_system_time,
            u64::from(timebase.numer),
            denominator,
        )?,
    })
}

#[cfg(windows)]
fn windows_error(code: u32) -> ProcessCpuUnavailable {
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_CALL_NOT_IMPLEMENTED, ERROR_INVALID_PARAMETER,
        ERROR_NOT_SUPPORTED,
    };
    match code {
        ERROR_ACCESS_DENIED => ProcessCpuUnavailable::PermissionDenied,
        ERROR_INVALID_PARAMETER => ProcessCpuUnavailable::NotRunning,
        ERROR_CALL_NOT_IMPLEMENTED | ERROR_NOT_SUPPORTED => ProcessCpuUnavailable::Unsupported,
        _ => ProcessCpuUnavailable::Unavailable,
    }
}

#[cfg(windows)]
fn observe(pid: u32) -> Result<ProcessCpuObservation, ProcessCpuUnavailable> {
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GetLastError, FILETIME, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        System::Threading::{
            GetProcessTimes, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        },
    };

    fn ticks(time: FILETIME) -> u64 {
        (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
    }

    // SAFETY: OpenProcess accepts a numeric PID; no inherited handle is requested.
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return Err(windows_error(unsafe { GetLastError() }));
    }
    let mut created = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exited = created;
    let mut user = created;
    let mut system = created;
    // SAFETY: this live handle pins one process object even if its PID is reused.
    // All four output pointers refer to separate writable FILETIME values.
    let success =
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut system, &mut user) };
    let mut error = (success == 0).then(|| unsafe { GetLastError() });
    // GetProcessTimes leaves exit time undefined for a live process. A zero-time
    // wait observes termination without confusing an exit code with STILL_ACTIVE.
    // SAFETY: this handle has SYNCHRONIZE access; a zero timeout never blocks.
    let state = unsafe { WaitForSingleObject(handle, 0) };
    if state == WAIT_FAILED && error.is_none() {
        error = Some(unsafe { GetLastError() });
    }
    // SAFETY: the handle was opened above and is closed exactly once on every path.
    unsafe { CloseHandle(handle) };
    if let Some(error) = error {
        return Err(windows_error(error));
    }
    match state {
        WAIT_OBJECT_0 => return Err(ProcessCpuUnavailable::NotRunning),
        WAIT_TIMEOUT => {}
        _ => return Err(ProcessCpuUnavailable::Unavailable),
    }
    Ok(ProcessCpuObservation {
        identity: ProcessCreationIdentity {
            pid,
            started: ticks(created),
            boot_id: None,
        },
        user_cpu_us: cpu_units_to_us(ticks(user), 1, 10)?,
        system_cpu_us: cpu_units_to_us(ticks(system), 1, 10)?,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn observe(_pid: u32) -> Result<ProcessCpuObservation, ProcessCpuUnavailable> {
    Err(ProcessCpuUnavailable::Unsupported)
}

#[cfg(test)]
mod tests;
