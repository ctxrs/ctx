use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
use std::io::Read;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::{analytics::AnalyticsProperties, identity};

const SNAPSHOT_SCHEMA: u64 = 1;
const CLAIM_FILE: &str = "execution-capabilities-v1.claim";
const REPORTED_FILE: &str = "execution-capabilities-v1.reported";
#[cfg(target_os = "linux")]
const MAX_NATIVE_FILE_BYTES: usize = 64 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;

pub(crate) struct PendingSnapshot {
    claim_path: PathBuf,
    reported_path: PathBuf,
    snapshot: Snapshot,
}

impl PendingSnapshot {
    pub(crate) fn insert_properties(&self, properties: &mut AnalyticsProperties) {
        properties.insert(
            "capability_snapshot_schema".to_owned(),
            Value::Number(SNAPSHOT_SCHEMA.into()),
        );
        properties.insert(
            "available_parallelism_bucket".to_owned(),
            Value::String(self.snapshot.available_parallelism_bucket.to_owned()),
        );
        properties.insert(
            "host_memory_bucket".to_owned(),
            Value::String(self.snapshot.host_memory_bucket.to_owned()),
        );
        properties.insert(
            "cpu_vector_tier".to_owned(),
            Value::String(self.snapshot.cpu_vector_tier.to_owned()),
        );
        properties.insert(
            "acceleration_candidate".to_owned(),
            Value::String(self.snapshot.acceleration_candidate.to_owned()),
        );
    }

    pub(crate) fn mark_reported(self) -> Result<()> {
        match fs::rename(&self.claim_path, &self.reported_path) {
            Ok(()) => Ok(()),
            Err(_) if path_entry_exists(&self.reported_path)? => {
                let _ = fs::remove_file(&self.claim_path);
                Ok(())
            }
            Err(err) => Err(err).with_context(|| {
                format!(
                    "promote {} to {}",
                    self.claim_path.display(),
                    self.reported_path.display()
                )
            }),
        }
    }
}

pub(crate) fn pending(data_root: &Path) -> Result<Option<PendingSnapshot>> {
    let claim_path = identity::device_state_path(CLAIM_FILE, data_root)?;
    let reported_path = identity::device_state_path(REPORTED_FILE, data_root)?;
    if path_entry_exists(&reported_path)? || path_entry_exists(&claim_path)? {
        return Ok(None);
    }
    if let Some(parent) = claim_path.parent() {
        fs::create_dir_all(parent)?;
    }
    match identity::create_private_file(&claim_path, b"schema_version=1\n") {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("claim {}", claim_path.display()));
        }
    }
    Ok(Some(PendingSnapshot {
        claim_path,
        reported_path,
        snapshot: Snapshot::collect(),
    }))
}

fn path_entry_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    available_parallelism_bucket: &'static str,
    host_memory_bucket: &'static str,
    cpu_vector_tier: &'static str,
    acceleration_candidate: &'static str,
}

impl Snapshot {
    fn collect() -> Self {
        Self {
            available_parallelism_bucket: std::thread::available_parallelism()
                .ok()
                .map(|value| parallelism_bucket(value.get()))
                .unwrap_or("unknown"),
            host_memory_bucket: host_memory_bytes().map(memory_bucket).unwrap_or("unknown"),
            cpu_vector_tier: cpu_vector_tier(),
            acceleration_candidate: acceleration_candidate(),
        }
    }
}

fn parallelism_bucket(parallelism: usize) -> &'static str {
    match parallelism {
        0 => "unknown",
        1 => "1",
        2 => "2",
        3..=4 => "3-4",
        5..=8 => "5-8",
        9..=16 => "9-16",
        17..=32 => "17-32",
        33..=64 => "33-64",
        _ => "65+",
    }
}

fn memory_bucket(bytes: u64) -> &'static str {
    if bytes == 0 {
        "unknown"
    } else if bytes < 4 * GIB {
        "lt_4gb"
    } else if bytes < 8 * GIB {
        "4-8gb"
    } else if bytes < 16 * GIB {
        "8-16gb"
    } else if bytes < 32 * GIB {
        "16-32gb"
    } else if bytes < 64 * GIB {
        "32-64gb"
    } else {
        "64gb+"
    }
}

#[cfg(target_os = "linux")]
fn host_memory_bytes() -> Option<u64> {
    let body = read_bounded(Path::new("/proc/meminfo"), MAX_NATIVE_FILE_BYTES).ok()?;
    parse_meminfo_total_bytes(std::str::from_utf8(&body).ok()?)
}

#[cfg(any(target_os = "linux", test))]
fn parse_meminfo_total_bytes(text: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?;
        let mut fields = rest.split_whitespace();
        let kib = fields.next()?.parse::<u64>().ok()?;
        if fields.next()? != "kB" || fields.next().is_some() {
            return None;
        }
        kib.checked_mul(1024)
    })
}

#[cfg(target_os = "macos")]
fn host_memory_bytes() -> Option<u64> {
    sysctl_u64(b"hw.memsize\0")
}

#[cfg(target_os = "freebsd")]
fn host_memory_bytes() -> Option<u64> {
    sysctl_u64(b"hw.physmem\0")
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn sysctl_u64(name: &'static [u8]) -> Option<u64> {
    let mut bytes = 0_u64;
    let mut size = std::mem::size_of::<u64>();
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            (&mut bytes as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && size == std::mem::size_of::<u64>()).then_some(bytes)
}

#[cfg(target_os = "windows")]
fn host_memory_bytes() -> Option<u64> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.total_phys)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "freebsd"
)))]
fn host_memory_bytes() -> Option<u64> {
    None
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn cpu_vector_tier() -> &'static str {
    if std::arch::is_x86_feature_detected!("avx512f") {
        "avx512"
    } else if std::arch::is_x86_feature_detected!("avx2") {
        "avx2"
    } else {
        "x86_baseline"
    }
}

#[cfg(target_arch = "aarch64")]
fn cpu_vector_tier() -> &'static str {
    if std::arch::is_aarch64_feature_detected!("neon") {
        "arm_neon"
    } else {
        "other"
    }
}

#[cfg(target_arch = "arm")]
fn cpu_vector_tier() -> &'static str {
    if std::arch::is_arm_feature_detected!("neon") {
        "arm_neon"
    } else {
        "other"
    }
}

#[cfg(not(any(
    target_arch = "x86",
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "arm"
)))]
fn cpu_vector_tier() -> &'static str {
    "other"
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn acceleration_candidate() -> &'static str {
    "apple_ane"
}

#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
fn acceleration_candidate() -> &'static str {
    "not_detected"
}

#[cfg(target_os = "linux")]
fn acceleration_candidate() -> &'static str {
    match linux_nvidia_driver_has_device() {
        Ok(true) => "nvidia_cuda",
        Ok(false) => match linux_drm_has_nvidia_device() {
            Ok(true) | Err(_) => "unknown",
            Ok(false) => "not_detected",
        },
        Err(_) => "unknown",
    }
}

#[cfg(target_os = "linux")]
fn linux_nvidia_driver_has_device() -> io::Result<bool> {
    match fs::read_dir("/proc/driver/nvidia/gpus") {
        Ok(entries) => Ok(entries.take(32).filter_map(Result::ok).next().is_some()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(target_os = "linux")]
fn linux_drm_has_nvidia_device() -> io::Result<bool> {
    let entries = fs::read_dir("/sys/class/drm")?;
    for entry in entries.take(128) {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with("card") {
            continue;
        }
        let vendor = entry.path().join("device/vendor");
        let value = match read_bounded(&vendor, 32) {
            Ok(value) => value,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        if std::str::from_utf8(&value)
            .ok()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("0x10de"))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "windows")]
fn acceleration_candidate() -> &'static str {
    match windows_system_directory() {
        Some(path) => match fs::symlink_metadata(path.join("nvcuda.dll")) {
            Ok(metadata) if metadata.is_file() => "nvidia_cuda",
            Ok(_) => "unknown",
            Err(err) if err.kind() == io::ErrorKind::NotFound => "not_detected",
            Err(_) => "unknown",
        },
        None => "unknown",
    }
}

#[cfg(target_os = "windows")]
fn windows_system_directory() -> Option<PathBuf> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};

    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }

    let mut buffer = vec![0_u16; 32_768];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&buffer[..length])))
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn acceleration_candidate() -> &'static str {
    "unknown"
}

#[cfg(target_os = "linux")]
fn read_bounded(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut body = Vec::new();
    file.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut body)?;
    if body.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "native capability input exceeds size limit",
        ));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallelism_buckets_are_coarse_and_exhaustive() {
        assert_eq!(parallelism_bucket(0), "unknown");
        assert_eq!(parallelism_bucket(1), "1");
        assert_eq!(parallelism_bucket(2), "2");
        assert_eq!(parallelism_bucket(4), "3-4");
        assert_eq!(parallelism_bucket(8), "5-8");
        assert_eq!(parallelism_bucket(16), "9-16");
        assert_eq!(parallelism_bucket(32), "17-32");
        assert_eq!(parallelism_bucket(64), "33-64");
        assert_eq!(parallelism_bucket(65), "65+");
    }

    #[test]
    fn memory_buckets_do_not_expose_byte_counts() {
        assert_eq!(memory_bucket(0), "unknown");
        assert_eq!(memory_bucket(4 * GIB - 1), "lt_4gb");
        assert_eq!(memory_bucket(4 * GIB), "4-8gb");
        assert_eq!(memory_bucket(8 * GIB), "8-16gb");
        assert_eq!(memory_bucket(16 * GIB), "16-32gb");
        assert_eq!(memory_bucket(32 * GIB), "32-64gb");
        assert_eq!(memory_bucket(64 * GIB), "64gb+");
    }

    #[test]
    fn parses_linux_memory_strictly() {
        assert_eq!(
            parse_meminfo_total_bytes("MemFree: 10 kB\nMemTotal: 16384 kB\n"),
            Some(16 * 1024 * 1024)
        );
        assert_eq!(parse_meminfo_total_bytes("MemTotal: 16 MB\n"), None);
    }

    #[test]
    fn cpu_vector_tier_is_an_allowlisted_scalar() {
        assert!(matches!(
            cpu_vector_tier(),
            "avx512" | "avx2" | "x86_baseline" | "arm_neon" | "other"
        ));
    }
}
