use std::collections::HashMap;

use anyhow::Result;
use ctx_core::ids::{MessageId, RunId, TurnId};
use ctx_core::models::{ExecutionEnvironment, Session};
use ctx_harness_sources::{HarnessRuntimeSourceMode, ResolvedHarnessSource};
use ctx_settings_model::ExecutionSettings;

use crate::daemon::scheduler::host::{ProviderTurnLaunchHost, WorkerLifecycleHost};

use super::super::execution_plan::{prepare_turn_execution_plan, TurnExecutionPlan};
use super::super::provider_env::apply_runtime_source_env;
use super::super::turn_failure::emit_turn_start_failed;

pub(super) struct ProviderExecutionContextRequest<'a> {
    pub(super) provider_launch: &'a ProviderTurnLaunchHost,
    pub(super) lifecycle: &'a WorkerLifecycleHost,
    pub(super) store: &'a ctx_store::Store,
    pub(super) session: &'a Session,
    pub(super) run_id: RunId,
    pub(super) turn_id: TurnId,
    pub(super) message_id: MessageId,
    pub(super) execution_environment: ExecutionEnvironment,
}

pub(super) struct ProviderExecutionContext {
    pub(super) execution_settings: ExecutionSettings,
    pub(super) runtime_plan: ctx_harness_runtime::HarnessExecutionPlan,
    pub(super) resolved_source: ResolvedHarnessSource,
    pub(super) runtime_source_mode: HarnessRuntimeSourceMode,
    pub(super) using_endpoint_source: bool,
}

pub(super) async fn prepare_provider_execution_context(
    request: ProviderExecutionContextRequest<'_>,
    provider_env: &mut HashMap<String, String>,
) -> Result<ProviderExecutionContext> {
    let execution_plan = prepare_turn_execution_plan_or_fail(&request).await?;
    let TurnExecutionPlan {
        execution_settings,
        runtime_plan,
    } = execution_plan;
    let source_env = match apply_runtime_source_env(
        request.provider_launch.data_root(),
        &request.session.provider_id,
        &runtime_plan,
        provider_env,
    )
    .await
    {
        Ok(source_env) => source_env,
        Err(err) => {
            emit_turn_start_failed(
                request.lifecycle,
                request.session,
                request.run_id,
                request.turn_id,
                request.message_id,
                &err,
            )
            .await;
            return Err(err);
        }
    };
    Ok(ProviderExecutionContext {
        execution_settings,
        runtime_plan,
        resolved_source: source_env.resolved_source,
        runtime_source_mode: source_env.runtime_source_mode,
        using_endpoint_source: source_env.using_endpoint_source,
    })
}

async fn prepare_turn_execution_plan_or_fail(
    request: &ProviderExecutionContextRequest<'_>,
) -> Result<TurnExecutionPlan> {
    match prepare_turn_execution_plan(
        request.provider_launch,
        request.store,
        request.session,
        request.execution_environment,
    )
    .await
    {
        Ok(execution_plan) => Ok(execution_plan),
        Err(err) => {
            emit_turn_start_failed(
                request.lifecycle,
                request.session,
                request.run_id,
                request.turn_id,
                request.message_id,
                &err,
            )
            .await;
            Err(err)
        }
    }
}
