use url::Url;

#[cfg(test)]
use ctx_sandbox_contract::ContainerNetworkMode;

pub const LLM_ALLOWLIST: &[&str] = &[
    "api.anthropic.com",
    "chatgpt.com",
    "auth.openai.com",
    "api.openai.com",
    "api.mistral.ai",
    "api.groq.com",
    "api.cohere.ai",
    "api.together.xyz",
    "api.openrouter.ai",
    "openrouter.ai",
    "generativelanguage.googleapis.com",
    "vertex.googleapis.com",
    "dashscope.aliyuncs.com",
    "api.deepseek.com",
];

pub fn normalize_allowlist_entry(entry: &str) -> Option<String> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(url) = Url::parse(trimmed) {
        if let Some(host) = url.host_str() {
            return Some(host.to_ascii_lowercase());
        }
    }
    let host = trimmed
        .split('/')
        .next()
        .unwrap_or(trimmed)
        .split(':')
        .next()
        .unwrap_or(trimmed)
        .trim();
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
pub fn host_matches(host: &str, entry: &str) -> bool {
    host == entry || host.ends_with(&format!(".{entry}"))
}

#[cfg(test)]
pub fn allowed_host(host: &str, mode: ContainerNetworkMode, allowlist: &[String]) -> bool {
    if matches!(mode, ContainerNetworkMode::All) {
        return true;
    }
    let host = host.to_ascii_lowercase();
    let mut entries = Vec::new();
    if matches!(mode, ContainerNetworkMode::LlmOnly) {
        entries.extend(
            LLM_ALLOWLIST
                .iter()
                .filter_map(|entry| normalize_allowlist_entry(entry)),
        );
    }
    if matches!(mode, ContainerNetworkMode::Allowlist) {
        entries.extend(
            allowlist
                .iter()
                .filter_map(|entry| normalize_allowlist_entry(entry)),
        );
    }
    entries.iter().any(|entry| host_matches(&host, entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_matches_allows_suffix() {
        assert!(host_matches("api.openai.com", "openai.com"));
        assert!(host_matches("openai.com", "openai.com"));
        assert!(!host_matches("evilopenai.com", "openai.com"));
    }

    #[test]
    fn allowlist_enforces_llm_only() {
        assert!(allowed_host(
            "api.openai.com",
            ContainerNetworkMode::LlmOnly,
            &[]
        ));
        assert!(allowed_host(
            "auth.openai.com",
            ContainerNetworkMode::LlmOnly,
            &[]
        ));
        assert!(allowed_host(
            "openrouter.ai",
            ContainerNetworkMode::LlmOnly,
            &[]
        ));
        assert!(!allowed_host(
            "example.com",
            ContainerNetworkMode::LlmOnly,
            &[]
        ));
    }
}
