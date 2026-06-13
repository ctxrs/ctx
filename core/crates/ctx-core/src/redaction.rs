const REDACTED: &str = "[REDACTED]";

const SENSITIVE_KEYS: &[&str] = &[
    "access_token",
    "accessToken",
    "anthropic_api_key",
    "anthropic_auth_token",
    "api_key",
    "apiKey",
    "apikey",
    "augment_api_token",
    "augment_session_auth",
    "authorization",
    "auth_token",
    "authToken",
    "bearer_token",
    "claude_code_oauth_token",
    "client_secret",
    "codex_auth_token",
    "copilot_github_token",
    "cursor_api_key",
    "cursor_auth_token",
    "ctx_auth_token",
    "ctxAuthToken",
    "ctx_mcp_token",
    "ctxMcpToken",
    "daemon_private_key",
    "gemini_api_key",
    "gh_token",
    "github_token",
    "google_api_key",
    "id_token",
    "openAiNativeApiKey",
    "kimi_api_key",
    "mistral_api_key",
    "oauth_creds",
    "openai_api_key",
    "openrouter_api_key",
    "openRouterApiKey",
    "private_key",
    "qwen_api_key",
    "refresh_token",
    "refreshToken",
    "session_token",
    "service_account_json",
    "token",
    "tunnel_secret",
];

const SENSITIVE_QUERY_KEYS: &[&str] = &[
    "access_token",
    "api_key",
    "apikey",
    "auth_token",
    "client_secret",
    "code",
    "daemon_private_key",
    "id_token",
    "key",
    "refresh_token",
    "token",
    "tunnel_secret",
];

pub fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();

    normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("authorization")
        || normalized.contains("credential")
        || normalized.contains("privatekey")
        || normalized.contains("apikey")
}

pub fn redact_sensitive(input: &str) -> String {
    let mut out = input.to_string();

    for marker in [
        "Bearer ",
        "Authorization: Bearer ",
        "authorization: Bearer ",
        "Authorization=Bearer ",
        "authorization=Bearer ",
        "Authorization: Basic ",
        "authorization: Basic ",
        "Authorization=Basic ",
        "authorization=Basic ",
    ] {
        out = redact_after_marker_ci(out, marker, &value_terminators());
    }

    for key in SENSITIVE_KEYS {
        out = redact_key_like_value(out, key);
    }

    for key in SENSITIVE_QUERY_KEYS {
        out = redact_after_marker_ci(out, &format!("?{key}="), &url_terminators());
        out = redact_after_marker_ci(out, &format!("&{key}="), &url_terminators());
        out = redact_after_marker_ci(out, &format!(";{key}="), &url_terminators());
    }

    out
}

pub fn redact_json_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (key, value) in map {
                if is_sensitive_key(&key) {
                    out.insert(key, serde_json::Value::String(REDACTED.to_string()));
                } else {
                    out.insert(key, redact_json_value(value));
                }
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(redact_json_value).collect())
        }
        serde_json::Value::String(value) => serde_json::Value::String(redact_sensitive(&value)),
        other => other,
    }
}

fn redact_key_like_value(mut value: String, key: &str) -> String {
    for marker in [
        format!("{key}="),
        format!("{key}:"),
        format!("{key}: "),
        format!("\"{key}\":\""),
        format!("\"{key}\": \""),
        format!("'{key}':'"),
        format!("'{key}': '"),
    ] {
        value = redact_after_marker_ci(value, &marker, &value_terminators());
    }
    value
}

fn value_terminators() -> [char; 9] {
    [' ', '\t', '\n', '\r', '"', '\'', '&', ',', '}']
}

fn url_terminators() -> [char; 7] {
    [' ', '\t', '\n', '\r', '&', '#', '"']
}

fn redact_after_marker_ci(mut value: String, marker: &str, terminators: &[char]) -> String {
    let marker_lower = marker.to_ascii_lowercase();
    let redacted_lower = REDACTED.to_ascii_lowercase();
    let mut lower = value.to_ascii_lowercase();
    let mut search_from = 0usize;

    while let Some(rel) = lower[search_from..].find(&marker_lower) {
        let marker_start = search_from + rel;
        let start = marker_start + marker.len();
        if start >= value.len() {
            break;
        }
        if lower[start..].starts_with(&redacted_lower) {
            search_from = start + REDACTED.len();
            continue;
        }

        let mut value_start = start;
        while let Some(ch) = value[value_start..].chars().next() {
            if ch == '"' || ch == '\'' || ch.is_whitespace() {
                value_start += ch.len_utf8();
                continue;
            }
            break;
        }

        let mut end = value.len();
        for (offset, ch) in value[value_start..].char_indices() {
            if terminators.contains(&ch) || ch == ']' || ch == '<' {
                end = value_start + offset;
                break;
            }
        }

        if end <= value_start {
            search_from = value_start.saturating_add(1);
            continue;
        }

        value.replace_range(value_start..end, REDACTED);
        lower.replace_range(value_start..end, &redacted_lower);
        search_from = value_start + REDACTED.len();
    }

    value
}

#[cfg(test)]
mod tests {
    use super::{is_sensitive_key, redact_json_value, redact_sensitive};

    #[test]
    fn redact_sensitive_covers_provider_envs_and_token_shapes() {
        let input = concat!(
            "OPENAI_API_KEY=sk-openai ",
            "ANTHROPIC_API_KEY=sk-ant ",
            "GOOGLE_API_KEY=google ",
            "GEMINI_API_KEY=gemini ",
            "OPENROUTER_API_KEY=sk-or ",
            "COPILOT_GITHUB_TOKEN=ghp_secret ",
            "TOKEN=generic-token ",
            "token=lower-token ",
            "CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN=shutdown-token ",
            "\"access_token\":\"access-1\",",
            "\"refresh_token\": \"refresh-1\",",
            "\"tunnel_secret\":\"tun-1\",",
            "\"daemon_private_key\":\"priv-1\""
        );

        let redacted = redact_sensitive(input);

        for secret in [
            "sk-openai",
            "sk-ant",
            "google",
            "gemini",
            "sk-or",
            "ghp_secret",
            "generic-token",
            "lower-token",
            "shutdown-token",
            "access-1",
            "refresh-1",
            "tun-1",
            "priv-1",
        ] {
            assert!(!redacted.contains(secret), "{secret} leaked in {redacted}");
        }
    }

    #[test]
    fn redact_sensitive_covers_authorization_headers_and_url_query_secrets() {
        let input = concat!(
            "Authorization: Bearer header-secret\n",
            "curl https://example.test/path?api_key=query-key&ok=1",
            " https://example.test/callback?code=oauth-code#frag",
            " authorization=Bearer form-secret"
        );

        let redacted = redact_sensitive(input);

        for secret in ["header-secret", "query-key", "oauth-code", "form-secret"] {
            assert!(!redacted.contains(secret), "{secret} leaked in {redacted}");
        }
        assert!(redacted.contains("ok=1"));
    }

    #[test]
    fn sensitive_key_detection_covers_common_secret_names() {
        for key in [
            "api_key",
            "openAiApiKey",
            "refreshToken",
            "daemon_private_key",
            "Authorization",
            "serviceCredential",
        ] {
            assert!(is_sensitive_key(key), "{key} should be sensitive");
        }
        assert!(!is_sensitive_key("provider_id"));
    }

    #[test]
    fn redact_json_value_redacts_sensitive_keys_and_nested_strings() {
        let value = serde_json::json!({
            "provider_id": "codex",
            "OPENAI_API_KEY": "sk-json",
            "nested": {
                "url": "https://example.test/callback?code=oauth-code",
                "daemon_private_key": "daemon-key"
            }
        });

        let redacted = redact_json_value(value).to_string();

        for secret in ["sk-json", "oauth-code", "daemon-key"] {
            assert!(!redacted.contains(secret), "{secret} leaked in {redacted}");
        }
        assert!(redacted.contains("provider_id"));
    }
}
