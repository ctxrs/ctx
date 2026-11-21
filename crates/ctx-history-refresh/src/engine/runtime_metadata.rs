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

    /// Resolves optional user-configured indexing memory and worker limits.
    /// Callers without explicit limits inherit the built-in defaults.
    fn indexing_limits(&self, _data_root: &Path) -> Result<IndexingResourceLimits> {
        Ok(IndexingResourceLimits::default())
    }

    /// Notifies the process host after refresh execution has returned and its
    /// scoped workers have dropped, but before publication verification and
    /// terminal completion are recorded.
    fn refresh_execution_finished(&self) {}

    /// Returns true when the owning daemon is shutting down and an in-flight
    /// refresh should abort cooperatively so locks and temp candidates unwind.
    fn refresh_cancelled(&self) -> bool {
        false
    }
}

pub(super) type SourceRefreshRuntimeMetadata = RefreshRuntimeMetadata;

pub(super) fn canonical_daemon_mode(value: &str) -> Option<String> {
    match value.to_ascii_lowercase().as_str() {
        "full" => Some("full".to_owned()),
        "source-refresh-only" => Some("source-refresh-only".to_owned()),
        _ => None,
    }
}
