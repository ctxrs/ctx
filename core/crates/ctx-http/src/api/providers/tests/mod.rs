use super::*;
use crate::api::provider_launch::get_install_statuses;
use chrono::Utc;
use ctx_provider_install::install_state::{InstallEventLevel, InstallProgressEvent};
use ctx_provider_install::ProviderInstallStatusesRouteRequest;
use ctx_provider_runtime::provider_launch::install::should_skip_install_for_healthy_provider;
use ctx_provider_runtime::provider_launch::probe_error::classify_probe_error;
use ctx_provider_runtime::provider_launch::status::apply_target_aware_provider_status;
use ctx_providers::adapters::{
    ProviderAdapter, ProviderHealth, ProviderProcessInfo, ProviderRestartMode, ProviderStatus,
    RunHandle, TurnInput,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ctx_harness_sources::HarnessEndpointVerificationStatus;
use ctx_provider_runtime::provider_auth::{
    selected_endpoint_from_harness_config, selected_endpoint_record_from_harness_config,
};
use ctx_provider_runtime::provider_launch::models::{
    endpoint_catalog_runtime_probe_failure, endpoint_catalog_verify_outcome,
    endpoint_models_payload,
};
use ctx_provider_runtime::provider_launch::options::endpoint_supports_model_catalog_verify;

fn test_endpoint(id: &str) -> harness_sources::HarnessEndpointRecord {
    harness_sources::HarnessEndpointRecord {
        id: id.to_string(),
        provider_id: "codex".to_string(),
        name: "Test endpoint".to_string(),
        base_url: Some("https://api.openai.com/v1".to_string()),
        api_shape: HarnessApiShape::OpenaiResponses,
        auth_type: "bearer".to_string(),
        model_override: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_verification_status: harness_sources::HarnessEndpointVerificationStatus::Unknown,
        last_verification_at: None,
        last_error: None,
        has_api_key: true,
        model_catalog_status: harness_sources::EndpointModelCatalogStatus::Unknown,
        model_catalog_fetched_at: None,
        model_catalog_error: None,
        model_catalog_models: Vec::new(),
        manual_model_ids: Vec::new(),
        model_catalog_source: None,
    }
}

mod auth_selection;
mod endpoint_catalog;
mod install_policy;
mod install_statuses;
mod login_auth;
mod restarts;
