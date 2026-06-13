use std::path::Path;

use ctx_harness_sources as harness_sources;
use ctx_observability::logs;
use ctx_provider_runtime::provider_harness_config as harness_config_service;
use ctx_provider_runtime::{
    ProviderHarnessConfigRouteError, ProviderHarnessEndpointRouteError,
    ProviderHarnessSourceConfig, ProviderRuntime, SelectProviderHarnessSourceRouteRequest,
    SetProviderHarnessEndpointManualModelsRouteRequest, UpsertProviderHarnessEndpointRouteRequest,
};

use crate::daemon::ProviderHarnessConfigHandle;

impl ProviderHarnessConfigHandle {
    pub async fn get_provider_harness_config_for_route(
        &self,
        provider_id: &str,
    ) -> Result<ProviderHarnessSourceConfig, ProviderHarnessConfigRouteError> {
        get_provider_harness_config(self.data_root(), provider_id)
            .await
            .map_err(provider_harness_config_bad_request_error)
    }

    pub async fn select_provider_harness_source_for_route(
        &self,
        provider_id: &str,
        request: SelectProviderHarnessSourceRouteRequest,
    ) -> Result<ProviderHarnessSourceConfig, ProviderHarnessConfigRouteError> {
        let (source_kind, endpoint_id) = request.into_parts();
        select_provider_harness_source(
            self.providers(),
            self.data_root(),
            provider_id,
            source_kind,
            endpoint_id,
        )
        .await
        .map_err(provider_harness_config_bad_request_error)
    }

    pub async fn upsert_provider_harness_endpoint_for_route(
        &self,
        provider_id: &str,
        request: UpsertProviderHarnessEndpointRouteRequest,
    ) -> Result<ProviderHarnessSourceConfig, ProviderHarnessEndpointRouteError> {
        let (
            endpoint_id,
            name,
            base_url,
            api_shape,
            auth_type,
            model_override,
            api_key,
            service_account_json,
            project_id,
            location,
            manual_model_ids,
        ) = request.into_parts();
        let endpoint = harness_sources::HarnessEndpointUpsert {
            endpoint_id,
            name,
            base_url,
            api_shape,
            auth_type,
            model_override,
            api_key,
            service_account_json,
            project_id,
            location,
        };
        upsert_provider_harness_endpoint(
            self.providers(),
            self.data_root(),
            provider_id,
            endpoint,
            manual_model_ids,
        )
        .await
        .map_err(provider_harness_endpoint_bad_request_error)
    }

    pub async fn refresh_provider_harness_endpoint_models_for_route(
        &self,
        provider_id: &str,
        endpoint_id: &str,
    ) -> Result<ProviderHarnessSourceConfig, ProviderHarnessEndpointRouteError> {
        refresh_provider_harness_endpoint_models(
            self.providers(),
            self.data_root(),
            provider_id,
            endpoint_id,
        )
        .await
        .map_err(provider_harness_endpoint_bad_request_error)
    }

    pub async fn set_provider_harness_endpoint_manual_models_for_route(
        &self,
        provider_id: &str,
        endpoint_id: &str,
        request: SetProviderHarnessEndpointManualModelsRouteRequest,
    ) -> Result<ProviderHarnessSourceConfig, ProviderHarnessEndpointRouteError> {
        set_provider_harness_endpoint_manual_models(
            self.providers(),
            self.data_root(),
            provider_id,
            endpoint_id,
            request.into_model_ids(),
        )
        .await
        .map_err(provider_harness_endpoint_bad_request_error)
    }

    pub async fn delete_provider_harness_endpoint_for_route(
        &self,
        provider_id: &str,
        endpoint_id: &str,
    ) -> Result<ProviderHarnessSourceConfig, ProviderHarnessEndpointRouteError> {
        delete_provider_harness_endpoint(
            self.providers(),
            self.data_root(),
            provider_id,
            endpoint_id,
        )
        .await
        .map_err(provider_harness_endpoint_delete_error)
    }
}

async fn get_provider_harness_config(
    data_root: &Path,
    provider_id: &str,
) -> anyhow::Result<harness_sources::HarnessProviderSourceConfig> {
    harness_config_service::get_provider_harness_config(data_root, provider_id).await
}

async fn select_provider_harness_source(
    providers: &ProviderRuntime,
    data_root: &Path,
    provider_id: &str,
    source_kind: harness_sources::HarnessSourceKind,
    endpoint_id: Option<String>,
) -> anyhow::Result<harness_sources::HarnessProviderSourceConfig> {
    harness_config_service::select_provider_harness_source(
        providers,
        data_root,
        provider_id,
        source_kind,
        endpoint_id,
    )
    .await
}

async fn upsert_provider_harness_endpoint(
    providers: &ProviderRuntime,
    data_root: &Path,
    provider_id: &str,
    endpoint: harness_sources::HarnessEndpointUpsert,
    manual_model_ids: Option<Vec<String>>,
) -> anyhow::Result<harness_sources::HarnessProviderSourceConfig> {
    harness_config_service::upsert_provider_harness_endpoint(
        providers,
        data_root,
        provider_id,
        endpoint,
        manual_model_ids,
    )
    .await
}

async fn refresh_provider_harness_endpoint_models(
    providers: &ProviderRuntime,
    data_root: &Path,
    provider_id: &str,
    endpoint_id: &str,
) -> anyhow::Result<harness_sources::HarnessProviderSourceConfig> {
    harness_config_service::refresh_provider_harness_endpoint_models(
        providers,
        data_root,
        provider_id,
        endpoint_id,
    )
    .await
}

async fn set_provider_harness_endpoint_manual_models(
    providers: &ProviderRuntime,
    data_root: &Path,
    provider_id: &str,
    endpoint_id: &str,
    model_ids: Vec<String>,
) -> anyhow::Result<harness_sources::HarnessProviderSourceConfig> {
    harness_config_service::set_provider_harness_endpoint_manual_models(
        providers,
        data_root,
        provider_id,
        endpoint_id,
        model_ids,
    )
    .await
}

async fn delete_provider_harness_endpoint(
    providers: &ProviderRuntime,
    data_root: &Path,
    provider_id: &str,
    endpoint_id: &str,
) -> anyhow::Result<harness_sources::HarnessProviderSourceConfig> {
    harness_config_service::delete_provider_harness_endpoint(
        providers,
        data_root,
        provider_id,
        endpoint_id,
    )
    .await
}

fn provider_harness_endpoint_bad_request_error(
    error: anyhow::Error,
) -> ProviderHarnessEndpointRouteError {
    ProviderHarnessEndpointRouteError::bad_request(logs::redact_sensitive(&error.to_string()))
}

fn provider_harness_config_bad_request_error(
    error: anyhow::Error,
) -> ProviderHarnessConfigRouteError {
    ProviderHarnessConfigRouteError::bad_request(logs::redact_sensitive(&error.to_string()))
}

fn provider_harness_endpoint_delete_error(
    error: anyhow::Error,
) -> ProviderHarnessEndpointRouteError {
    let message = logs::redact_sensitive(&error.to_string());
    if message.contains("unknown endpoint") {
        ProviderHarnessEndpointRouteError::not_found(message)
    } else {
        ProviderHarnessEndpointRouteError::bad_request(message)
    }
}
