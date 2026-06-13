use super::*;
use ctx_core::provider_ids::CODEX_PROVIDER_ID;

pub fn supports_harness_endpoint(provider_id: &str) -> bool {
    normalize_provider_id(provider_id).is_some_and(provider_supports_harness_endpoint)
}

pub fn default_shape_for_provider(provider_id: &str) -> Option<HarnessApiShape> {
    match normalize_provider_id(provider_id) {
        Some(PROVIDER_CODEX) => Some(HarnessApiShape::OpenaiResponses),
        Some(PROVIDER_CLAUDE) => Some(HarnessApiShape::AnthropicMessages),
        Some(PROVIDER_GEMINI) => Some(HarnessApiShape::OpenaiResponses),
        Some(PROVIDER_KIMI) => Some(HarnessApiShape::OpenaiResponses),
        Some(PROVIDER_QWEN) => Some(HarnessApiShape::OpenaiResponses),
        Some(PROVIDER_OPENCODE) => Some(HarnessApiShape::OpenaiResponses),
        Some(PROVIDER_MISTRAL) => Some(HarnessApiShape::OpenaiResponses),
        Some(PROVIDER_GOOSE) => Some(HarnessApiShape::OpenaiResponses),
        Some(PROVIDER_DROID) => Some(HarnessApiShape::OpenaiResponses),
        Some(PROVIDER_OPENHANDS) => Some(HarnessApiShape::OpenaiResponses),
        Some(PROVIDER_COPILOT) => Some(HarnessApiShape::OpenaiResponses),
        Some(PROVIDER_PI) => Some(HarnessApiShape::OpenaiResponses),
        Some(PROVIDER_CLINE) => Some(HarnessApiShape::OpenaiResponses),
        _ => None,
    }
}

pub fn ensure_shape_compatible(provider_id: &str, shape: HarnessApiShape) -> Result<()> {
    let canonical = normalize_provider_id(provider_id).ok_or_else(|| {
        anyhow::anyhow!("provider does not support harness endpoints: {provider_id}")
    })?;
    if !provider_supports_harness_endpoint(canonical) {
        anyhow::bail!("provider does not support harness endpoints: {provider_id}");
    }
    match canonical {
        PROVIDER_CODEX => {
            if shape != HarnessApiShape::OpenaiResponses {
                anyhow::bail!(
                    "codex requires api_shape=openai_responses; found {}",
                    shape.as_str()
                );
            }
        }
        PROVIDER_CLAUDE => {
            if shape != HarnessApiShape::AnthropicMessages {
                anyhow::bail!(
                    "claude-crp requires api_shape=anthropic_messages; found {}",
                    shape.as_str()
                );
            }
        }
        PROVIDER_GEMINI => {
            if shape != HarnessApiShape::OpenaiResponses {
                anyhow::bail!(
                    "gemini requires api_shape=openai_responses; found {}",
                    shape.as_str()
                );
            }
        }
        PROVIDER_KIMI => {
            if shape != HarnessApiShape::OpenaiResponses {
                anyhow::bail!(
                    "kimi requires api_shape=openai_responses; found {}",
                    shape.as_str()
                );
            }
        }
        PROVIDER_QWEN | PROVIDER_OPENCODE | PROVIDER_MISTRAL | PROVIDER_GOOSE | PROVIDER_DROID
        | PROVIDER_OPENHANDS | PROVIDER_COPILOT | PROVIDER_PI | PROVIDER_CLINE => {
            if shape != HarnessApiShape::OpenaiResponses {
                anyhow::bail!(
                    "{} requires api_shape=openai_responses; found {}",
                    provider_id,
                    shape.as_str()
                );
            }
        }
        _ => anyhow::bail!("provider does not support harness endpoints: {provider_id}"),
    }
    Ok(())
}

pub(super) fn provider_requires_verified_endpoint_for_run(canonical: &str) -> bool {
    matches!(canonical, PROVIDER_CODEX | PROVIDER_CLAUDE)
}

pub(super) fn normalize_provider_id(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        CODEX_PROVIDER_ID => Some(PROVIDER_CODEX),
        PROVIDER_CLAUDE => Some(PROVIDER_CLAUDE),
        PROVIDER_GEMINI => Some(PROVIDER_GEMINI),
        PROVIDER_KIMI => Some(PROVIDER_KIMI),
        PROVIDER_QWEN => Some(PROVIDER_QWEN),
        PROVIDER_OPENCODE => Some(PROVIDER_OPENCODE),
        PROVIDER_MISTRAL => Some(PROVIDER_MISTRAL),
        PROVIDER_GOOSE => Some(PROVIDER_GOOSE),
        PROVIDER_AMP => Some(PROVIDER_AMP),
        PROVIDER_DROID => Some(PROVIDER_DROID),
        PROVIDER_OPENHANDS => Some(PROVIDER_OPENHANDS),
        PROVIDER_COPILOT => Some(PROVIDER_COPILOT),
        PROVIDER_AUGGIE => Some(PROVIDER_AUGGIE),
        PROVIDER_PI => Some(PROVIDER_PI),
        PROVIDER_CURSOR => Some(PROVIDER_CURSOR),
        PROVIDER_CLINE => Some(PROVIDER_CLINE),
        _ => None,
    }
}

pub(super) fn normalize_base_url_for_provider(
    provider_id: &str,
    raw: Option<&str>,
) -> Result<String> {
    match raw {
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                if provider_requires_endpoint_base_url(provider_id) {
                    anyhow::bail!("base_url is required");
                }
                Ok(String::new())
            } else if provider_id == PROVIDER_GEMINI {
                anyhow::bail!(
                    "gemini does not support custom endpoint base_url; use Gemini OAuth or Gemini API key auth"
                );
            } else if provider_id == PROVIDER_CLAUDE {
                normalize_claude_anthropic_base_url(trimmed)
            } else {
                normalize_base_url(trimmed)
            }
        }
        None => {
            if provider_requires_endpoint_base_url(provider_id) {
                anyhow::bail!("base_url is required");
            }
            Ok(String::new())
        }
    }
}

pub(super) fn normalize_auth_type_for_provider(
    provider_id: &str,
    raw: Option<&str>,
) -> Result<String> {
    match provider_id {
        PROVIDER_CODEX => Ok(CODEX_AUTH_TYPE_BEARER.to_string()),
        PROVIDER_CLAUDE => Ok(CLAUDE_AUTH_TYPE_API_KEY.to_string()),
        PROVIDER_GEMINI => {
            let normalized = raw
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(GEMINI_AUTH_TYPE_GEMINI_API_KEY)
                .to_ascii_lowercase();
            match normalized.as_str() {
                GEMINI_AUTH_TYPE_GEMINI_API_KEY | GEMINI_AUTH_TYPE_VERTEX_AI => Ok(normalized),
                _ => anyhow::bail!(
                    "auth_type '{normalized}' is not supported for gemini (expected '{GEMINI_AUTH_TYPE_GEMINI_API_KEY}' or '{GEMINI_AUTH_TYPE_VERTEX_AI}')"
                ),
            }
        }
        _ => Ok(CODEX_AUTH_TYPE_BEARER.to_string()),
    }
}

pub(super) fn endpoint_base_url_or_err(endpoint: &HarnessEndpointRecordInternal) -> Result<String> {
    let trimmed = endpoint.base_url.trim();
    if trimmed.is_empty() {
        anyhow::bail!(
            "selected endpoint '{}' for {} is missing base_url",
            endpoint.name,
            endpoint.provider_id
        );
    }
    Ok(trimmed.to_string())
}

pub(super) fn normalize_name(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("endpoint name is required");
    }
    Ok(trimmed.to_string())
}

pub(super) fn normalize_manual_model_ids(input: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for raw in input {
        let normalized = raw.trim();
        if normalized.is_empty() {
            continue;
        }
        if seen.insert(normalized.to_string()) {
            out.push(normalized.to_string());
        }
    }
    out
}

pub(super) fn ensure_safe_endpoint_id(endpoint_id: &str) -> Result<()> {
    if endpoint_id.is_empty() {
        anyhow::bail!("endpoint_id is required");
    }
    if endpoint_id.len() > 128 {
        anyhow::bail!("endpoint_id must be 128 characters or fewer");
    }
    if !endpoint_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        anyhow::bail!("endpoint_id may only contain ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

pub(super) fn normalize_endpoint_id(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    ensure_safe_endpoint_id(trimmed)?;
    Ok(trimmed.to_string())
}

pub(super) fn provider_supports_harness_endpoint(canonical_provider_id: &str) -> bool {
    matches!(
        canonical_provider_id,
        PROVIDER_CODEX
            | PROVIDER_CLAUDE
            | PROVIDER_GEMINI
            | PROVIDER_KIMI
            | PROVIDER_QWEN
            | PROVIDER_OPENCODE
            | PROVIDER_MISTRAL
            | PROVIDER_GOOSE
            | PROVIDER_DROID
            | PROVIDER_OPENHANDS
            | PROVIDER_COPILOT
            | PROVIDER_PI
            | PROVIDER_CLINE
    )
}

fn normalize_base_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("base_url is required");
    }
    let parsed = Url::parse(trimmed).context("base_url must be a valid URL")?;
    let scheme = parsed.scheme();
    if scheme != "https" && scheme != "http" {
        anyhow::bail!("base_url must use http:// or https://");
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

fn normalize_claude_anthropic_base_url(raw: &str) -> Result<String> {
    let normalized = normalize_base_url(raw)?;
    let mut parsed = Url::parse(&normalized).context("base_url must be a valid URL")?;
    let current_path = parsed.path().to_string();
    let lowered = current_path.to_ascii_lowercase();

    let suffixes = ["/v1/messages/count_tokens", "/v1/messages", "/v1"];
    let mut stripped_path: Option<String> = None;
    for suffix in suffixes {
        if lowered.ends_with(suffix) {
            let keep_len = current_path.len().saturating_sub(suffix.len());
            let prefix = current_path.get(..keep_len).unwrap_or_default();
            let trimmed = prefix.trim_end_matches('/');
            stripped_path = Some(if trimmed.is_empty() {
                "/".to_string()
            } else {
                trimmed.to_string()
            });
            break;
        }
    }

    if let Some(path) = stripped_path {
        parsed.set_path(&path);
    }

    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

fn provider_requires_endpoint_base_url(provider_id: &str) -> bool {
    matches!(
        provider_id,
        PROVIDER_CODEX
            | PROVIDER_CLAUDE
            | PROVIDER_KIMI
            | PROVIDER_QWEN
            | PROVIDER_OPENCODE
            | PROVIDER_MISTRAL
            | PROVIDER_GOOSE
            | PROVIDER_DROID
            | PROVIDER_OPENHANDS
            | PROVIDER_CLINE
    )
}
