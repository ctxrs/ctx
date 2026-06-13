#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::time::Instant;

#[cfg(not(target_os = "linux"))]
use sysinfo::{Pid, ProcessRefreshKind, System};

#[cfg(target_os = "linux")]
use super::ProcCpuSample;
use super::{ProcMemoryRollup, ResourceProcess};

#[cfg(target_os = "linux")]
pub(super) fn proc_snapshot_from_proc(
    pid: u32,
    label: &str,
    now: Instant,
    proc_cpu: &mut HashMap<u32, ProcCpuSample>,
    clock_ticks: u64,
    seen: &mut HashSet<u32>,
) -> Option<ResourceProcess> {
    let rollup = read_proc_memory_rollup(pid)?;
    let cpu_pct = read_proc_cpu_pct(pid, now, proc_cpu, clock_ticks);
    let memory_bytes = rollup.rss_bytes.or(rollup.vm_hwm_bytes).unwrap_or(0);
    let virtual_memory_bytes = rollup.vm_size_bytes.or(rollup.vm_hwm_bytes).unwrap_or(0);
    seen.insert(pid);
    Some(ResourceProcess {
        label: label.to_string(),
        pid,
        cpu_pct,
        memory_bytes,
        virtual_memory_bytes,
        child_count: 0,
        children: Vec::new(),
        children_truncated: false,
    })
}

#[cfg(not(target_os = "linux"))]
pub(super) fn aggregate_process_sysinfo(
    system: &System,
    pid: u32,
    label: &str,
) -> Option<ResourceProcess> {
    let proc = system.process(Pid::from_u32(pid))?;
    Some(ResourceProcess {
        label: label.to_string(),
        pid,
        cpu_pct: proc.cpu_usage(),
        memory_bytes: proc.memory(),
        virtual_memory_bytes: proc.virtual_memory(),
        child_count: 0,
        children: Vec::new(),
        children_truncated: false,
    })
}

#[cfg(not(target_os = "linux"))]
pub(super) fn process_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::new().with_memory().with_cpu()
}

#[cfg(not(target_os = "linux"))]
pub(super) fn memory_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::new().with_memory()
}

#[cfg(target_os = "linux")]
pub(super) fn read_proc_memory_rollup(pid: u32) -> Option<ProcMemoryRollup> {
    let mut rollup = ProcMemoryRollup::default();
    let smaps_path = format!("/proc/{pid}/smaps_rollup");
    if let Ok(contents) = std::fs::read_to_string(smaps_path) {
        for line in contents.lines() {
            if let Some(value) = parse_kb_line(line, "Rss:") {
                rollup.rss_bytes = Some(value);
            } else if let Some(value) = parse_kb_line(line, "RssAnon:") {
                rollup.rss_anon_bytes = Some(value);
            } else if let Some(value) = parse_kb_line(line, "RssFile:") {
                rollup.rss_file_bytes = Some(value);
            } else if let Some(value) = parse_kb_line(line, "RssShmem:") {
                rollup.rss_shmem_bytes = Some(value);
            }
        }
    }

    let status_path = format!("/proc/{pid}/status");
    if let Ok(contents) = std::fs::read_to_string(status_path) {
        for line in contents.lines() {
            if rollup.rss_bytes.is_none() {
                if let Some(value) = parse_kb_line(line, "VmRSS:") {
                    rollup.rss_bytes = Some(value);
                }
            }
            if let Some(value) = parse_kb_line(line, "VmHWM:") {
                rollup.vm_hwm_bytes = Some(value);
            }
            if let Some(value) = parse_kb_line(line, "VmSize:") {
                rollup.vm_size_bytes = Some(value);
            }
        }
    }

    if rollup.is_empty() {
        None
    } else {
        Some(rollup)
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn read_proc_memory_rollup(_pid: u32) -> Option<ProcMemoryRollup> {
    None
}

#[cfg(target_os = "linux")]
fn parse_kb_line(line: &str, key: &str) -> Option<u64> {
    let mut parts = line.split_whitespace();
    if parts.next()? != key {
        return None;
    }
    let value = parts.next()?.parse::<u64>().ok()?;
    Some(value.saturating_mul(1024))
}

#[cfg(target_os = "linux")]
fn read_proc_cpu_pct(
    pid: u32,
    now: Instant,
    proc_cpu: &mut HashMap<u32, ProcCpuSample>,
    clock_ticks: u64,
) -> f32 {
    let total_ticks = match read_proc_cpu_ticks(pid) {
        Some(total) => total,
        None => return 0.0,
    };
    let prev = proc_cpu.insert(
        pid,
        ProcCpuSample {
            total_ticks,
            at: now,
        },
    );
    let Some(prev) = prev else {
        return 0.0;
    };
    let delta_ticks = total_ticks.saturating_sub(prev.total_ticks);
    let delta_secs = now.duration_since(prev.at).as_secs_f64();
    if delta_secs <= 0.0 || clock_ticks == 0 {
        return 0.0;
    }
    let cpu = (delta_ticks as f64 / clock_ticks as f64) / delta_secs * 100.0;
    cpu as f32
}

#[cfg(target_os = "linux")]
fn read_proc_cpu_ticks(pid: u32) -> Option<u64> {
    let stat_path = format!("/proc/{pid}/stat");
    let contents = std::fs::read_to_string(stat_path).ok()?;
    let end = contents.rfind(')')?;
    let rest = contents.get(end + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime.saturating_add(stime))
}

#[cfg(target_os = "linux")]
pub(super) fn clock_ticks_per_second() -> u64 {
    unsafe {
        let ticks = libc::sysconf(libc::_SC_CLK_TCK);
        if ticks > 0 {
            return ticks as u64;
        }
    }
    100
}
