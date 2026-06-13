use std::path::Path;

use ctx_core::ids::WorkspaceId;
use ctx_core::redaction;
use ctx_harness_sources::HarnessProviderSourceConfig;
use ctx_providers::adapters::ProviderStatus;

use crate::model_preferences::preferred_model_id_from_available_models;
use crate::provider_auth::{
    provider_auth_mode, provider_has_active_auth_config_with_runtime_root,
    selected_endpoint_record_from_harness_config,
};
use crate::provider_launch::models::{
    endpoint_models_payload, subscription_models_payload_from_status,
};
use crate::provider_usability::{provider_status_is_usable, provider_status_unusable_reason};

pub struct ProviderBootstrapOptions {
    pub provider_id: String,
    pub options: serde_json::Value,
    pub source_config: Option<HarnessProviderSourceConfig>,
}

pub fn should_build_bootstrap_options(provider_status: &ProviderStatus) -> bool {
    !provider_status.detail_flag("ui_hidden").unwrap_or(false)
}

pub fn visible_provider_count_hint(total_provider_count: usize) -> usize {
    total_provider_count.max(1)
}

pub async fn build_provider_bootstrap_options(
    data_root: &Path,
    workspace_id: WorkspaceId,
    provider_status: ProviderStatus,
    preferred_model_id: Option<String>,
) -> ProviderBootstrapOptions {
    let provider_id = provider_status.provider_id.clone();
    let (source_config, source_config_error) =
        crate::provider_launch::config::load_provider_source_config_with_error(
            data_root,
            &provider_id,
        )
        .await;
    let (has_active_auth, auth_mode, auth_config_error) = provider_auth_summary(
        data_root,
        &provider_id,
        source_config.as_ref(),
        &source_config_error,
    )
    .await;
    let options = provider_bootstrap_options_response(ProviderBootstrapOptionsResponseInput {
        workspace_id,
        provider_status: &provider_status,
        preferred_model_id,
        source_config: source_config.as_ref(),
        source_config_error: source_config_error.as_deref(),
        has_active_auth,
        auth_mode,
        auth_config_error: auth_config_error.as_deref(),
    });

    ProviderBootstrapOptions {
        provider_id,
        options,
        source_config,
    }
}

pub struct ProviderBootstrapOptionsResponseInput<'a> {
    pub workspace_id: WorkspaceId,
    pub provider_status: &'a ProviderStatus,
    pub preferred_model_id: Option<String>,
    pub source_config: Option<&'a HarnessProviderSourceConfig>,
    pub source_config_error: Option<&'a str>,
    pub has_active_auth: bool,
    pub auth_mode: &'a str,
    pub auth_config_error: Option<&'a str>,
}

pub fn provider_bootstrap_options_response(
    input: ProviderBootstrapOptionsResponseInput<'_>,
) -> serde_json::Value {
    let ProviderBootstrapOptionsResponseInput {
        workspace_id,
        provider_status,
        preferred_model_id,
        source_config,
        source_config_error,
        has_active_auth,
        auth_mode,
        auth_config_error,
    } = input;
    let provider_id = provider_status.provider_id.as_str();
    let (mut probe_ok, mut auth_required, mut probe_error) =
        bootstrap_provider_probe_summary(provider_status, has_active_auth);
    if let Some(config_error) = auth_config_error {
        probe_ok = false;
        auth_required = false;
        probe_error = Some(config_error.to_string());
    }

    let mut options = serde_json::json!({
        "provider_id": provider_id,
        "workspace_id": workspace_id.0.to_string(),
        "supports_load": false,
        "auth_required": auth_required,
        "has_active_auth": has_active_auth,
        "auth_mode": auth_mode,
        "probe_ok": probe_ok,
        "probed_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(probe_error) = probe_error {
        options["probe_error"] = serde_json::json!(probe_error);
    }
    if let Some(config_error) = source_config_error.or(auth_config_error) {
        options["probe_ok"] = serde_json::json!(false);
        options["probe_error"] = serde_json::json!(config_error);
        options["config_error"] = serde_json::json!(config_error);
    }
    append_model_options(provider_id, provider_status, source_config, &mut options);
    if let Some(preferred_model_id) =
        preferred_model_id_from_available_models(preferred_model_id, options.get("models"))
    {
        options["preferred_model_id"] = serde_json::json!(preferred_model_id);
    }

    if let Some(source) = source_config {
        options["source"] = serde_json::to_value(source).unwrap_or(serde_json::Value::Null);
    }

    options
}

async fn provider_auth_summary(
    data_root: &Path,
    provider_id: &str,
    source_config: Option<&HarnessProviderSourceConfig>,
    source_config_error: &Option<String>,
) -> (bool, &'static str, Option<String>) {
    // Bootstrap is auth/config hydration only. It must stay substrate-agnostic and
    // never cross into workspace runtime preparation.
    if source_config_error.is_some() {
        return (false, "none", None);
    }

    match provider_has_active_auth_config_with_runtime_root(
        data_root,
        None,
        provider_id,
        source_config,
    )
    .await
    {
        Ok(has_active_auth) => (
            has_active_auth,
            provider_auth_mode(has_active_auth, source_config),
            None,
        ),
        Err(err) => (false, "none", Some(redaction::redact_sensitive(&err))),
    }
}

fn bootstrap_provider_probe_summary(
    provider_status: &ProviderStatus,
    has_active_auth: bool,
) -> (bool, bool, Option<String>) {
    if !provider_status_is_usable(provider_status) {
        return (
            false,
            false,
            Some(
                provider_status_unusable_reason(provider_status)
                    .unwrap_or_else(|| "provider not ready for use".to_string()),
            ),
        );
    }

    (true, !has_active_auth, None)
}

fn append_model_options(
    provider_id: &str,
    provider_status: &ProviderStatus,
    source_config: Option<&HarnessProviderSourceConfig>,
    options: &mut serde_json::Value,
) {
    if let Some(endpoint) = selected_endpoint_record_from_harness_config(source_config) {
        options["models"] = endpoint_models_payload(provider_id, &endpoint, chrono::Utc::now());
    } else if let Some(models) = subscription_models_payload_from_status(provider_status) {
        options["models"] = models;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::Utc;
    use ctx_harness_sources::{
        EndpointModelCatalogStatus, HarnessApiShape, HarnessEndpointRecord,
        HarnessEndpointVerificationStatus, HarnessProviderSourceConfig, HarnessSourceKind,
    };
    use ctx_providers::adapters::{
        ProviderHealth, ProviderRecommendedAction, ProviderStatus, ProviderUsability,
        ProviderUsabilityStatus,
    };

    use super::*;

    fn provider_status(provider_id: &str) -> ProviderStatus {
        ProviderStatus {
            provider_id: provider_id.to_string(),
            installed: true,
            detected_path: None,
            version: None,
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ProviderUsability {
                usable: true,
                status: ProviderUsabilityStatus::Ready,
                reason_code: None,
                reason: None,
                blocking_provider_ids: Vec::new(),
                recommended_action: ProviderRecommendedAction::None,
            },
        }
    }

    fn endpoint_record() -> HarnessEndpointRecord {
        HarnessEndpointRecord {
            id: "endpoint-1".to_string(),
            provider_id: "codex".to_string(),
            name: "Codex endpoint".to_string(),
            base_url: Some("https://api.openai.test/v1".to_string()),
            api_shape: HarnessApiShape::OpenaiResponses,
            auth_type: "bearer".to_string(),
            model_override: Some("gpt-test".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_verification_status: HarnessEndpointVerificationStatus::Unknown,
            last_verification_at: None,
            last_error: None,
            has_api_key: true,
            model_catalog_status: EndpointModelCatalogStatus::Ready,
            model_catalog_fetched_at: None,
            model_catalog_error: None,
            model_catalog_models: Vec::new(),
            manual_model_ids: vec!["gpt-test".to_string(), "gpt-alt".to_string()],
            model_catalog_source: None,
        }
    }

    fn source_config(endpoint: HarnessEndpointRecord) -> HarnessProviderSourceConfig {
        HarnessProviderSourceConfig {
            provider_id: "codex".to_string(),
            selected_source_kind: HarnessSourceKind::Endpoint,
            selected_endpoint_id: Some(endpoint.id.clone()),
            endpoints: vec![endpoint],
        }
    }

    #[test]
    fn hidden_provider_is_excluded_from_bootstrap_option_building_only() {
        let mut hidden = provider_status("hidden");
        hidden.details.insert("ui_hidden".into(), "true".into());

        assert!(!should_build_bootstrap_options(&hidden));
        assert!(should_build_bootstrap_options(&provider_status("visible")));
        assert_eq!(visible_provider_count_hint(0), 1);
        assert_eq!(visible_provider_count_hint(3), 3);
    }

    #[test]
    fn usable_unauthenticated_provider_requires_auth() {
        let status = provider_status("codex");

        let options = provider_bootstrap_options_response(ProviderBootstrapOptionsResponseInput {
            workspace_id: WorkspaceId(uuid::Uuid::nil()),
            provider_status: &status,
            preferred_model_id: None,
            source_config: None,
            source_config_error: None,
            has_active_auth: false,
            auth_mode: "none",
            auth_config_error: None,
        });

        assert_eq!(options["provider_id"], "codex");
        assert_eq!(options["workspace_id"], uuid::Uuid::nil().to_string());
        assert_eq!(options["probe_ok"], true);
        assert_eq!(options["auth_required"], true);
        assert_eq!(options["has_active_auth"], false);
        assert_eq!(options["auth_mode"], "none");
    }

    #[test]
    fn unusable_provider_reports_probe_error() {
        let mut status = provider_status("codex");
        status.usability = ProviderUsability {
            usable: false,
            status: ProviderUsabilityStatus::Blocked,
            reason_code: Some("missing_dependency".to_string()),
            reason: Some("missing dependency: bun".to_string()),
            blocking_provider_ids: Vec::new(),
            recommended_action: ProviderRecommendedAction::ResolveDependency,
        };

        let options = provider_bootstrap_options_response(ProviderBootstrapOptionsResponseInput {
            workspace_id: WorkspaceId(uuid::Uuid::nil()),
            provider_status: &status,
            preferred_model_id: None,
            source_config: None,
            source_config_error: None,
            has_active_auth: true,
            auth_mode: "subscription",
            auth_config_error: None,
        });

        assert_eq!(options["probe_ok"], false);
        assert_eq!(options["auth_required"], false);
        assert_eq!(options["probe_error"], "missing dependency: bun");
        assert_eq!(options["has_active_auth"], true);
    }

    #[test]
    fn config_errors_fail_closed_and_surface_config_error() {
        let status = provider_status("codex");

        let options = provider_bootstrap_options_response(ProviderBootstrapOptionsResponseInput {
            workspace_id: WorkspaceId(uuid::Uuid::nil()),
            provider_status: &status,
            preferred_model_id: None,
            source_config: None,
            source_config_error: Some("stale selected endpoint"),
            has_active_auth: true,
            auth_mode: "endpoint",
            auth_config_error: None,
        });

        assert_eq!(options["probe_ok"], false);
        assert_eq!(options["auth_required"], false);
        assert_eq!(options["probe_error"], "stale selected endpoint");
        assert_eq!(options["config_error"], "stale selected endpoint");
    }

    #[test]
    fn auth_config_errors_fail_closed() {
        let status = provider_status("codex");

        let options = provider_bootstrap_options_response(ProviderBootstrapOptionsResponseInput {
            workspace_id: WorkspaceId(uuid::Uuid::nil()),
            provider_status: &status,
            preferred_model_id: None,
            source_config: None,
            source_config_error: None,
            has_active_auth: false,
            auth_mode: "none",
            auth_config_error: Some("auth load failed"),
        });

        assert_eq!(options["probe_ok"], false);
        assert_eq!(options["auth_required"], false);
        assert_eq!(options["probe_error"], "auth load failed");
        assert_eq!(options["config_error"], "auth load failed");
    }

    #[test]
    fn selected_endpoint_attaches_models_source_and_matching_preference() {
        let status = provider_status("codex");
        let source = source_config(endpoint_record());

        let options = provider_bootstrap_options_response(ProviderBootstrapOptionsResponseInput {
            workspace_id: WorkspaceId(uuid::Uuid::nil()),
            provider_status: &status,
            preferred_model_id: Some("gpt-alt".to_string()),
            source_config: Some(&source),
            source_config_error: None,
            has_active_auth: true,
            auth_mode: "endpoint",
            auth_config_error: None,
        });

        assert_eq!(options["auth_required"], false);
        assert_eq!(options["models"]["meta"]["source_kind"], "endpoint");
        assert_eq!(options["models"]["current_model_id"], "gpt-test");
        assert_eq!(options["models"]["models"][1]["id"], "gpt-alt");
        assert_eq!(options["preferred_model_id"], "gpt-alt");
        assert_eq!(options["source"]["selected_source_kind"], "endpoint");
    }

    #[test]
    fn subscription_models_attach_without_endpoint_and_filter_preference() {
        let mut status = provider_status("fake");
        status.version = Some("1.0.0".to_string());

        let options = provider_bootstrap_options_response(ProviderBootstrapOptionsResponseInput {
            workspace_id: WorkspaceId(uuid::Uuid::nil()),
            provider_status: &status,
            preferred_model_id: Some("not-in-catalog".to_string()),
            source_config: None,
            source_config_error: None,
            has_active_auth: true,
            auth_mode: "subscription",
            auth_config_error: None,
        });

        assert_eq!(options["models"]["meta"]["source_kind"], "subscription");
        assert_eq!(options["models"]["current_model_id"], "fake-model");
        assert!(options.get("preferred_model_id").is_none());
    }
}
