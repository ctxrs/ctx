use super::super::*;
use std::path::Path;

mod defaults;

pub(super) use defaults::{
    build_execution_runtime, build_provider_runtime, build_telemetry_runtime,
    build_transport_runtime, build_workspace_runtime,
};

pub(super) struct ToolOutputSpool {
    pub(super) enabled: bool,
    pub(super) dir: PathBuf,
}

pub(super) struct BuilderRuntimeParts {
    pub(super) shutdown_tx: broadcast::Sender<()>,
    pub(super) ask_user_question: Arc<AskUserQuestionBroker>,
    pub(super) telemetry: Telemetry,
    pub(super) provider_unknown_events:
        ctx_observability::provider_unknown_events::ProviderUnknownEvents,
    pub(super) ops_events: OpsEvents,
    pub(super) perf_telemetry: PerfTelemetry,
    pub(super) harness_runtime: Arc<HarnessRuntimeManager>,
    pub(super) execution_setup: Arc<ExecutionSetupCoordinator>,
    pub(super) terminals: Arc<TerminalManager>,
    pub(super) workspace_active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(super) web_sessions: Arc<WebSessionManager>,
}

pub(super) fn build_tool_output_spool(data_root: &Path) -> ToolOutputSpool {
    // Internal spool-path mechanics remain experimental, but once output is
    // promoted into the session artifact list it follows the normal
    // SessionState/artifact client contract.
    let enabled = std::env::var("CTX_TOOL_OUTPUT_DISK_SPOOL")
        .ok()
        .as_deref()
        .and_then(ctx_core::boolish::parse_boolish)
        .unwrap_or(false);
    let dir = data_root.join("tool-output-spool");
    if enabled {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(
                "failed to create tool output spool dir {}: {e}",
                dir.to_string_lossy()
            );
        }
    }
    ToolOutputSpool { enabled, dir }
}

pub(super) fn build_runtime_parts(data_root: &Path) -> BuilderRuntimeParts {
    let (shutdown_tx, _) = broadcast::channel(8);
    let ask_user_question = Arc::new(AskUserQuestionBroker::new());
    let data_root = data_root.to_path_buf();
    let telemetry = Telemetry::new(data_root.clone());
    let provider_unknown_events =
        ctx_observability::provider_unknown_events::ProviderUnknownEvents::new(
            data_root.clone(),
            telemetry.clone(),
        );
    let ops_events = OpsEvents::new(data_root.clone());
    let perf_telemetry = PerfTelemetry::new(data_root.clone());
    let runtime_events = Arc::new(CtxRuntimeEventSink::new(ops_events.clone()));
    let harness_runtime = Arc::new(HarnessRuntimeManager::new_with_event_sink(
        data_root.clone(),
        runtime_events.clone(),
    ));
    let runtime_metrics = Arc::new(CtxRuntimeMetricsSink::new(perf_telemetry.clone()));
    let execution_harness = Arc::new(CtxExecutionHarness::new(harness_runtime.clone()));
    let warmup_operations = Arc::new(DefaultWarmupOperations::new(
        data_root.clone(),
        runtime_events.clone(),
    ));
    let execution_setup = Arc::new(ExecutionSetupCoordinator::new_with_operations(
        data_root.clone(),
        execution_harness,
        runtime_events,
        runtime_metrics,
        warmup_operations,
    ));
    BuilderRuntimeParts {
        shutdown_tx,
        ask_user_question,
        telemetry,
        provider_unknown_events,
        ops_events,
        perf_telemetry,
        harness_runtime,
        execution_setup,
        terminals: Arc::new(TerminalManager::default()),
        workspace_active_snapshot: Arc::new(WorkspaceActiveSnapshotHub::new()),
        web_sessions: Arc::new(WebSessionManager::new()),
    }
}
