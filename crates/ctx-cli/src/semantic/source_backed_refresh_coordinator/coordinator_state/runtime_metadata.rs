use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) struct SourceRefreshRuntimeMetadata {
    pub(super) operation: SourceBackedRefreshOperation,
    pub(super) daemon_mode: DaemonMode,
    pub(super) trigger: &'static str,
    pub(super) trigger_provenance: &'static str,
}

impl Default for SourceRefreshRuntimeMetadata {
    fn default() -> Self {
        Self {
            operation: SourceBackedRefreshOperation::Refresh,
            daemon_mode: DaemonMode::Full,
            trigger: "search",
            trigger_provenance: "manual",
        }
    }
}

impl SourceRefreshRuntimeMetadata {
    pub(super) fn periodic() -> Self {
        Self {
            operation: SourceBackedRefreshOperation::Refresh,
            daemon_mode: DaemonMode::Full,
            trigger: "periodic",
            trigger_provenance: "daemon_scheduler",
        }
    }
}

pub(super) fn source_refresh_runtime_metadata(data_root: &Path) -> SourceRefreshRuntimeMetadata {
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
    let trigger_provenance = if daemon_status
        .as_ref()
        .and_then(|status| status.get("start_mode"))
        .and_then(Value::as_str)
        == Some("auto")
    {
        "autostart"
    } else {
        "manual"
    };
    SourceRefreshRuntimeMetadata {
        operation: SourceBackedRefreshOperation::Refresh,
        daemon_mode,
        trigger: "search",
        trigger_provenance,
    }
}

pub(super) fn source_catalog_refresh_runtime_metadata(
    data_root: &Path,
) -> SourceRefreshRuntimeMetadata {
    SourceRefreshRuntimeMetadata {
        operation: SourceBackedRefreshOperation::Import,
        trigger: "import",
        trigger_provenance: "explicit_source_catalog",
        ..source_refresh_runtime_metadata(data_root)
    }
}
