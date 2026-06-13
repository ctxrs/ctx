use std::path::PathBuf;
use std::time::Duration;

use ctx_core::ids::WorkspaceId;
use ctx_core::redaction::redact_json_value;
use ctx_harness_sources as harness_sources;
use ctx_harness_sources::HarnessEndpointRecord;
use ctx_providers::adapters::ProviderStatus;
use serde_json::Value;

use crate::model_preferences::inject_preferred_model_id;
use crate::provider_auth::provider_auth_mode;
use crate::provider_harness_config;
use crate::provider_launch::models::{
    endpoint_models_payload, subscription_models_payload_from_status,
    supplement_models_payload_with_endpoint_metadata,
};
use crate::provider_options::cache::ProviderOptionsCacheSnapshot;
use crate::provider_options::response::{
    config_error_provider_options_response, unusable_provider_options_response,
};
use crate::ProviderRuntimeHost;

pub(super) struct ProviderOptionsErrorContext<'a> {
    pub(super) provider_id: &'a str,
    pub(super) workspace_id: WorkspaceId,
    pub(super) cache: &'a ProviderOptionsCacheSnapshot,
    pub(super) preferred_model_id: Option<String>,
    pub(super) verify_ttl: Duration,
}

pub(super) async fn managed_config_error_provider_options<H>(
    host: &H,
    context: ProviderOptionsErrorContext<'_>,
    config_error: &str,
    source_config: Option<&harness_sources::HarnessProviderSourceConfig>,
) -> Value
where
    H: ProviderRuntimeHost,
{
    let raw_resp = config_error_provider_options_response(
        context.provider_id,
        context.workspace_id,
        None,
        provider_auth_mode(false, source_config),
        config_error,
        source_config,
    );
    finalize_provider_options_response(
        host,
        ProviderOptionsResponseContext {
            provider_id: context.provider_id,
            provider_status: None,
            selected_endpoint: None,
            cache: context.cache,
            preferred_model_id: context.preferred_model_id,
        },
        raw_resp,
        false,
        context.verify_ttl,
    )
    .await
}

pub(super) async fn source_config_error_provider_options<H>(
    host: &H,
    context: ProviderOptionsErrorContext<'_>,
    provider_status: &ProviderStatus,
    config_error: &str,
    source_config: Option<&harness_sources::HarnessProviderSourceConfig>,
) -> Value
where
    H: ProviderRuntimeHost,
{
    let raw_resp = config_error_provider_options_response(
        context.provider_id,
        context.workspace_id,
        Some(provider_status.installed),
        provider_auth_mode(false, source_config),
        config_error,
        None,
    );
    finalize_provider_options_response(
        host,
        ProviderOptionsResponseContext {
            provider_id: context.provider_id,
            provider_status: Some(provider_status),
            selected_endpoint: None,
            cache: context.cache,
            preferred_model_id: context.preferred_model_id,
        },
        raw_resp,
        false,
        context.verify_ttl,
    )
    .await
}

pub(super) async fn auth_config_error_provider_options<H>(
    host: &H,
    context: ProviderOptionsErrorContext<'_>,
    provider_status: &ProviderStatus,
    config_error: &str,
) -> Value
where
    H: ProviderRuntimeHost,
{
    let raw_resp = config_error_provider_options_response(
        context.provider_id,
        context.workspace_id,
        Some(provider_status.installed),
        "none",
        config_error,
        None,
    );
    finalize_provider_options_response(
        host,
        ProviderOptionsResponseContext {
            provider_id: context.provider_id,
            provider_status: Some(provider_status),
            selected_endpoint: None,
            cache: context.cache,
            preferred_model_id: context.preferred_model_id,
        },
        raw_resp,
        false,
        context.verify_ttl,
    )
    .await
}

pub(super) async fn unusable_provider_options<H>(
    host: &H,
    context: ProviderOptionsErrorContext<'_>,
    provider_status: &ProviderStatus,
    has_active_auth: bool,
    auth_mode: &str,
    source_config: Option<&harness_sources::HarnessProviderSourceConfig>,
    selected_endpoint: Option<&HarnessEndpointRecord>,
) -> Value
where
    H: ProviderRuntimeHost,
{
    let raw_base_resp = unusable_provider_options_response(
        context.provider_id,
        context.workspace_id,
        provider_status,
        has_active_auth,
        auth_mode,
        source_config,
    );
    finalize_provider_options_response(
        host,
        ProviderOptionsResponseContext {
            provider_id: context.provider_id,
            provider_status: Some(provider_status),
            selected_endpoint,
            cache: context.cache,
            preferred_model_id: context.preferred_model_id,
        },
        raw_base_resp,
        true,
        context.verify_ttl,
    )
    .await
}

pub(super) struct ProviderOptionsResponseContext<'a> {
    pub(super) provider_id: &'a str,
    pub(super) provider_status: Option<&'a ProviderStatus>,
    pub(super) selected_endpoint: Option<&'a HarnessEndpointRecord>,
    pub(super) cache: &'a ProviderOptionsCacheSnapshot,
    pub(super) preferred_model_id: Option<String>,
}

pub(super) async fn finalize_provider_options_response<H>(
    host: &H,
    context: ProviderOptionsResponseContext<'_>,
    mut raw_response: Value,
    write_options_cache: bool,
    verify_ttl: Duration,
) -> Value
where
    H: ProviderRuntimeHost,
{
    if let Some(provider_status) = context.provider_status {
        attach_static_provider_models_and_modes(
            host,
            &mut raw_response,
            context.provider_id,
            provider_status,
            context.selected_endpoint,
            context.cache.cached_models(),
            context.cache.cached_modes(),
        )
        .await;
    }
    inject_preferred_model_id(&mut raw_response, context.preferred_model_id);

    let response = redact_json_value(raw_response);
    if write_options_cache {
        context
            .cache
            .store_response(host.provider_runtime(), response.clone())
            .await;
    }

    let mut out = response;
    context.cache.attach_verify_cache(&mut out, verify_ttl);
    out
}

async fn attach_static_provider_models_and_modes<H>(
    host: &H,
    value: &mut Value,
    provider_id: &str,
    provider_status: &ProviderStatus,
    selected_endpoint: Option<&HarnessEndpointRecord>,
    cached_models: Option<Value>,
    cached_modes: Option<Value>,
) where
    H: ProviderRuntimeHost,
{
    if let Some(endpoint) = selected_endpoint {
        let now = chrono::Utc::now();
        if value.get("models").is_none() || value.get("models").is_some_and(|next| next.is_null()) {
            value["models"] = endpoint_models_payload(provider_id, endpoint, now);
        } else {
            supplement_models_payload_with_endpoint_metadata(
                &mut value["models"],
                provider_id,
                endpoint,
                now,
            );
        }
        if harness_sources::endpoint_model_catalog_is_stale(endpoint, now) {
            let data_root = PathBuf::from(host.data_root());
            let provider_id_for_refresh = provider_id.to_string();
            let endpoint_id_for_refresh = endpoint.id.clone();
            tokio::spawn(async move {
                let _ = provider_harness_config::refresh_provider_endpoint_model_catalog(
                    &data_root,
                    &provider_id_for_refresh,
                    &endpoint_id_for_refresh,
                )
                .await;
            });
        }
    } else if value.get("models").is_none()
        || value.get("models").is_some_and(|next| next.is_null())
    {
        if let Some(models) = subscription_models_payload_from_status(provider_status) {
            value["models"] = models;
        }
    }

    if value.get("models").is_none() || value.get("models").is_some_and(|next| next.is_null()) {
        if let Some(models) = cached_models {
            value["models"] = models;
        }
    }
    if value.get("modes").is_none() || value.get("modes").is_some_and(|next| next.is_null()) {
        if let Some(modes) = cached_modes {
            value["modes"] = modes;
        }
    }
}
