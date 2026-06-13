use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use ctx_avf_linux_runtime::SubstrateLifecycleRecord;
use ctx_resource_utilization::{ProviderMemoryRollup, ResourceProcesses, SystemSnapshot};

#[derive(Debug, Serialize)]
pub(super) struct ResourceTelemetryEvent {
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) cache_age_ms: u64,
    pub(super) system: SystemSnapshot,
    pub(super) processes: ResourceProcesses,
    pub(super) provider_sessions: HashMap<String, u64>,
    pub(super) provider_memory_rollups: Vec<ProviderMemoryRollup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) shared_substrate_lifecycle: Option<SubstrateLifecycleRecord>,
}
