use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use ctx_execution_runtime::{
    ExecutionSettings, HarnessSetupObserver, HarnessSetupPhase, RuntimeEventSink,
    SharedWarmupOperations,
};

#[derive(Clone)]
pub struct DefaultWarmupOperations {
    data_root: PathBuf,
    events: Arc<dyn RuntimeEventSink>,
}

impl DefaultWarmupOperations {
    pub fn new(data_root: PathBuf, events: Arc<dyn RuntimeEventSink>) -> Self {
        Self { data_root, events }
    }
}

#[async_trait]
impl SharedWarmupOperations for DefaultWarmupOperations {
    async fn warm_runtime(
        &self,
        settings: ExecutionSettings,
        observer: Arc<dyn HarnessSetupObserver>,
    ) -> Result<()> {
        ctx_harness_runtime::prewarm_selected_runtime_with_observer(
            &self.data_root,
            &settings.container,
            Some(observer.as_ref()),
        )
        .await
    }

    async fn warm_runtime_launch_ready(
        &self,
        settings: ExecutionSettings,
        observer: Arc<dyn HarnessSetupObserver>,
    ) -> Result<()> {
        ctx_harness_runtime::prewarm_selected_runtime_for_launch_with_observer(
            &self.data_root,
            &settings.container,
            Some(observer.as_ref()),
        )
        .await?;
        if let Some(record) =
            ctx_harness_runtime::selected_shared_substrate_lifecycle(&self.data_root)?
        {
            self.events
                .emit_substrate_lifecycle(&record, "runtime_prewarm_launch_ready", None);
        }
        Ok(())
    }

    async fn warm_builder(&self, observer: Arc<dyn HarnessSetupObserver>) -> Result<()> {
        observer.on_phase(HarnessSetupPhase::ImageLoad, "warming container builder");
        ctx_harness_runtime::container_builder::ensure_builder_ready(&self.data_root).await?;
        if let Some(record) =
            ctx_harness_runtime::selected_shared_substrate_lifecycle(&self.data_root)?
        {
            self.events
                .emit_substrate_lifecycle(&record, "builder_prewarm", None);
        }
        Ok(())
    }
}
