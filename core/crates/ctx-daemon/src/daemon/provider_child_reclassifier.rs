use std::sync::Arc;

use tokio::sync::broadcast;

use super::DaemonState;

pub(super) fn spawn_provider_child_reclassifier(state: Arc<DaemonState>) {
    ctx_provider_runtime::provider_child_reclassifier::spawn_provider_child_reclassifier(state);
}

#[async_trait::async_trait]
impl ctx_provider_runtime::provider_child_reclassifier::ProviderChildReclassifierHost
    for DaemonState
{
    fn subscribe_shutdown(&self) -> broadcast::Receiver<()> {
        self.core.shutdown_tx.subscribe()
    }

    async fn provider_process_pids(&self) -> Vec<u32> {
        self.providers.provider_process_pids().await
    }

    fn tool_slice_unit(&self) -> &'static str {
        #[cfg(target_os = "linux")]
        {
            super::tool_cgroup::TOOL_SLICE_UNIT
        }
        #[cfg(not(target_os = "linux"))]
        {
            ""
        }
    }
}
