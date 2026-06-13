use std::collections::HashMap;
use std::sync::Arc;

use ctx_core::ids::{MessageId, RunId, TurnId};
use ctx_core::models::{ExecutionEnvironment, Session};
use ctx_providers::adapters::ProviderAdapter;

use crate::daemon::scheduler::host::{
    ProviderTurnLaunchHost, TurnRuntimeHost, WorkerLifecycleHost,
};

pub(in crate::daemon::scheduler::runtime) struct ProviderTurnRuntimeSetupRequest<'a> {
    pub(in crate::daemon::scheduler::runtime) turn_runtime: &'a TurnRuntimeHost,
    pub(in crate::daemon::scheduler::runtime) provider_launch: &'a ProviderTurnLaunchHost,
    pub(in crate::daemon::scheduler::runtime) lifecycle: &'a WorkerLifecycleHost,
    pub(in crate::daemon::scheduler::runtime) store: &'a ctx_store::Store,
    pub(in crate::daemon::scheduler::runtime) session: &'a Session,
    pub(in crate::daemon::scheduler::runtime) run_id: RunId,
    pub(in crate::daemon::scheduler::runtime) turn_id: TurnId,
    pub(in crate::daemon::scheduler::runtime) message_id: MessageId,
    pub(in crate::daemon::scheduler::runtime) workdir_str: &'a str,
    pub(in crate::daemon::scheduler::runtime) full_model_id: &'a str,
    pub(in crate::daemon::scheduler::runtime) execution_environment: ExecutionEnvironment,
    pub(in crate::daemon::scheduler::runtime) session_root_kind: &'a str,
}

pub(in crate::daemon::scheduler::runtime) struct ProviderTurnRuntimeSetup {
    pub(in crate::daemon::scheduler::runtime) provider_env: HashMap<String, String>,
    pub(in crate::daemon::scheduler::runtime) runtime_provider_id: String,
    pub(in crate::daemon::scheduler::runtime) adapter: Arc<dyn ProviderAdapter>,
}
