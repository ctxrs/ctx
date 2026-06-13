use serde_json::json;

use super::copilot_models_value_for_version;

#[derive(Clone, Copy)]
struct PinnedFlatModel {
    id: &'static str,
    display_name: &'static str,
}

#[derive(Clone, Copy)]
struct PinnedReasoningModel {
    id: &'static str,
    display_name: &'static str,
    default_effort: &'static str,
    efforts: &'static [&'static str],
}

const CODEX_PINNED_SUBSCRIPTION_MODELS: [PinnedReasoningModel; 8] = [
    PinnedReasoningModel {
        id: "gpt-5.4",
        display_name: "gpt-5.4",
        default_effort: "medium",
        efforts: &["low", "medium", "high", "xhigh"],
    },
    PinnedReasoningModel {
        id: "gpt-5.5",
        display_name: "gpt-5.5",
        default_effort: "medium",
        efforts: &["low", "medium", "high", "xhigh"],
    },
    PinnedReasoningModel {
        id: "gpt-5.4-mini",
        display_name: "gpt-5.4-mini",
        default_effort: "medium",
        efforts: &["low", "medium", "high", "xhigh"],
    },
    PinnedReasoningModel {
        id: "gpt-5.3-codex",
        display_name: "gpt-5.3-codex",
        default_effort: "medium",
        efforts: &["low", "medium", "high", "xhigh"],
    },
    PinnedReasoningModel {
        id: "gpt-5.2-codex",
        display_name: "gpt-5.2-codex",
        default_effort: "medium",
        efforts: &["low", "medium", "high", "xhigh"],
    },
    PinnedReasoningModel {
        id: "gpt-5.1-codex-max",
        display_name: "gpt-5.1-codex-max",
        default_effort: "medium",
        efforts: &["low", "medium", "high", "xhigh"],
    },
    PinnedReasoningModel {
        id: "gpt-5.2",
        display_name: "gpt-5.2",
        default_effort: "medium",
        efforts: &["low", "medium", "high", "xhigh"],
    },
    PinnedReasoningModel {
        id: "gpt-5.1-codex-mini",
        display_name: "gpt-5.1-codex-mini",
        default_effort: "medium",
        efforts: &["medium", "high"],
    },
];

const CLAUDE_PINNED_SUBSCRIPTION_MODELS: [PinnedReasoningModel; 3] = [
    PinnedReasoningModel {
        id: "default",
        display_name: "Default",
        default_effort: "medium",
        efforts: &["low", "medium", "high"],
    },
    PinnedReasoningModel {
        id: "sonnet",
        display_name: "Sonnet",
        default_effort: "medium",
        efforts: &["low", "medium", "high"],
    },
    PinnedReasoningModel {
        id: "opus",
        display_name: "Opus",
        default_effort: "medium",
        efforts: &["low", "medium", "high"],
    },
];

const GEMINI_CATALOG_VERSION_0_33_1: &str = "0.33.1";
const GEMINI_CATALOG_VERSION_0_38_2: &str = "0.38.2";
const GEMINI_CATALOG_VERSION_0_39_0: &str = "0.39.0";

const GEMINI_PINNED_SUBSCRIPTION_MODELS_0_33_1: [PinnedFlatModel; 7] = [
    PinnedFlatModel {
        id: "auto-gemini-3",
        display_name: "Auto (Gemini 3)",
    },
    PinnedFlatModel {
        id: "auto-gemini-2.5",
        display_name: "Auto (Gemini 2.5)",
    },
    PinnedFlatModel {
        id: "gemini-3-pro-preview",
        display_name: "Gemini 3 Pro Preview",
    },
    PinnedFlatModel {
        id: "gemini-3-flash-preview",
        display_name: "Gemini 3 Flash Preview",
    },
    PinnedFlatModel {
        id: "gemini-2.5-pro",
        display_name: "Gemini 2.5 Pro",
    },
    PinnedFlatModel {
        id: "gemini-2.5-flash",
        display_name: "Gemini 2.5 Flash",
    },
    PinnedFlatModel {
        id: "gemini-2.5-flash-lite",
        display_name: "Gemini 2.5 Flash Lite",
    },
];

fn effort_label(effort: &str) -> &'static str {
    match effort {
        "xhigh" => "Extra High",
        "high" => "High",
        "medium" => "Medium",
        "low" => "Low",
        "minimal" => "Minimal",
        "none" => "None",
        _ => "Unknown",
    }
}

fn pinned_reasoning_models_value(
    catalog_source: &'static str,
    current_model_id: String,
    models: &[PinnedReasoningModel],
) -> serde_json::Value {
    json!({
        "catalog_source": catalog_source,
        "current_model_id": current_model_id,
        "models": models.iter().flat_map(|model| {
            model.efforts.iter().map(|effort| json!({
                "id": format!("{}/{}", model.id, effort),
                "name": format!("{} ({})", model.display_name, effort_label(effort)),
            })).collect::<Vec<_>>()
        }).collect::<Vec<_>>(),
        "meta": {
            "source_kind": "subscription",
            "catalog_source": catalog_source,
            "refresh_pending": true,
        },
    })
}

fn pinned_flat_models_value(
    catalog_source: &'static str,
    catalog_version: &str,
    current_model_id: &'static str,
    models: &[PinnedFlatModel],
) -> serde_json::Value {
    json!({
        "catalog_source": catalog_source,
        "catalog_version": catalog_version,
        "current_model_id": current_model_id,
        "models": models.iter().map(|model| json!({
            "id": model.id,
            "name": model.display_name,
        })).collect::<Vec<_>>(),
        "meta": {
            "source_kind": "subscription",
            "catalog_source": catalog_source,
            "catalog_version": catalog_version,
            "refresh_pending": true,
        },
    })
}

fn normalize_cli_version(version: &str) -> Option<String> {
    let trimmed = version.trim().trim_start_matches('v').trim_end_matches('.');
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

fn gemini_models_value_for_version(version: &str) -> Option<serde_json::Value> {
    let normalized_version = normalize_cli_version(version)?;
    match normalized_version.as_str() {
        GEMINI_CATALOG_VERSION_0_33_1
        | GEMINI_CATALOG_VERSION_0_38_2
        | GEMINI_CATALOG_VERSION_0_39_0 => Some(pinned_flat_models_value(
            "gemini_cli_version_pinned",
            normalized_version.as_str(),
            GEMINI_PINNED_SUBSCRIPTION_MODELS_0_33_1[0].id,
            &GEMINI_PINNED_SUBSCRIPTION_MODELS_0_33_1,
        )),
        _ => None,
    }
}

pub fn pinned_subscription_models_value(
    provider_id: &str,
    provider_version: Option<&str>,
) -> Option<serde_json::Value> {
    match provider_id {
        "codex" => Some(pinned_reasoning_models_value(
            "codex_bundle_pinned",
            format!(
                "{}/{}",
                CODEX_PINNED_SUBSCRIPTION_MODELS[0].id,
                CODEX_PINNED_SUBSCRIPTION_MODELS[0].default_effort
            ),
            &CODEX_PINNED_SUBSCRIPTION_MODELS,
        )),
        "claude-crp" => Some(pinned_reasoning_models_value(
            "claude_subscription_pinned",
            format!(
                "{}/{}",
                CLAUDE_PINNED_SUBSCRIPTION_MODELS[0].id,
                CLAUDE_PINNED_SUBSCRIPTION_MODELS[0].default_effort
            ),
            &CLAUDE_PINNED_SUBSCRIPTION_MODELS,
        )),
        "gemini" => provider_version.and_then(gemini_models_value_for_version),
        "copilot" => provider_version.and_then(copilot_models_value_for_version),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::pinned_subscription_models_value;

    #[test]
    fn codex_pinned_subscription_models_include_reasoning_variants() {
        let payload =
            pinned_subscription_models_value("codex", None).expect("codex pinned payload");
        assert_eq!(
            payload
                .get("current_model_id")
                .and_then(serde_json::Value::as_str),
            Some("gpt-5.4/medium")
        );
        assert_eq!(
            payload
                .pointer("/meta/refresh_pending")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            payload
                .pointer("/models/0/id")
                .and_then(serde_json::Value::as_str),
            Some("gpt-5.4/low")
        );
        assert!(
            payload
                .get("models")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|models| models
                    .iter()
                    .any(|model| model.get("id").and_then(serde_json::Value::as_str)
                        == Some("gpt-5.4-mini/medium"))),
            "codex pinned catalog should include GPT-5.4 Mini"
        );
    }

    #[test]
    fn claude_pinned_subscription_models_include_effort_variants() {
        let payload =
            pinned_subscription_models_value("claude-crp", None).expect("claude pinned payload");
        assert_eq!(
            payload
                .get("current_model_id")
                .and_then(serde_json::Value::as_str),
            Some("default/medium")
        );
        assert_eq!(
            payload
                .pointer("/models/5/id")
                .and_then(serde_json::Value::as_str),
            Some("sonnet/high")
        );
    }

    #[test]
    fn gemini_pinned_subscription_models_match_current_managed_catalog() {
        let payload = pinned_subscription_models_value("gemini", Some("0.33.1"))
            .expect("gemini pinned payload");
        assert_eq!(
            payload
                .get("current_model_id")
                .and_then(serde_json::Value::as_str),
            Some("auto-gemini-3")
        );
        assert_eq!(
            payload
                .get("catalog_version")
                .and_then(serde_json::Value::as_str),
            Some("0.33.1")
        );
        assert_eq!(
            payload
                .pointer("/meta/refresh_pending")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            payload
                .pointer("/models/6/id")
                .and_then(serde_json::Value::as_str),
            Some("gemini-2.5-flash-lite")
        );
    }

    #[test]
    fn gemini_pinned_catalog_supports_the_managed_matrix_release() {
        let matrix: serde_json::Value =
            serde_json::from_str(crate::PROVIDER_MATRIX_JSON).expect("provider matrix");
        let managed_release = matrix
            .get("providers")
            .and_then(serde_json::Value::as_array)
            .and_then(|providers| {
                providers.iter().find(|provider| {
                    provider.get("id").and_then(serde_json::Value::as_str) == Some("gemini")
                })
            })
            .and_then(|provider| provider.get("releases"))
            .and_then(serde_json::Value::as_array)
            .and_then(|releases| releases.first())
            .and_then(|release| release.get("version"))
            .and_then(serde_json::Value::as_str)
            .expect("managed gemini release");

        assert!(
            pinned_subscription_models_value("gemini", Some(managed_release)).is_some(),
            "missing pinned gemini catalog for managed release {managed_release}"
        );
    }
}
