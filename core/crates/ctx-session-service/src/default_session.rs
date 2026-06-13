use std::collections::BTreeMap;

use ctx_core::provider_ids::CODEX_PROVIDER_ID;
use ctx_provider_runtime::provider_launch::models::subscription_models_payload_from_status;
use ctx_providers::adapters::{ProviderHealth, ProviderStatus};
use ctx_session_tools::model_resolution::{resolve_model_id, ModelCatalog};

const PREFERRED_DEFAULT_PROVIDER_IDS: &[&str] = &[
    CODEX_PROVIDER_ID,
    "claude-crp",
    "gemini",
    "qwen",
    "opencode",
    "mistral",
    "kimi",
    "auggie",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultSessionModelTarget {
    pub model_id: String,
    pub reasoning_effort: Option<String>,
}

pub fn select_default_provider_id(statuses: &[ProviderStatus]) -> Option<String> {
    let is_visible = |status: &ProviderStatus| !status.detail_flag("ui_hidden").unwrap_or(false);
    let is_installed = |status: &ProviderStatus| status.installed;
    let is_ready = |status: &ProviderStatus| {
        status.installed && status.health == ProviderHealth::Ok && status.is_usable()
    };
    let mut provider_statuses = BTreeMap::new();
    for status in statuses {
        provider_statuses
            .entry(status.provider_id.clone())
            .or_insert(status);
    }

    for preferred in PREFERRED_DEFAULT_PROVIDER_IDS {
        if provider_statuses
            .get(*preferred)
            .is_some_and(|status| is_visible(status) && is_ready(status))
        {
            return Some((*preferred).to_string());
        }
    }

    provider_statuses
        .iter()
        .filter(|(_, status)| is_visible(status) && is_ready(status))
        .map(|(provider_id, _)| provider_id.clone())
        .next()
        .or_else(|| {
            provider_statuses
                .iter()
                .filter(|(_, status)| is_ready(status))
                .map(|(provider_id, _)| provider_id.clone())
                .next()
        })
        .or_else(|| {
            provider_statuses
                .iter()
                .filter(|(_, status)| is_visible(status) && is_installed(status))
                .map(|(provider_id, _)| provider_id.clone())
                .next()
        })
        .or_else(|| {
            provider_statuses
                .iter()
                .filter(|(_, status)| is_installed(status))
                .map(|(provider_id, _)| provider_id.clone())
                .next()
        })
}

pub fn resolve_default_session_model(
    preferred_model_id: Option<&str>,
    catalog: Option<&ModelCatalog>,
    provider_status: Option<&ProviderStatus>,
) -> Result<DefaultSessionModelTarget, String> {
    let fallback_model = catalog
        .and_then(ModelCatalog::default_model_id)
        .map(str::to_string)
        .or_else(|| {
            provider_status.and_then(|status| {
                subscription_models_payload_from_status(status).and_then(|value| {
                    value
                        .get("current_model_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
            })
        });
    let resolved_model =
        resolve_model_id(preferred_model_id, None, fallback_model.as_deref(), catalog)
            .map_err(|error| error.to_string())?;
    Ok(DefaultSessionModelTarget {
        model_id: resolved_model.model_id,
        reasoning_effort: resolved_model.reasoning_effort,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_providers::adapters::{
        ProviderRecommendedAction, ProviderUsability, ProviderUsabilityStatus,
    };
    use serde_json::json;
    use std::collections::HashMap;

    fn status(provider_id: &str) -> ProviderStatus {
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

    #[test]
    fn selects_canonical_codex_crp_for_default_session_creation() {
        let statuses = vec![status("codex")];

        assert_eq!(
            select_default_provider_id(&statuses),
            Some("codex".to_string())
        );
    }

    #[test]
    fn falls_back_to_stable_canonical_order_for_nonpreferred_providers() {
        let statuses = vec![status("zeta"), status("alpha")];

        assert_eq!(
            select_default_provider_id(&statuses),
            Some("alpha".to_string())
        );
    }

    #[test]
    fn resolves_preferred_model_from_catalog() {
        let catalog = ctx_session_tools::model_resolution::build_model_catalog(&json!({
            "models": [
                { "id": "gpt-5.4/medium", "name": "GPT-5.4 (medium)" },
                { "id": "gpt-5.4/high", "name": "GPT-5.4 (high)" }
            ],
            "current_model_id": "gpt-5.4/medium"
        }))
        .expect("model catalog fixture should be valid");

        let resolved = resolve_default_session_model(Some("gpt-5.4/high"), Some(&catalog), None)
            .expect("preferred fixture model should resolve");

        assert_eq!(
            resolved,
            DefaultSessionModelTarget {
                model_id: "gpt-5.4".to_string(),
                reasoning_effort: Some("high".to_string()),
            }
        );
    }

    #[test]
    fn falls_back_to_provider_status_current_model_without_catalog() {
        let status = status("fake");

        let resolved = resolve_default_session_model(None, None, Some(&status))
            .expect("status current model should resolve");

        assert_eq!(
            resolved,
            DefaultSessionModelTarget {
                model_id: "fake-model".to_string(),
                reasoning_effort: None,
            }
        );
    }
}
