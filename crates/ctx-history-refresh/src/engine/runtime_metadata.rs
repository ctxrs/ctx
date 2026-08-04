use super::*;

/// Process-owned facts recorded with one refresh attempt.
#[derive(Debug, Clone)]
pub struct RefreshRuntimeMetadata {
    pub operation: RefreshOperation,
    pub daemon_mode: String,
    pub trigger: &'static str,
    pub trigger_provenance: &'static str,
}

impl Default for RefreshRuntimeMetadata {
    fn default() -> Self {
        Self {
            operation: RefreshOperation::Refresh,
            daemon_mode: "full".to_owned(),
            trigger: "search",
            trigger_provenance: "manual",
        }
    }
}

impl RefreshRuntimeMetadata {
    pub fn periodic() -> Self {
        Self {
            operation: RefreshOperation::Refresh,
            daemon_mode: "full".to_owned(),
            trigger: "periodic",
            trigger_provenance: "daemon_scheduler",
        }
    }
}

/// Process boundary for refresh metadata and provider discovery context.
pub trait RefreshRuntime: Send + Sync {
    fn metadata(&self, data_root: &Path, operation: RefreshOperation) -> RefreshRuntimeMetadata;

    fn discovery_context(&self, data_root: &Path) -> Result<DiscoveryContext>;
}

pub(super) type SourceRefreshRuntimeMetadata = RefreshRuntimeMetadata;

pub(super) fn canonical_daemon_mode(value: &str) -> Option<String> {
    match value.to_ascii_lowercase().as_str() {
        "full" => Some("full".to_owned()),
        "source-refresh-only" => Some("source-refresh-only".to_owned()),
        _ => None,
    }
}
