use serde_json::Value;

pub fn preferred_model_id_from_available_models(
    preferred_model_id: Option<String>,
    models: Option<&Value>,
) -> Option<String> {
    let preferred_model_id = preferred_model_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)?;
    let models = models?;
    if model_payload_contains_id(models, &preferred_model_id) {
        Some(preferred_model_id)
    } else {
        None
    }
}

pub fn inject_preferred_model_id(value: &mut Value, preferred_model_id: Option<String>) {
    let resolved =
        preferred_model_id_from_available_models(preferred_model_id, value.get("models"));
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    if let Some(preferred_model_id) = resolved {
        obj.insert(
            "preferred_model_id".to_string(),
            serde_json::json!(preferred_model_id),
        );
    } else {
        obj.remove("preferred_model_id");
    }
}

fn model_payload_contains_id(models: &Value, target_id: &str) -> bool {
    let target_id = target_id.trim();
    if target_id.is_empty() {
        return false;
    }
    if models
        .get("current_model_id")
        .or_else(|| models.get("currentModelId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| value == target_id)
    {
        return true;
    }
    extract_model_entries(models)
        .into_iter()
        .any(|model_id| model_id == target_id)
}

fn extract_model_entries(models: &Value) -> Vec<String> {
    let Some(entries) = models
        .get("models")
        .or_else(|| models.get("availableModels"))
        .or_else(|| models.get("available_models"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };

    entries
        .iter()
        .filter_map(|entry| {
            entry
                .get("id")
                .or_else(|| entry.get("modelId"))
                .or_else(|| entry.get("model_id"))
                .or_else(|| entry.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{inject_preferred_model_id, preferred_model_id_from_available_models};

    #[test]
    fn omits_preference_when_catalog_does_not_contain_it() {
        let models = serde_json::json!({
            "current_model_id": "gpt-5.4/medium",
            "models": [
                {"id": "gpt-5.4/medium"},
                {"id": "gpt-5.4/high"}
            ]
        });

        assert_eq!(
            preferred_model_id_from_available_models(
                Some("gpt-5.4/xhigh".to_string()),
                Some(&models),
            ),
            None
        );
    }

    #[test]
    fn keeps_preference_when_catalog_contains_it() {
        let models = serde_json::json!({
            "current_model_id": "gpt-5.4/medium",
            "models": [
                {"id": "gpt-5.4/medium"},
                {"id": "gpt-5.4/xhigh"}
            ]
        });

        assert_eq!(
            preferred_model_id_from_available_models(
                Some(" gpt-5.4/xhigh ".to_string()),
                Some(&models),
            ),
            Some("gpt-5.4/xhigh".to_string())
        );
    }

    #[test]
    fn keeps_preference_when_catalog_only_has_camel_case_current_model_id() {
        let models = serde_json::json!({
            "currentModelId": "gpt-5.4/xhigh"
        });

        assert_eq!(
            preferred_model_id_from_available_models(
                Some("gpt-5.4/xhigh".to_string()),
                Some(&models),
            ),
            Some("gpt-5.4/xhigh".to_string())
        );
    }

    #[test]
    fn inject_preferred_model_id_removes_unavailable_preference() {
        let mut value = serde_json::json!({
            "preferred_model_id": "stale",
            "models": {
                "models": [
                    {"id": "gpt-5.4/medium"}
                ]
            }
        });

        inject_preferred_model_id(&mut value, Some("gpt-5.4/xhigh".to_string()));

        assert!(value.get("preferred_model_id").is_none());
    }

    #[test]
    fn inject_preferred_model_id_keeps_available_preference() {
        let mut value = serde_json::json!({
            "models": {
                "models": [
                    {"id": "gpt-5.4/xhigh"}
                ]
            }
        });

        inject_preferred_model_id(&mut value, Some(" gpt-5.4/xhigh ".to_string()));

        assert_eq!(
            value
                .get("preferred_model_id")
                .and_then(serde_json::Value::as_str),
            Some("gpt-5.4/xhigh")
        );
    }
}
