use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub(super) struct ProviderRefreshResourceReceipt {
    pub(super) cpu_duration: Duration,
    pub(super) observed_process_peak_rss_bytes: u64,
}

/// Captures process-authoritative resource counters around one provider import.
///
/// CPU time is a delta across the observation window. The RSS value is the
/// process-lifetime high-water mark observed when the window finishes; it is
/// not a per-provider-window peak. The resulting values stay typed and exact
/// only inside the process. Callers can expose them solely through the closed
/// bucketed analytics event.
#[derive(Debug)]
pub(crate) struct ProviderRefreshResourceObservation {
    started: Option<ProcessResourceSnapshot>,
}

impl ProviderRefreshResourceObservation {
    pub(crate) fn begin() -> Self {
        Self {
            started: process_resource_snapshot(),
        }
    }

    pub(super) fn finish(self) -> Option<ProviderRefreshResourceReceipt> {
        let started = self.started?;
        let finished = process_resource_snapshot()?;
        Some(ProviderRefreshResourceReceipt {
            cpu_duration: finished.cpu_duration.saturating_sub(started.cpu_duration),
            observed_process_peak_rss_bytes: finished.process_peak_rss_bytes,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct ProcessResourceSnapshot {
    cpu_duration: Duration,
    process_peak_rss_bytes: u64,
}

#[cfg(unix)]
fn process_resource_snapshot() -> Option<ProcessResourceSnapshot> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    let usage = unsafe { usage.assume_init() };
    let user = timeval_duration(usage.ru_utime)?;
    let system = timeval_duration(usage.ru_stime)?;
    let reported_rss = u64::try_from(usage.ru_maxrss).ok()?;
    #[cfg(target_os = "macos")]
    let process_peak_rss_bytes = reported_rss;
    #[cfg(not(target_os = "macos"))]
    let process_peak_rss_bytes = reported_rss.saturating_mul(1024);
    Some(ProcessResourceSnapshot {
        cpu_duration: user.saturating_add(system),
        process_peak_rss_bytes,
    })
}

#[cfg(unix)]
fn timeval_duration(value: libc::timeval) -> Option<Duration> {
    let seconds = u64::try_from(value.tv_sec).ok()?;
    let micros = u64::try_from(value.tv_usec).ok()?;
    Some(Duration::from_secs(seconds).saturating_add(Duration::from_micros(micros.min(999_999))))
}

#[cfg(windows)]
fn process_resource_snapshot() -> Option<ProcessResourceSnapshot> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::{
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetCurrentProcess, GetProcessTimes},
        },
    };

    let process = unsafe { GetCurrentProcess() };
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return None;
    }
    let mut memory = PROCESS_MEMORY_COUNTERS {
        cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?,
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    if unsafe { K32GetProcessMemoryInfo(process, &mut memory, memory.cb) } == 0 {
        return None;
    }
    Some(ProcessResourceSnapshot {
        cpu_duration: filetime_duration(kernel).saturating_add(filetime_duration(user)),
        process_peak_rss_bytes: u64::try_from(memory.PeakWorkingSetSize).unwrap_or(u64::MAX),
    })
}

#[cfg(windows)]
fn filetime_duration(value: windows_sys::Win32::Foundation::FILETIME) -> Duration {
    let ticks = (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime);
    Duration::from_nanos(ticks.saturating_mul(100))
}

#[cfg(not(any(unix, windows)))]
fn process_resource_snapshot() -> Option<ProcessResourceSnapshot> {
    None
}

#[cfg(test)]
mod tests {
    use super::ProviderRefreshResourceObservation;

    #[test]
    fn process_resource_observation_is_available_without_exact_values_leaving_the_seam() {
        let observation = ProviderRefreshResourceObservation::begin();
        let mut accumulator = 0_u64;
        for value in 0..10_000 {
            accumulator = accumulator.wrapping_add(value);
        }
        std::hint::black_box(accumulator);

        let receipt = observation
            .finish()
            .expect("supported release platforms expose process resource counters");
        assert!(receipt.observed_process_peak_rss_bytes > 0);
    }
}
