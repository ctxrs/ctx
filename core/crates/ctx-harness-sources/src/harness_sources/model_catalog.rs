use super::*;

pub fn endpoint_model_catalog_ttl() -> Duration {
    ENDPOINT_MODEL_CATALOG_TTL
}

pub fn endpoint_model_catalog_is_stale(
    endpoint: &HarnessEndpointRecord,
    now: DateTime<Utc>,
) -> bool {
    if endpoint.model_catalog_status == EndpointModelCatalogStatus::Unknown {
        return true;
    }
    if endpoint.model_catalog_status == EndpointModelCatalogStatus::Error {
        return true;
    }
    let Some(fetched_at) = endpoint.model_catalog_fetched_at else {
        return endpoint.model_catalog_status != EndpointModelCatalogStatus::ManualOnly;
    };
    let age = now.signed_duration_since(fetched_at);
    age > chrono::Duration::from_std(ENDPOINT_MODEL_CATALOG_TTL)
        .unwrap_or_else(|_| chrono::Duration::hours(24))
}

pub(super) fn merge_endpoint_model_records(
    discovered: &[EndpointModelRecord],
    manual_model_ids: &[String],
) -> Vec<EndpointModelRecord> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut merged = Vec::new();
    for model in discovered {
        let id = model.id.trim();
        if id.is_empty() {
            continue;
        }
        if seen.insert(id.to_string()) {
            merged.push(EndpointModelRecord {
                id: id.to_string(),
                name: model.name.as_ref().map(|value| value.trim().to_string()),
            });
        }
    }
    for manual in manual_model_ids {
        let id = manual.trim();
        if id.is_empty() {
            continue;
        }
        if seen.insert(id.to_string()) {
            merged.push(EndpointModelRecord {
                id: id.to_string(),
                name: None,
            });
        }
    }
    merged
}

pub(super) fn infer_endpoint_model_provider_namespace(base_url: &str) -> Option<String> {
    let parsed = Url::parse(base_url).ok()?;
    let host = parsed.host_str()?.trim().to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }

    let mut labels = host.split('.').filter(|label| !label.is_empty());
    let candidate = labels
        .find(|label| !GENERIC_ENDPOINT_NAMESPACE_LABELS.contains(label))
        .or_else(|| host.split('.').find(|label| !label.is_empty()))?;

    let mut normalized = String::new();
    for ch in candidate.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' {
            normalized.push('_');
        }
    }
    let normalized = normalized.trim_matches('_').to_string();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

pub(super) fn normalize_namespaced_model_override(
    model: &str,
    endpoint_namespace: Option<&str>,
) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let Some(namespace) = endpoint_namespace
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return trimmed.to_string();
    };

    format!("{namespace}/{trimmed}")
}

pub(super) fn truncate_discovery_error(raw: &str) -> String {
    const MAX: usize = 280;
    let collapsed = raw.replace(['\n', '\r'], " ").trim().to_string();
    if collapsed.len() <= MAX {
        return collapsed;
    }
    let mut end = MAX;
    while end > 0 && !collapsed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &collapsed[..end])
}

pub(super) fn supports_model_discovery(endpoint: &HarnessEndpointRecordInternal) -> bool {
    endpoint.api_shape == HarnessApiShape::OpenaiResponses && !endpoint.base_url.trim().is_empty()
}

pub(super) async fn discover_openai_models(
    base_url: &str,
    auth_type: &str,
    api_key: &str,
) -> Result<Vec<EndpointModelRecord>> {
    let client = reqwest::Client::builder()
        .timeout(ENDPOINT_MODEL_DISCOVERY_TIMEOUT)
        .build()?;
    let url = endpoint_models_url(base_url)?;
    let mut request = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json");
    match auth_type {
        GEMINI_AUTH_TYPE_GEMINI_API_KEY => {
            request = request.header("x-goog-api-key", api_key);
        }
        _ => {
            request = request.bearer_auth(api_key);
        }
    }
    let response = request.send().await?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "model discovery failed with status {}: {}",
            status,
            truncate_discovery_error(&body)
        );
    }
    let payload: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| "model discovery response was not valid JSON")?;
    parse_openai_models_payload(&payload)
}

fn endpoint_models_url(base_url: &str) -> Result<String> {
    let mut normalized =
        validation::normalize_base_url_for_provider(PROVIDER_CODEX, Some(base_url))?;
    normalized.push_str("/models");
    Ok(normalized)
}

pub(super) fn parse_openai_models_payload(
    payload: &serde_json::Value,
) -> Result<Vec<EndpointModelRecord>> {
    let data = payload
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("models payload missing array field 'data'"))?;
    let mut seen: HashSet<String> = HashSet::new();
    let mut models = Vec::new();
    for entry in data {
        let Some(rec) = entry.as_object() else {
            continue;
        };
        let id = rec
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        if id.is_empty() {
            continue;
        }
        if !seen.insert(id.to_string()) {
            continue;
        }
        let name = rec
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        models.push(EndpointModelRecord {
            id: id.to_string(),
            name,
        });
    }
    if models.is_empty() {
        anyhow::bail!("models payload did not include any model ids");
    }
    Ok(models)
}
