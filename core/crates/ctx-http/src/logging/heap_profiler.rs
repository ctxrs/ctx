use std::path::Path;

#[cfg(feature = "daemon-heap-prof")]
use std::time::Duration;

#[cfg(feature = "daemon-heap-prof")]
use anyhow::{anyhow, Result};
#[cfg(feature = "daemon-heap-prof")]
use chrono::Utc;
#[cfg(feature = "daemon-heap-prof")]
use tokio::time::MissedTickBehavior;

#[cfg(feature = "daemon-heap-prof")]
use super::{env_bool, env_string, env_u64};

#[cfg(feature = "daemon-heap-prof")]
const DEFAULT_DAEMON_HEAP_PROFILE_INTERVAL_SECS: u64 = 60;

#[cfg(feature = "daemon-heap-prof")]
pub(super) fn spawn_daemon_heap_profiler(logs_dir: &Path) {
    if !env_bool("CTX_DAEMON_HEAP_PROFILE").unwrap_or(false) {
        return;
    }
    let interval_secs = env_u64("CTX_DAEMON_HEAP_PROFILE_INTERVAL_SECS")
        .unwrap_or(DEFAULT_DAEMON_HEAP_PROFILE_INTERVAL_SECS)
        .max(1);
    let profile_dir = env_string("CTX_DAEMON_HEAP_PROFILE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| logs_dir.join("daemon-heap"));
    if let Err(err) = std::fs::create_dir_all(&profile_dir) {
        tracing::warn!("heap profile dir create failed: {err:?}");
        return;
    }
    match tikv_jemalloc_ctl::profiling::prof::read() {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!("jemalloc profiling disabled; set MALLOC_CONF=prof:true before start");
            return;
        }
        Err(err) => {
            tracing::warn!("jemalloc profiling unavailable: {err:?}");
            return;
        }
    }

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
            let path = profile_dir.join(format!("heap-{timestamp}.heap"));
            if let Err(err) = dump_heap_profile(&path) {
                tracing::warn!("heap profile dump failed: {err:#}");
            }
        }
    });
}

#[cfg(not(feature = "daemon-heap-prof"))]
pub(super) fn spawn_daemon_heap_profiler(_logs_dir: &Path) {}

#[cfg(feature = "daemon-heap-prof")]
fn dump_heap_profile(path: &Path) -> Result<()> {
    use std::ffi::CString;

    let path_str = path.to_string_lossy();
    let c_path = CString::new(path_str.as_bytes())
        .map_err(|err| anyhow!("heap profile path invalid: {err}"))?;
    unsafe {
        tikv_jemalloc_ctl::raw::write(b"prof.dump\0", c_path.as_ptr())
            .map_err(|err| anyhow!("jemalloc prof.dump failed: {err}"))?;
    }
    Ok(())
}
