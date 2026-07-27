use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
use std::io::Read;

use crate::identity;
use anyhow::{Context, Result};

const CLAIM_FILE: &str = "execution-capabilities-v1.claim";
const REPORTED_FILE: &str = "execution-capabilities-v1.reported";
#[cfg(target_os = "linux")]
const MAX_NATIVE_FILE_BYTES: usize = 64 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;

pub(crate) struct PendingSnapshot {
    claim_path: PathBuf,
    reported_path: PathBuf,
    snapshot: CapabilitySnapshotV1,
}

impl PendingSnapshot {
    pub(crate) fn snapshot(&self) -> &CapabilitySnapshotV1 {
        &self.snapshot
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
    if discard_claim_if_reported(&claim_path, &reported_path)? {
        return Ok(None);
    }
    Ok(Some(PendingSnapshot {
        claim_path,
        reported_path,
        snapshot: CapabilitySnapshotV1::collect(),
    }))
}

fn discard_claim_if_reported(claim_path: &Path, reported_path: &Path) -> Result<bool> {
    if !path_entry_exists(reported_path)? {
        return Ok(false);
    }
    fs::remove_file(claim_path)
        .with_context(|| format!("discard superseded claim {}", claim_path.display()))?;
    Ok(true)
}

fn path_entry_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParallelismBucketV1 {
    Unknown,
    One,
    Two,
    ThreeToFour,
    FiveToEight,
    NineToSixteen,
    SeventeenToThirtyTwo,
    ThirtyThreeToSixtyFour,
    OverSixtyFour,
}

impl ParallelismBucketV1 {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::One => "1",
            Self::Two => "2",
            Self::ThreeToFour => "3-4",
            Self::FiveToEight => "5-8",
            Self::NineToSixteen => "9-16",
            Self::SeventeenToThirtyTwo => "17-32",
            Self::ThirtyThreeToSixtyFour => "33-64",
            Self::OverSixtyFour => "65+",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryBucketV1 {
    Unknown,
    UnderFourGb,
    FourToEightGb,
    EightToSixteenGb,
    SixteenToThirtyTwoGb,
    ThirtyTwoToSixtyFourGb,
    OverSixtyFourGb,
}

impl MemoryBucketV1 {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::UnderFourGb => "lt_4gb",
            Self::FourToEightGb => "4-8gb",
            Self::EightToSixteenGb => "8-16gb",
            Self::SixteenToThirtyTwoGb => "16-32gb",
            Self::ThirtyTwoToSixtyFourGb => "32-64gb",
            Self::OverSixtyFourGb => "64gb+",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CpuVectorTierV1 {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    Avx512,
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    Avx2,
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    X86Baseline,
    #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
    ArmNeon,
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    Other,
}

impl CpuVectorTierV1 {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Self::Avx512 => "avx512",
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Self::Avx2 => "avx2",
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Self::X86Baseline => "x86_baseline",
            #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
            Self::ArmNeon => "arm_neon",
            #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccelerationCandidateV1 {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    AppleAne,
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    NvidiaCuda,
    #[cfg(any(
        target_os = "linux",
        target_os = "windows",
        all(target_os = "macos", not(target_arch = "aarch64"))
    ))]
    NotDetected,
    #[cfg(not(target_os = "macos"))]
    Unknown,
}

impl AccelerationCandidateV1 {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            Self::AppleAne => "apple_ane",
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            Self::NvidiaCuda => "nvidia_cuda",
            #[cfg(any(
                target_os = "linux",
                target_os = "windows",
                all(target_os = "macos", not(target_arch = "aarch64"))
            ))]
            Self::NotDetected => "not_detected",
            #[cfg(not(target_os = "macos"))]
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CapabilitySnapshotV1 {
    pub(crate) available_parallelism: ParallelismBucketV1,
    pub(crate) host_memory: MemoryBucketV1,
    pub(crate) cpu_vector: CpuVectorTierV1,
    pub(crate) acceleration: AccelerationCandidateV1,
}

impl CapabilitySnapshotV1 {
    fn collect() -> Self {
        Self {
            available_parallelism: std::thread::available_parallelism()
                .ok()
                .map(|value| parallelism_bucket(value.get()))
                .unwrap_or(ParallelismBucketV1::Unknown),
            host_memory: host_memory_bytes()
                .map(memory_bucket)
                .unwrap_or(MemoryBucketV1::Unknown),
            cpu_vector: cpu_vector_tier(),
            acceleration: acceleration_candidate(),
        }
    }
}

fn parallelism_bucket(parallelism: usize) -> ParallelismBucketV1 {
    match parallelism {
        0 => ParallelismBucketV1::Unknown,
        1 => ParallelismBucketV1::One,
        2 => ParallelismBucketV1::Two,
        3..=4 => ParallelismBucketV1::ThreeToFour,
        5..=8 => ParallelismBucketV1::FiveToEight,
        9..=16 => ParallelismBucketV1::NineToSixteen,
        17..=32 => ParallelismBucketV1::SeventeenToThirtyTwo,
        33..=64 => ParallelismBucketV1::ThirtyThreeToSixtyFour,
        _ => ParallelismBucketV1::OverSixtyFour,
    }
}

fn memory_bucket(bytes: u64) -> MemoryBucketV1 {
    if bytes == 0 {
        MemoryBucketV1::Unknown
    } else if bytes < 4 * GIB {
        MemoryBucketV1::UnderFourGb
    } else if bytes < 8 * GIB {
        MemoryBucketV1::FourToEightGb
    } else if bytes < 16 * GIB {
        MemoryBucketV1::EightToSixteenGb
    } else if bytes < 32 * GIB {
        MemoryBucketV1::SixteenToThirtyTwoGb
    } else if bytes < 64 * GIB {
        MemoryBucketV1::ThirtyTwoToSixtyFourGb
    } else {
        MemoryBucketV1::OverSixtyFourGb
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
fn cpu_vector_tier() -> CpuVectorTierV1 {
    if std::arch::is_x86_feature_detected!("avx512f") {
        CpuVectorTierV1::Avx512
    } else if std::arch::is_x86_feature_detected!("avx2") {
        CpuVectorTierV1::Avx2
    } else {
        CpuVectorTierV1::X86Baseline
    }
}

#[cfg(target_arch = "aarch64")]
fn cpu_vector_tier() -> CpuVectorTierV1 {
    if std::arch::is_aarch64_feature_detected!("neon") {
        CpuVectorTierV1::ArmNeon
    } else {
        CpuVectorTierV1::Other
    }
}

#[cfg(target_arch = "arm")]
fn cpu_vector_tier() -> CpuVectorTierV1 {
    if std::arch::is_arm_feature_detected!("neon") {
        CpuVectorTierV1::ArmNeon
    } else {
        CpuVectorTierV1::Other
    }
}

#[cfg(not(any(
    target_arch = "x86",
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "arm"
)))]
fn cpu_vector_tier() -> CpuVectorTierV1 {
    CpuVectorTierV1::Other
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn acceleration_candidate() -> AccelerationCandidateV1 {
    AccelerationCandidateV1::AppleAne
}

#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
fn acceleration_candidate() -> AccelerationCandidateV1 {
    AccelerationCandidateV1::NotDetected
}

#[cfg(target_os = "linux")]
fn acceleration_candidate() -> AccelerationCandidateV1 {
    match linux_nvidia_driver_has_device() {
        Ok(true) => AccelerationCandidateV1::NvidiaCuda,
        Ok(false) => match linux_drm_has_nvidia_device() {
            Ok(true) | Err(_) => AccelerationCandidateV1::Unknown,
            Ok(false) => AccelerationCandidateV1::NotDetected,
        },
        Err(_) => AccelerationCandidateV1::Unknown,
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
fn acceleration_candidate() -> AccelerationCandidateV1 {
    match windows_system_directory() {
        Some(path) => match fs::symlink_metadata(path.join("nvcuda.dll")) {
            Ok(metadata) if metadata.is_file() => AccelerationCandidateV1::NvidiaCuda,
            Ok(_) => AccelerationCandidateV1::Unknown,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                AccelerationCandidateV1::NotDetected
            }
            Err(_) => AccelerationCandidateV1::Unknown,
        },
        None => AccelerationCandidateV1::Unknown,
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
fn acceleration_candidate() -> AccelerationCandidateV1 {
    AccelerationCandidateV1::Unknown
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
        assert_eq!(parallelism_bucket(0).as_str(), "unknown");
        assert_eq!(parallelism_bucket(1).as_str(), "1");
        assert_eq!(parallelism_bucket(2).as_str(), "2");
        assert_eq!(parallelism_bucket(4).as_str(), "3-4");
        assert_eq!(parallelism_bucket(8).as_str(), "5-8");
        assert_eq!(parallelism_bucket(16).as_str(), "9-16");
        assert_eq!(parallelism_bucket(32).as_str(), "17-32");
        assert_eq!(parallelism_bucket(64).as_str(), "33-64");
        assert_eq!(parallelism_bucket(65).as_str(), "65+");
    }

    #[test]
    fn memory_buckets_do_not_expose_byte_counts() {
        assert_eq!(memory_bucket(0).as_str(), "unknown");
        assert_eq!(memory_bucket(4 * GIB - 1).as_str(), "lt_4gb");
        assert_eq!(memory_bucket(4 * GIB).as_str(), "4-8gb");
        assert_eq!(memory_bucket(8 * GIB).as_str(), "8-16gb");
        assert_eq!(memory_bucket(16 * GIB).as_str(), "16-32gb");
        assert_eq!(memory_bucket(32 * GIB).as_str(), "32-64gb");
        assert_eq!(memory_bucket(64 * GIB).as_str(), "64gb+");
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
    fn newly_created_claim_is_discarded_after_reported_marker_wins_handoff() {
        let temp = tempfile::tempdir().unwrap();
        let claim_path = temp.path().join(CLAIM_FILE);
        let reported_path = temp.path().join(REPORTED_FILE);

        assert!(!path_entry_exists(&reported_path).unwrap());
        fs::write(&claim_path, b"schema_version=1\n").unwrap();
        fs::write(&reported_path, b"schema_version=1\n").unwrap();

        assert!(discard_claim_if_reported(&claim_path, &reported_path).unwrap());
        assert!(!path_entry_exists(&claim_path).unwrap());
        assert!(path_entry_exists(&reported_path).unwrap());
    }

    #[test]
    fn cpu_vector_tier_is_an_allowlisted_scalar() {
        assert!(matches!(
            cpu_vector_tier().as_str(),
            "avx512" | "avx2" | "x86_baseline" | "arm_neon" | "other"
        ));
    }
}
