use ctx_core::ids::{RunId, TurnId};
use ctx_core::models::Session;

use crate::daemon::scheduler::host::{ProviderRunStartedOpsEvent, TurnRuntimeHost};

pub(super) struct ProviderRunStartedEvent<'a> {
    pub(super) host: &'a TurnRuntimeHost,
    pub(super) session: &'a Session,
    pub(super) run_id: RunId,
    pub(super) turn_id: TurnId,
    pub(super) workdir_str: &'a str,
    pub(super) full_model_id: &'a str,
    pub(super) execution_environment: &'a str,
    pub(super) session_root_kind: &'a str,
}

pub(super) fn emit_provider_run_started_event(event: ProviderRunStartedEvent<'_>) {
    let ProviderRunStartedEvent {
        host,
        session,
        run_id,
        turn_id,
        workdir_str,
        full_model_id,
        execution_environment,
        session_root_kind,
    } = event;
    host.emit_provider_run_started_event(ProviderRunStartedOpsEvent {
        session,
        run_id,
        turn_id,
        workdir_str,
        full_model_id,
        execution_environment,
        session_root_kind,
    });
}
