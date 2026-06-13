use chrono::{DateTime, Utc};
use ctx_harness_runtime::HarnessRuntimeStats;
use ctx_observability::perf_telemetry::PerfTelemetryStats;
use ctx_resource_utilization::memleak_debug::{GlibcMallinfo, JemallocStats};
use ctx_session_runtime::runtime::SessionRuntimeCacheDebugStats;
use ctx_store::StoreManagerStats;
use ctx_transport_runtime::terminals::TerminalManagerStats;
use ctx_transport_runtime::web_sessions::WebSessionManagerStats;
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotStats;
use serde::Serialize;

use crate::daemon::workspaces::WorkspaceCacheDebugStats;

#[derive(Debug, Serialize)]
pub(super) struct MemleakDebugSnapshot {
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) rss_bytes: u64,
    pub(super) thread_count: u32,
    pub(super) sessions: SessionCacheStats,
    pub(super) workspaces: WorkspaceCacheStats,
    pub(super) providers: ProviderCacheStats,
    pub(super) active_snapshot: WorkspaceActiveSnapshotStats,
    pub(super) terminals: TerminalManagerStats,
    pub(super) perf_telemetry: PerfTelemetryStats,
    pub(super) web_sessions: WebSessionManagerStats,
    pub(super) harness_runtime: HarnessRuntimeStats,
    pub(super) stores: StoreManagerStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) glibc: Option<GlibcMallinfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) jemalloc: Option<JemallocStats>,
}

pub(super) type SessionCacheStats = SessionRuntimeCacheDebugStats;

pub(super) type WorkspaceCacheStats = WorkspaceCacheDebugStats;

#[derive(Debug, Serialize)]
pub(super) struct ProviderCacheStats {
    pub(super) adapters: usize,
    pub(super) statuses: usize,
    pub(super) options_cache: usize,
    pub(super) verify_cache: usize,
    pub(super) usage_cache: usize,
    pub(super) installs: usize,
}
