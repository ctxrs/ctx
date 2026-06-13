use super::{AppServerSessionState, TurnAliasState, TurnTracker};
use crate::app_server::{AppServerClient, ModelListResponse, ThreadStartLikeResponse};
use crate::builtins::{
    build_app_server_config_overrides, build_app_server_launch_config_overrides,
    build_current_model_id, build_model_infos, build_session_command_infos, command_names,
    merge_config_overrides, normalize_ctx_system_prompt_append, resolve_codex_home,
    split_model_and_effort,
};
use crate::protocol::{CrpModelInfo, CrpSessionConfig};
use crate::RuntimeOptions;
use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;

pub(super) struct ModelsProbe {
    pub(super) models: Vec<CrpModelInfo>,
    pub(super) current_model_id: Option<String>,
    pub(super) catalog_source: Option<String>,
}

pub(super) async fn open_session(
    session_config: CrpSessionConfig,
    provider_session_id: Option<String>,
    options: &RuntimeOptions,
) -> Result<AppServerSessionState> {
    let workdir = session_config
        .spawn_cwd
        .clone()
        .or_else(|| session_config.cwd.clone())
        .unwrap_or(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let launch_config_overrides = build_app_server_launch_config_overrides(&session_config);
    let mut client = AppServerClient::start(&workdir, &launch_config_overrides).await?;
    let developer_instructions =
        normalize_ctx_system_prompt_append(std::env::var("CTX_SYSTEM_PROMPT_APPEND").ok());
    let config_overrides = session_config_overrides(&session_config, options);
    let effort = session_effort(&session_config);
    let (thread_response, thread_id, resumed_from_provider_session) = if let Some(
        provider_session_id,
    ) = provider_session_id
    {
        match client
                .request::<ThreadStartLikeResponse>(
                    "thread/resume",
                    json!({
                        "threadId": provider_session_id,
                        "cwd": session_config.cwd.as_ref().map(|path| path.to_string_lossy().to_string()),
                        "model": session_config.model.as_ref().map(|model| split_model_and_effort(model).0),
                        "effort": effort,
                        "modelProvider": session_config.model_provider,
                        "approvalPolicy": session_config.approval_policy,
                        "sandbox": session_config.sandbox_mode,
                        "config": config_overrides,
                        "developerInstructions": developer_instructions,
                        "personality": session_config.personality,
                        "persistExtendedHistory": false,
                    }),
                )
                .await
            {
                Ok(response) => {
                    let thread_id = response.thread.id.clone();
                    (response, thread_id, true)
                }
                Err(err) => {
                    anyhow::bail!(
                        "failed to resume Codex provider session `{}`: {err}",
                        provider_session_id
                    );
                }
            }
    } else {
        let response = start_thread(
            &mut client,
            &session_config,
            developer_instructions,
            options,
        )
        .await?;
        let thread_id = response.thread.id.clone();
        (response, thread_id, false)
    };

    let codex_home = resolve_codex_home();
    let opened_commands = build_session_command_infos(&codex_home);
    let opened_slash_commands = command_names(&opened_commands);

    Ok(AppServerSessionState {
        tracker: TurnTracker::new(String::new()),
        client,
        thread_id,
        default_cwd: PathBuf::from(thread_response.cwd),
        default_model: thread_response.model,
        default_effort: thread_response.reasoning_effort,
        opened_commands,
        opened_slash_commands,
        turn_aliases: TurnAliasState::new(),
        resumed_from_provider_session,
        command_execution_seen: false,
    })
}

pub(super) async fn probe_models(
    config: CrpSessionConfig,
    _options: &RuntimeOptions,
) -> Result<ModelsProbe> {
    let workdir = config
        .spawn_cwd
        .clone()
        .or_else(|| config.cwd.clone())
        .unwrap_or(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let launch_config_overrides = build_app_server_launch_config_overrides(&config);
    let mut client = AppServerClient::start(&workdir, &launch_config_overrides).await?;
    let response = client
        .request::<ModelListResponse>("model/list", json!({ "includeHidden": false }))
        .await?;
    client.shutdown().await;
    let models = build_model_infos(&response.data);
    let current_model_id = build_current_model_id(Some(&config), &response.data, None, None);
    Ok(ModelsProbe {
        models,
        current_model_id,
        catalog_source: Some("live_remote".to_string()),
    })
}

async fn start_thread(
    client: &mut AppServerClient,
    session_config: &CrpSessionConfig,
    developer_instructions: Option<String>,
    options: &RuntimeOptions,
) -> Result<ThreadStartLikeResponse> {
    let config_overrides = session_config_overrides(session_config, options);
    client
        .request::<ThreadStartLikeResponse>(
            "thread/start",
            json!({
                "model": session_config.model.as_ref().map(|model| split_model_and_effort(model).0),
                "effort": session_effort(session_config),
                "modelProvider": session_config.model_provider,
                "cwd": session_config.cwd.as_ref().map(|path| path.to_string_lossy().to_string()),
                "approvalPolicy": session_config.approval_policy,
                "sandbox": session_config.sandbox_mode,
                "config": config_overrides,
                "developerInstructions": developer_instructions,
                "personality": session_config.personality,
                "ephemeral": Value::Null,
                "experimentalRawEvents": false,
                "persistExtendedHistory": false,
            }),
        )
        .await
}

fn session_config_overrides(
    session_config: &CrpSessionConfig,
    options: &RuntimeOptions,
) -> Option<Value> {
    merge_config_overrides(
        options.config_overrides.clone(),
        build_app_server_config_overrides(session_config),
    )
}

fn session_effort(config: &CrpSessionConfig) -> Option<String> {
    config
        .model
        .as_deref()
        .and_then(|model| split_model_and_effort(model).1)
        .or_else(|| {
            config
                .reasoning_effort
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}
