use anyhow::Result;

use self::admission::{apply_provider_turn_admission_env, ProviderTurnAdmissionEnvRequest};
use self::base_env::load_provider_setup_base_env;
use self::execution::{prepare_provider_execution_context, ProviderExecutionContextRequest};
use self::ready_event::{emit_provider_setup_ready_event, ProviderSetupReadyEvent};
pub(super) use self::types::{ProviderTurnRuntimeSetup, ProviderTurnRuntimeSetupRequest};
use super::helpers::runtime_provider_id_for_session_provider;
use super::provider_env::{
    prepare_provider_runtime_environment, ProviderRuntimeEnvironmentRequest,
};
use super::provider_spawn::prepare_provider_adapter_for_turn;
use super::turn_failure::emit_turn_start_failed;
use super::turn_start::apply_crp_launch_policy_env_for_control_mode;

mod admission;
mod base_env;
mod execution;
mod ready_event;
mod types;

pub(super) async fn prepare_provider_turn_runtime(
    request: ProviderTurnRuntimeSetupRequest<'_>,
) -> Result<ProviderTurnRuntimeSetup> {
    let ProviderTurnRuntimeSetupRequest {
        turn_runtime: _turn_runtime,
        provider_launch,
        lifecycle,
        store,
        session,
        run_id,
        turn_id,
        message_id,
        workdir_str,
        full_model_id,
        execution_environment,
        session_root_kind,
    } = request;

    let base_env = load_provider_setup_base_env(provider_launch, session, full_model_id).await?;
    let mut provider_env = base_env.provider_env;
    let provider_control_mode = base_env.provider_control_mode;

    let execution_context = prepare_provider_execution_context(
        ProviderExecutionContextRequest {
            provider_launch,
            lifecycle,
            store,
            session,
            run_id,
            turn_id,
            message_id,
            execution_environment,
        },
        &mut provider_env,
    )
    .await?;
    let execution_settings = execution_context.execution_settings;
    let runtime_plan = execution_context.runtime_plan;
    let is_linux_sandbox = runtime_plan.is_linux_sandbox();
    let resolved_source = execution_context.resolved_source;
    let runtime_source_mode = execution_context.runtime_source_mode;
    let using_endpoint_source = execution_context.using_endpoint_source;

    if let Err(err) = apply_provider_turn_admission_env(
        &mut provider_env,
        ProviderTurnAdmissionEnvRequest {
            provider_launch,
            store,
            session,
            run_id,
            provider_id: &session.provider_id,
            model_id: full_model_id,
            execution_environment,
            container_network_mode: execution_settings.container.network_mode.clone(),
            source_kind: resolved_source.source_kind,
        },
    )
    .await
    {
        emit_turn_start_failed(lifecycle, session, run_id, turn_id, message_id, &err).await;
        return Err(err);
    }

    let runtime_provider_id =
        runtime_provider_id_for_session_provider(&session.provider_id, &resolved_source)
            .to_string();
    if runtime_provider_id != session.provider_id {
        provider_env.insert(
            "CTX_PROVIDER_RUNTIME_ID".to_string(),
            runtime_provider_id.clone(),
        );
    }
    let prepared_adapter = match prepare_provider_adapter_for_turn(
        provider_launch,
        &runtime_provider_id,
        is_linux_sandbox,
    )
    .await
    {
        Ok(prepared_adapter) => prepared_adapter,
        Err(err) => {
            emit_turn_start_failed(lifecycle, session, run_id, turn_id, message_id, &err).await;
            return Err(err);
        }
    };

    prepare_provider_runtime_environment(ProviderRuntimeEnvironmentRequest {
        provider_launch,
        provider_env: &mut provider_env,
        runtime_provider_id: &runtime_provider_id,
        runtime_plan: &runtime_plan,
        is_linux_sandbox,
        runtime_source_mode,
        adapter_cfg: &prepared_adapter.adapter_cfg,
        install_target: prepared_adapter.install_target,
    })
    .await?;
    apply_crp_launch_policy_env_for_control_mode(&mut provider_env, &provider_control_mode);

    emit_provider_setup_ready_event(ProviderSetupReadyEvent {
        provider_launch,
        session,
        run_id,
        turn_id,
        workdir_str,
        full_model_id,
        execution_environment,
        session_root_kind,
        runtime_provider_id: &runtime_provider_id,
        using_endpoint_source,
        is_linux_sandbox,
        runtime_plan: &runtime_plan,
        provider_env: &provider_env,
    });

    Ok(ProviderTurnRuntimeSetup {
        provider_env,
        runtime_provider_id,
        adapter: prepared_adapter.adapter,
    })
}
