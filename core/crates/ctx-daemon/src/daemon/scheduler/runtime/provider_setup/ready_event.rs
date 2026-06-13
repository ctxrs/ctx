use std::collections::HashMap;

use ctx_core::ids::{RunId, TurnId};
use ctx_core::models::{ExecutionEnvironment, Session};

use crate::daemon::scheduler::host::ProviderTurnLaunchHost;

pub(super) struct ProviderSetupReadyEvent<'a> {
    pub(super) provider_launch: &'a ProviderTurnLaunchHost,
    pub(super) session: &'a Session,
    pub(super) run_id: RunId,
    pub(super) turn_id: TurnId,
    pub(super) workdir_str: &'a str,
    pub(super) full_model_id: &'a str,
    pub(super) execution_environment: ExecutionEnvironment,
    pub(super) session_root_kind: &'a str,
    pub(super) runtime_provider_id: &'a str,
    pub(super) using_endpoint_source: bool,
    pub(super) is_linux_sandbox: bool,
    pub(super) runtime_plan: &'a ctx_harness_runtime::HarnessExecutionPlan,
    pub(super) provider_env: &'a HashMap<String, String>,
}

pub(super) fn emit_provider_setup_ready_event(request: ProviderSetupReadyEvent<'_>) {
    request.provider_launch.emit_provider_run_env_ready_event(
        request.session,
        request.run_id,
        request.turn_id,
        request.workdir_str,
        request.full_model_id,
        request.execution_environment.as_str(),
        request.session_root_kind,
        request.runtime_provider_id,
        request.using_endpoint_source,
        request.is_linux_sandbox,
        request.runtime_plan,
        request.provider_env,
    );
}
