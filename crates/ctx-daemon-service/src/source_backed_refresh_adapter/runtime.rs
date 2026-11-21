use std::{path::Path, sync::Arc};

use anyhow::{Context, Result};
use ctx_history_capture::DiscoveryContext;
use ctx_history_refresh::{
    IndexingResourceLimits, RefreshOperation, RefreshRuntime, RefreshRuntimeMetadata,
};
use serde_json::Value;

use crate::{
    daemon_wakeup::DaemonWakeup, paths_status::read_daemon_status, DaemonConfigPort, DaemonMode,
};

pub(crate) struct DaemonRefreshRuntime {
    config: &'static dyn DaemonConfigPort,
    wakeup: Option<Arc<DaemonWakeup>>,
}

impl DaemonRefreshRuntime {
    pub(crate) const fn new(config: &'static dyn DaemonConfigPort) -> Self {
        Self {
            config,
            wakeup: None,
        }
    }

    pub(crate) fn with_wakeup(
        config: &'static dyn DaemonConfigPort,
        wakeup: Arc<DaemonWakeup>,
    ) -> Self {
        Self {
            config,
            wakeup: Some(wakeup),
        }
    }
}

impl RefreshRuntime for DaemonRefreshRuntime {
    fn metadata(&self, data_root: &Path, operation: RefreshOperation) -> RefreshRuntimeMetadata {
        let daemon_status = read_daemon_status(data_root);
        let daemon_mode = self
            .config
            .load(data_root)
            .map(|config| config.daemon.mode)
            .ok()
            .or_else(|| {
                daemon_status
                    .as_ref()
                    .and_then(|status| status.get("config_reload"))
                    .and_then(|reload| reload.get("applied"))
                    .and_then(|applied| applied.get("daemon_mode"))
                    .and_then(Value::as_str)
                    .and_then(DaemonMode::parse)
            })
            .unwrap_or_default();
        let manual_provenance = if daemon_status
            .as_ref()
            .and_then(|status| status.get("start_mode"))
            .and_then(Value::as_str)
            == Some("auto")
        {
            "autostart"
        } else {
            "manual"
        };
        let (trigger, trigger_provenance) = match operation {
            RefreshOperation::Refresh => ("search", manual_provenance),
            RefreshOperation::Import => ("import", "explicit_source_catalog"),
        };
        RefreshRuntimeMetadata {
            operation,
            daemon_mode: daemon_mode.as_str().to_owned(),
            trigger,
            trigger_provenance,
        }
    }

    fn discovery_context(&self, _data_root: &Path) -> Result<DiscoveryContext> {
        self.config
            .discovery_context(_data_root)
            .context("resolve source-backed provider discovery context")
    }

    fn indexing_limits(&self, data_root: &Path) -> Result<IndexingResourceLimits> {
        let config = self
            .config
            .load(data_root)
            .context("load configured indexing memory and worker limits")?;
        Ok(IndexingResourceLimits::from_memory_mb_and_workers(
            config.indexing_memory_mb,
            config.indexing_workers,
        ))
    }

    fn refresh_execution_finished(&self) {
        release_unused_allocator_pages();
    }

    fn refresh_cancelled(&self) -> bool {
        self.wakeup
            .as_ref()
            .is_some_and(|wakeup| wakeup.shutdown_requested())
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn release_unused_allocator_pages() {
    // SAFETY: malloc_trim has no caller-owned pointer or buffer contract and
    // glibc serializes allocator maintenance across process threads.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn release_unused_allocator_pages() {}
