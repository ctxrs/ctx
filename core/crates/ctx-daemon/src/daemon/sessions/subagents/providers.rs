use std::collections::{HashMap, HashSet};

use ctx_core::models::{ExecutionEnvironment, Workspace};
use ctx_session_tools::model_resolution::ModelCatalog;

use super::errors::{
    api_error, internal_api_error, internal_request_or_policy_error, ApiResult, SubagentErrorKind,
};
use super::SubagentSpawnHost;

pub(super) async fn load_requested_model_catalogs(
    host: &SubagentSpawnHost,
    workspace: &Workspace,
    provider_ids: &HashSet<String>,
    execution_environment: ExecutionEnvironment,
) -> ApiResult<HashMap<String, Option<ModelCatalog>>> {
    let install_target = host
        .effective_install_target_for_environment(workspace.id, execution_environment)
        .await
        .map_err(internal_request_or_policy_error)?;
    let managed =
        ctx_provider_runtime::provider_launch::config::load_managed_agent_server_config_or_err(
            host.data_root(),
        )
        .await
        .map_err(internal_api_error)?;
    let matrix = host.load_provider_matrix().await;
    let known_providers = host.known_harness_provider_ids(&matrix).await;

    let mut available_providers = known_providers.iter().cloned().collect::<Vec<_>>();
    available_providers.sort();

    for provider_id in provider_ids {
        if !known_providers.contains(provider_id) {
            return Err(api_error(
                SubagentErrorKind::BadRequest,
                format!(
                    "unknown harness '{provider_id}'; available harnesses: {}",
                    available_providers.join(", ")
                ),
            ));
        }

        if let Some(reason) = host
            .provider_unusable_reason_for_target(&managed, &matrix, provider_id, install_target)
            .await
        {
            return Err(api_error(
                SubagentErrorKind::BadRequest,
                format!("harness '{provider_id}' is not ready: {reason}"),
            ));
        }
    }

    let mut model_catalogs = HashMap::new();
    for provider_id in provider_ids {
        let catalog = host
            .load_provider_model_catalog_for_execution_environment(
                workspace,
                provider_id,
                execution_environment,
            )
            .await;
        match catalog {
            Ok(catalog) => {
                model_catalogs.insert(provider_id.clone(), catalog);
            }
            Err(error) => {
                return Err(api_error(SubagentErrorKind::BadRequest, error));
            }
        }
    }

    Ok(model_catalogs)
}
