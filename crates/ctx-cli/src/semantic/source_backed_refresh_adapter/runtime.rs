use std::path::Path;

use anyhow::{Context, Result};
use ctx_history_capture::DiscoveryContext;
use ctx_history_refresh::{RefreshOperation, RefreshRuntime, RefreshRuntimeMetadata};
use serde_json::Value;

use crate::{
    config::{AppConfig, DaemonMode},
    identity,
    semantic::paths_status::read_daemon_status,
};

#[derive(Debug, Default)]
pub(in crate::semantic) struct DaemonRefreshRuntime;

impl RefreshRuntime for DaemonRefreshRuntime {
    fn metadata(&self, data_root: &Path, operation: RefreshOperation) -> RefreshRuntimeMetadata {
        let daemon_status = read_daemon_status(data_root);
        let daemon_mode = AppConfig::load(data_root)
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
        let home = identity::home_dir()
            .context("resolve the user home for source-backed provider discovery")?;
        Ok(DiscoveryContext::from_process(home))
    }
}
