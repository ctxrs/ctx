use ctx_core::ids::WorkspaceId;
use ctx_execution_runtime::RuntimeEventSink;
use ctx_observability::ops_events::{
    substrate_lifecycle_observed_event, OpsEvent, OpsEvents, SubstrateLifecycleOpsEventContext,
};

pub struct CtxRuntimeEventSink {
    inner: OpsEvents,
}

impl CtxRuntimeEventSink {
    pub fn new(inner: OpsEvents) -> Self {
        Self { inner }
    }
}

impl RuntimeEventSink for CtxRuntimeEventSink {
    fn emit_event(&self, level: &'static str, name: &'static str, meta: Option<serde_json::Value>) {
        let mut event = OpsEvent::new(level, name);
        event.meta = meta;
        self.inner.emit(event);
    }

    fn emit_substrate_lifecycle(
        &self,
        record: &ctx_avf_linux_runtime::SubstrateLifecycleRecord,
        source: &'static str,
        workspace_id: Option<WorkspaceId>,
    ) {
        self.inner.emit(substrate_lifecycle_observed_event(
            record,
            SubstrateLifecycleOpsEventContext {
                source,
                workspace_id: workspace_id.map(|value| value.0.to_string()),
            },
        ));
    }
}
