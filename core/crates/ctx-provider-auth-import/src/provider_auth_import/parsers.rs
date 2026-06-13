use super::*;

pub(super) fn parse_env_file(raw: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.splitn(2, '=');
        let key = parts.next().unwrap_or("").trim();
        let value = parts.next().unwrap_or("").trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim_matches('"').trim_matches('\'');
        out.insert(key.to_string(), value.to_string());
    }
    out
}

pub(super) fn trim_to_option(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(super) fn find_json_string_by_keys(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            for expected in keys {
                for (actual_key, actual_value) in map {
                    if !json_key_matches(actual_key, expected) {
                        continue;
                    }
                    if let Some(value) = actual_value.as_str().and_then(trim_to_option) {
                        return Some(value);
                    }
                }
            }
            for nested in map.values() {
                if let Some(value) = find_json_string_by_keys(nested, keys) {
                    return Some(value);
                }
            }
            None
        }
        serde_json::Value::Array(items) => items
            .iter()
            .find_map(|item| find_json_string_by_keys(item, keys)),
        _ => None,
    }
}

pub(super) fn env_value_case_insensitive(
    env_map: &BTreeMap<String, String>,
    keys: &[&str],
) -> Option<String> {
    for key in keys {
        if let Some(value) = env_map
            .iter()
            .find_map(|(actual, value)| actual.eq_ignore_ascii_case(key).then_some(value))
            .and_then(|value| trim_to_option(value))
        {
            return Some(value);
        }
    }
    None
}

pub(super) fn gemini_env_uses_vertex_ai(env_map: &BTreeMap<String, String>) -> bool {
    env_value_case_insensitive(env_map, &["GOOGLE_GENAI_USE_VERTEXAI"]).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    }) || env_value_case_insensitive(
        env_map,
        &[
            "GOOGLE_CLOUD_PROJECT",
            "GOOGLE_CLOUD_LOCATION",
            "GOOGLE_VERTEX_PROJECT",
            "GOOGLE_VERTEX_LOCATION",
        ],
    )
    .is_some()
}

pub(super) fn parse_endpoint_env_candidate(
    provider_id: &str,
    material: &CandidateMaterial,
    default_keys: &[&str],
) -> Result<(String, Option<String>)> {
    let Some(bytes) = material.secret_bytes.as_ref() else {
        anyhow::bail!("No importable auth material.");
    };
    let env_map = parse_env_file(&String::from_utf8_lossy(bytes));
    let api_key = env_value_case_insensitive(&env_map, default_keys)
        .or_else(|| {
            env_map
                .iter()
                .find(|(key, _)| {
                    key.to_ascii_uppercase().contains("API_KEY")
                        || key.to_ascii_uppercase().contains("TOKEN")
                })
                .and_then(|(_, value)| trim_to_option(value))
        })
        .ok_or_else(|| anyhow::anyhow!("No API key/token variable found in env file."))?;

    let base_url = material
        .candidate
        .endpoint
        .as_deref()
        .and_then(trim_to_option)
        .or_else(|| {
            env_value_case_insensitive(
                &env_map,
                &["OPENAI_BASE_URL", "BASE_URL", "CTX_GATEWAY_BASE_URL"],
            )
        })
        .or_else(|| default_endpoint_base_url_for_provider(provider_id));

    Ok((api_key, base_url))
}

pub(super) fn parse_endpoint_json_candidate(
    provider_id: &str,
    material: &CandidateMaterial,
) -> Result<(String, Option<String>, Option<String>)> {
    let Some(bytes) = material.secret_bytes.as_ref() else {
        anyhow::bail!("No importable auth material.");
    };
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("Auth file must be valid JSON.")?;
    let api_key = find_json_string_by_keys(
        &value,
        &[
            "api_key",
            "apiKey",
            "token",
            "auth_token",
            "authToken",
            "openai_api_key",
            "openrouter_api_key",
            "anthropic_api_key",
        ],
    )
    .ok_or_else(|| anyhow::anyhow!("No API key/token field found in auth file."))?;
    let base_url = find_json_string_by_keys(
        &value,
        &[
            "base_url",
            "baseURL",
            "url",
            "endpoint",
            "openai_base_url",
            "openrouter_base_url",
        ],
    )
    .or_else(|| default_endpoint_base_url_for_provider(provider_id));
    let model_override = find_json_string_by_keys(&value, &["model", "model_name", "openai_model"]);
    Ok((api_key, base_url, model_override))
}

fn normalize_json_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
}

fn json_key_matches(actual: &str, expected: &str) -> bool {
    normalize_json_key(actual) == normalize_json_key(expected)
}

fn default_endpoint_base_url_for_provider(provider_id: &str) -> Option<String> {
    match provider_id {
        "qwen" | "opencode" => Some(DEFAULT_OPENROUTER_BASE_URL.to_string()),
        _ => None,
    }
}
