use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;

const DEFAULT_INTERVAL_MS: u64 = 5_000;
const MIN_INTERVAL_MS: u64 = 250;
const LOG_FILE_NAME: &str = "memleak-debug.jsonl";

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MemleakDebugConfig {
    pub enabled: bool,
    pub interval: Duration,
}

impl MemleakDebugConfig {
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        let enabled = env_bool_from_lookup(&mut lookup, "CTX_MEMLEAK_DEBUG").unwrap_or(false);
        let interval_ms = env_u64_from_lookup(&mut lookup, "CTX_MEMLEAK_DEBUG_INTERVAL_MS")
            .unwrap_or(DEFAULT_INTERVAL_MS)
            .max(MIN_INTERVAL_MS);

        Self {
            enabled,
            interval: Duration::from_millis(interval_ms),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MemleakDebugProcessStats {
    pub rss_bytes: u64,
    pub thread_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glibc: Option<GlibcMallinfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jemalloc: Option<JemallocStats>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JemallocStats {
    pub allocated: u64,
    pub active: u64,
    pub resident: u64,
    pub retained: u64,
    pub mapped: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GlibcMallinfo {
    pub arena: u64,
    pub ordblks: u64,
    pub hblks: u64,
    pub hblkhd: u64,
    pub uordblks: u64,
    pub fordblks: u64,
    pub keepcost: u64,
}

pub fn read_memleak_debug_process_stats() -> MemleakDebugProcessStats {
    MemleakDebugProcessStats {
        rss_bytes: read_rss_bytes(),
        thread_count: read_thread_count(),
        glibc: read_glibc_mallinfo(),
        jemalloc: read_jemalloc_stats(),
    }
}

pub fn memleak_debug_log_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join(LOG_FILE_NAME)
}

pub async fn append_memleak_debug_log<T: Serialize + ?Sized>(
    logs_dir: &Path,
    snapshot: &T,
) -> Result<()> {
    tokio::fs::create_dir_all(logs_dir).await?;
    let path = memleak_debug_log_path(logs_dir);
    let line = serde_json::to_string(snapshot)?;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    use tokio::io::AsyncWriteExt;
    file.write_all(line.as_bytes()).await?;
    file.write_all(b"\n").await?;
    file.flush().await?;
    Ok(())
}

pub fn json_bytes<T: Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

fn env_u64_from_lookup(lookup: &mut impl FnMut(&str) -> Option<String>, key: &str) -> Option<u64> {
    lookup(key)
        .as_deref()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn env_bool_from_lookup(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    key: &str,
) -> Option<bool> {
    lookup(key)
        .as_deref()
        .and_then(ctx_core::boolish::parse_boolish)
}

#[cfg(feature = "daemon-heap-prof")]
fn read_jemalloc_stats() -> Option<JemallocStats> {
    use tikv_jemalloc_ctl::stats;
    let allocated = stats::allocated::read().ok()? as u64;
    let active = stats::active::read().ok()? as u64;
    let resident = stats::resident::read().ok()? as u64;
    let retained = stats::retained::read().ok()? as u64;
    let mapped = stats::mapped::read().ok()? as u64;
    Some(JemallocStats {
        allocated,
        active,
        resident,
        retained,
        mapped,
    })
}

#[cfg(not(feature = "daemon-heap-prof"))]
fn read_jemalloc_stats() -> Option<JemallocStats> {
    None
}

#[cfg(target_os = "linux")]
fn read_rss_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            return kb.saturating_mul(1024);
        }
    }
    0
}

#[cfg(not(target_os = "linux"))]
fn read_rss_bytes() -> u64 {
    0
}

#[cfg(target_os = "linux")]
fn read_thread_count() -> u32 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Threads:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0);
        }
    }
    0
}

#[cfg(not(target_os = "linux"))]
fn read_thread_count() -> u32 {
    0
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn read_glibc_mallinfo() -> Option<GlibcMallinfo> {
    let symbol_name = b"mallinfo2\0";
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, symbol_name.as_ptr().cast()) };
    mallinfo_from_symbol(symbol)
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn read_glibc_mallinfo() -> Option<GlibcMallinfo> {
    None
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
type Mallinfo2Fn = unsafe extern "C" fn() -> libc::mallinfo2;

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn mallinfo_from_symbol(symbol: *mut libc::c_void) -> Option<GlibcMallinfo> {
    if symbol.is_null() {
        return None;
    }
    let mallinfo2 = unsafe { std::mem::transmute::<*mut libc::c_void, Mallinfo2Fn>(symbol) };
    let info = unsafe { mallinfo2() };
    Some(glibc_mallinfo_from_mallinfo2(info))
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn glibc_mallinfo_from_mallinfo2(info: libc::mallinfo2) -> GlibcMallinfo {
    GlibcMallinfo {
        arena: info.arena as u64,
        ordblks: info.ordblks as u64,
        hblks: info.hblks as u64,
        hblkhd: info.hblkhd as u64,
        uordblks: info.uordblks as u64,
        fordblks: info.fordblks as u64,
        keepcost: info.keepcost as u64,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde::Serialize;

    use super::*;

    #[derive(Serialize)]
    struct TestEvent {
        value: &'static str,
    }

    #[test]
    fn config_defaults_to_disabled_with_bounded_interval() {
        let config = MemleakDebugConfig::from_lookup(|_| None);

        assert!(!config.enabled);
        assert_eq!(config.interval, Duration::from_millis(DEFAULT_INTERVAL_MS));
    }

    #[test]
    fn config_parses_enablement_and_clamps_interval() {
        let config = MemleakDebugConfig::from_lookup(|key| match key {
            "CTX_MEMLEAK_DEBUG" => Some("true".to_string()),
            "CTX_MEMLEAK_DEBUG_INTERVAL_MS" => Some("1".to_string()),
            _ => None,
        });

        assert!(config.enabled);
        assert_eq!(config.interval, Duration::from_millis(MIN_INTERVAL_MS));
    }

    #[test]
    fn memleak_debug_log_path_uses_stable_file_name() {
        assert_eq!(
            memleak_debug_log_path(Path::new("/var/log/ctx")),
            PathBuf::from("/var/log/ctx/memleak-debug.jsonl")
        );
    }

    #[tokio::test]
    async fn append_memleak_debug_log_writes_jsonl() {
        let logs_dir = temp_logs_dir("append");
        append_memleak_debug_log(&logs_dir, &TestEvent { value: "sample" })
            .await
            .expect("append memleak debug sample");

        let written = tokio::fs::read_to_string(memleak_debug_log_path(&logs_dir))
            .await
            .expect("read memleak debug log");

        assert_eq!(written, "{\"value\":\"sample\"}\n");
        let _ = std::fs::remove_dir_all(&logs_dir);
    }

    #[test]
    fn json_bytes_returns_serialized_length() {
        assert_eq!(json_bytes(&TestEvent { value: "sample" }), 18);
    }

    fn temp_logs_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "ctx-memleak-debug-log-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    mod glibc_tests {
        use super::{glibc_mallinfo_from_mallinfo2, mallinfo_from_symbol};

        unsafe extern "C" fn fake_mallinfo2() -> libc::mallinfo2 {
            libc::mallinfo2 {
                arena: 11,
                ordblks: 12,
                smblks: 0,
                hblks: 13,
                hblkhd: 14,
                usmblks: 0,
                fsmblks: 0,
                uordblks: 15,
                fordblks: 16,
                keepcost: 17,
            }
        }

        #[test]
        fn glibc_mallinfo_maps_all_fields() {
            let mapped = glibc_mallinfo_from_mallinfo2(unsafe { fake_mallinfo2() });

            assert_eq!(mapped.arena, 11);
            assert_eq!(mapped.ordblks, 12);
            assert_eq!(mapped.hblks, 13);
            assert_eq!(mapped.hblkhd, 14);
            assert_eq!(mapped.uordblks, 15);
            assert_eq!(mapped.fordblks, 16);
            assert_eq!(mapped.keepcost, 17);
        }

        #[test]
        fn glibc_mallinfo_returns_none_when_symbol_is_missing() {
            assert!(mallinfo_from_symbol(std::ptr::null_mut()).is_none());
        }

        #[test]
        fn glibc_mallinfo_reads_symbol_when_present() {
            let symbol = fake_mallinfo2 as *const () as usize as *mut libc::c_void;
            let mapped = match mallinfo_from_symbol(symbol) {
                Some(mapped) => mapped,
                None => panic!("mallinfo2 symbol should resolve"),
            };

            assert_eq!(mapped.arena, 11);
            assert_eq!(mapped.ordblks, 12);
            assert_eq!(mapped.hblks, 13);
            assert_eq!(mapped.hblkhd, 14);
            assert_eq!(mapped.uordblks, 15);
            assert_eq!(mapped.fordblks, 16);
            assert_eq!(mapped.keepcost, 17);
        }
    }
}
