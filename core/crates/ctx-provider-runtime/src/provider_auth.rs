use std::path::Path as StdPath;

use ctx_core::provider_ids::CODEX_PROVIDER_ID;
use ctx_harness_sources as harness_sources;
use ctx_harness_sources::{
    HarnessEndpointRecord, HarnessRouteBackend, HarnessRuntimeSourceMode, HarnessSourceKind,
};
use ctx_provider_accounts as provider_accounts;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAuthMode {
    None,
    Subscription,
    Endpoint(HarnessRouteBackend),
}

impl ProviderAuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Subscription => "subscription",
            Self::Endpoint(_) => "endpoint",
        }
    }

    pub fn runtime_source_mode(self) -> Option<HarnessRuntimeSourceMode> {
        match self {
            Self::None => None,
            Self::Subscription => Some(HarnessRuntimeSourceMode::Subscription),
            Self::Endpoint(backend) => Some(HarnessRuntimeSourceMode::Endpoint(backend)),
        }
    }
}

pub fn endpoint_selection_is_active(config: &harness_sources::HarnessProviderSourceConfig) -> bool {
    if config.selected_source_kind != HarnessSourceKind::Endpoint {
        return false;
    }
    let Some(selected_endpoint_id) = config.selected_endpoint_id.as_deref() else {
        return false;
    };
    config.endpoints.iter().any(|endpoint| {
        endpoint.id == selected_endpoint_id
            && (endpoint.has_api_key
                || endpoint.route_backend() == HarnessRouteBackend::CtxManagedRelay)
    })
}

pub fn selected_endpoint_from_harness_config(
    config: Option<harness_sources::HarnessProviderSourceConfig>,
) -> Option<String> {
    config.and_then(|cfg| {
        if cfg.selected_source_kind == HarnessSourceKind::Endpoint {
            cfg.selected_endpoint_id
        } else {
            None
        }
    })
}

pub fn selected_endpoint_record_from_harness_config(
    config: Option<&harness_sources::HarnessProviderSourceConfig>,
) -> Option<HarnessEndpointRecord> {
    let cfg = config?;
    if cfg.selected_source_kind != HarnessSourceKind::Endpoint {
        return None;
    }
    let selected_id = cfg.selected_endpoint_id.as_deref()?;
    cfg.endpoints
        .iter()
        .find(|endpoint| endpoint.id == selected_id)
        .cloned()
}

pub async fn provider_has_active_auth_config(
    data_root: &StdPath,
    provider_id: &str,
    source_config: Option<&harness_sources::HarnessProviderSourceConfig>,
) -> Result<bool, String> {
    provider_has_active_auth_config_with_runtime_root(data_root, None, provider_id, source_config)
        .await
}

pub async fn provider_has_active_auth_config_with_runtime_root(
    data_root: &StdPath,
    runtime_data_root: Option<&StdPath>,
    provider_id: &str,
    source_config: Option<&harness_sources::HarnessProviderSourceConfig>,
) -> Result<bool, String> {
    if provider_id == "fake" {
        return Ok(true);
    }
    if let Some(config) = source_config {
        if endpoint_selection_is_active(config) {
            return Ok(true);
        }
    }
    if provider_id == CODEX_PROVIDER_ID {
        return match runtime_data_root {
            Some(runtime_root) => {
                provider_accounts::codex_has_active_auth_with_runtime_root(data_root, runtime_root)
                    .await
            }
            None => provider_accounts::codex_has_active_auth(data_root).await,
        }
        .map_err(|err| err.to_string());
    }
    let env = match runtime_data_root {
        Some(runtime_root) => {
            provider_accounts::subscription_env_for_active_account_with_runtime_root(
                data_root,
                runtime_root,
                provider_id,
            )
            .await
        }
        None => {
            provider_accounts::subscription_env_for_active_account(data_root, provider_id).await
        }
    };
    env.map(|env| !env.is_empty())
        .map_err(|err| err.to_string())
}

pub fn provider_auth_mode(
    has_active_auth: bool,
    source_config: Option<&harness_sources::HarnessProviderSourceConfig>,
) -> &'static str {
    provider_auth_mode_detail(has_active_auth, source_config).as_str()
}

pub fn provider_auth_mode_detail(
    has_active_auth: bool,
    source_config: Option<&harness_sources::HarnessProviderSourceConfig>,
) -> ProviderAuthMode {
    if !has_active_auth {
        return ProviderAuthMode::None;
    }
    if let Some(config) = source_config {
        if endpoint_selection_is_active(config) {
            return match config.selected_runtime_source_mode() {
                HarnessRuntimeSourceMode::Subscription => ProviderAuthMode::Subscription,
                HarnessRuntimeSourceMode::Endpoint(backend) => ProviderAuthMode::Endpoint(backend),
            };
        }
    }
    ProviderAuthMode::Subscription
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ctx_harness_sources::{
        EndpointModelCatalogStatus, HarnessApiShape, HarnessEndpointRecord,
        HarnessEndpointVerificationStatus,
    };

    fn sample_endpoint(has_api_key: bool) -> HarnessEndpointRecord {
        HarnessEndpointRecord {
            id: "endpoint-1".to_string(),
            provider_id: CODEX_PROVIDER_ID.to_string(),
            name: "Codex endpoint".to_string(),
            base_url: Some("https://api.openai.com/v1".to_string()),
            api_shape: HarnessApiShape::OpenaiResponses,
            auth_type: "bearer".to_string(),
            model_override: Some("gpt-5.4".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_verification_status: HarnessEndpointVerificationStatus::Unknown,
            last_verification_at: None,
            last_error: None,
            has_api_key,
            model_catalog_status: EndpointModelCatalogStatus::Unknown,
            model_catalog_fetched_at: None,
            model_catalog_error: None,
            model_catalog_models: Vec::new(),
            manual_model_ids: Vec::new(),
            model_catalog_source: None,
        }
    }

    #[test]
    fn endpoint_selection_is_active_requires_credentialed_selected_endpoint() {
        let config = harness_sources::HarnessProviderSourceConfig {
            provider_id: CODEX_PROVIDER_ID.to_string(),
            selected_source_kind: HarnessSourceKind::Endpoint,
            selected_endpoint_id: Some("endpoint-1".to_string()),
            endpoints: vec![sample_endpoint(false)],
        };

        assert!(!endpoint_selection_is_active(&config));
    }

    #[test]
    fn selected_endpoint_from_harness_config_prefers_endpoint_selection() {
        let selected = selected_endpoint_from_harness_config(Some(
            harness_sources::HarnessProviderSourceConfig {
                provider_id: CODEX_PROVIDER_ID.to_string(),
                selected_source_kind: HarnessSourceKind::Endpoint,
                selected_endpoint_id: Some("endpoint-1".to_string()),
                endpoints: vec![],
            },
        ));
        assert_eq!(selected.as_deref(), Some("endpoint-1"));

        let subscription = selected_endpoint_from_harness_config(Some(
            harness_sources::HarnessProviderSourceConfig {
                provider_id: CODEX_PROVIDER_ID.to_string(),
                selected_source_kind: HarnessSourceKind::Subscription,
                selected_endpoint_id: Some("endpoint-1".to_string()),
                endpoints: vec![],
            },
        ));
        assert!(subscription.is_none());
    }

    #[test]
    fn selected_endpoint_record_from_harness_config_returns_selected_record() {
        let selected = selected_endpoint_record_from_harness_config(Some(
            &harness_sources::HarnessProviderSourceConfig {
                provider_id: CODEX_PROVIDER_ID.to_string(),
                selected_source_kind: HarnessSourceKind::Endpoint,
                selected_endpoint_id: Some("endpoint-2".to_string()),
                endpoints: vec![sample_endpoint(true), {
                    let mut endpoint = sample_endpoint(true);
                    endpoint.id = "endpoint-2".to_string();
                    endpoint
                }],
            },
        ))
        .expect("selected endpoint");
        assert_eq!(selected.id, "endpoint-2");

        let missing = selected_endpoint_record_from_harness_config(Some(
            &harness_sources::HarnessProviderSourceConfig {
                provider_id: CODEX_PROVIDER_ID.to_string(),
                selected_source_kind: HarnessSourceKind::Endpoint,
                selected_endpoint_id: Some("endpoint-3".to_string()),
                endpoints: vec![sample_endpoint(true)],
            },
        ));
        assert!(missing.is_none());
    }

    #[test]
    fn provider_auth_mode_falls_back_when_selected_endpoint_lacks_credentials() {
        let config = harness_sources::HarnessProviderSourceConfig {
            provider_id: CODEX_PROVIDER_ID.to_string(),
            selected_source_kind: HarnessSourceKind::Endpoint,
            selected_endpoint_id: Some("endpoint-1".to_string()),
            endpoints: vec![sample_endpoint(false)],
        };

        assert_eq!(provider_auth_mode(true, Some(&config)), "subscription");
        assert_eq!(
            provider_auth_mode_detail(true, Some(&config)),
            ProviderAuthMode::Subscription
        );
    }

    #[test]
    fn provider_auth_mode_detail_marks_direct_endpoint_selection_as_user_managed() {
        let config = harness_sources::HarnessProviderSourceConfig {
            provider_id: CODEX_PROVIDER_ID.to_string(),
            selected_source_kind: HarnessSourceKind::Endpoint,
            selected_endpoint_id: Some("endpoint-1".to_string()),
            endpoints: vec![sample_endpoint(true)],
        };

        assert_eq!(
            provider_auth_mode_detail(true, Some(&config)),
            ProviderAuthMode::Endpoint(HarnessRouteBackend::UserManaged)
        );
    }

    #[test]
    fn provider_auth_mode_detail_marks_ctx_managed_endpoint_selection() {
        let mut endpoint = sample_endpoint(false);
        endpoint.base_url = Some("https://api.ctx.rs/relay/openai/v1".to_string());
        let config = harness_sources::HarnessProviderSourceConfig {
            provider_id: CODEX_PROVIDER_ID.to_string(),
            selected_source_kind: HarnessSourceKind::Endpoint,
            selected_endpoint_id: Some("endpoint-1".to_string()),
            endpoints: vec![endpoint],
        };

        assert_eq!(
            provider_auth_mode_detail(true, Some(&config)),
            ProviderAuthMode::Endpoint(HarnessRouteBackend::CtxManagedRelay)
        );
    }
}
