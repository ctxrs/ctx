use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider_usage;

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ProviderUsageRouteQuery {
    refresh: Option<bool>,
}

impl ProviderUsageRouteQuery {
    pub fn new(refresh: Option<bool>) -> Self {
        Self { refresh }
    }

    pub fn refresh(&self) -> bool {
        self.refresh.unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderUsageRouteSnapshot {
    provider_id: String,
    source: String,
    fetched_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ProviderUsageRouteSnapshot {
    pub fn new(
        provider_id: String,
        source: String,
        fetched_at: DateTime<Utc>,
        payload: Option<Value>,
        error: Option<String>,
    ) -> Self {
        Self {
            provider_id,
            source,
            fetched_at,
            payload,
            error,
        }
    }
}

impl From<provider_usage::ProviderUsageSnapshot> for ProviderUsageRouteSnapshot {
    fn from(snapshot: provider_usage::ProviderUsageSnapshot) -> Self {
        Self::new(
            snapshot.provider_id,
            snapshot.source,
            snapshot.fetched_at,
            snapshot.payload,
            snapshot.error,
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexAccountUsageRouteEntry {
    account_id: Option<String>,
    label: String,
    email: Option<String>,
    plan_type: Option<String>,
    last_used_at: Option<DateTime<Utc>>,
    usage: ProviderUsageRouteSnapshot,
}

impl CodexAccountUsageRouteEntry {
    pub fn new(
        account_id: Option<String>,
        label: String,
        email: Option<String>,
        plan_type: Option<String>,
        last_used_at: Option<DateTime<Utc>>,
        usage: ProviderUsageRouteSnapshot,
    ) -> Self {
        Self {
            account_id,
            label,
            email,
            plan_type,
            last_used_at,
            usage,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexAccountsUsageRouteResponse {
    entries: Vec<CodexAccountUsageRouteEntry>,
}

impl CodexAccountsUsageRouteResponse {
    pub fn new(entries: Vec<CodexAccountUsageRouteEntry>) -> Self {
        Self { entries }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderUsageRouteError {
    message: String,
}

impl ProviderUsageRouteError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ProviderUsageRouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).expect("valid timestamp")
    }

    #[test]
    fn query_defaults_refresh_to_false() {
        assert!(!ProviderUsageRouteQuery::default().refresh());
        assert!(!ProviderUsageRouteQuery::new(Some(false)).refresh());
        assert!(ProviderUsageRouteQuery::new(Some(true)).refresh());
    }

    #[test]
    fn snapshot_skips_absent_payload_and_error() {
        let snapshot = ProviderUsageRouteSnapshot::new(
            "codex".to_string(),
            "oauth".to_string(),
            timestamp(1_700_000_000),
            None,
            None,
        );
        let payload = serde_json::to_value(snapshot).expect("serialize snapshot");

        assert_eq!(payload["provider_id"].as_str(), Some("codex"));
        assert_eq!(payload["source"].as_str(), Some("oauth"));
        assert!(payload.get("payload").is_none());
        assert!(payload.get("error").is_none());
    }

    #[test]
    fn codex_account_entry_preserves_null_optional_metadata() {
        let entry = CodexAccountUsageRouteEntry::new(
            None,
            "Default".to_string(),
            None,
            None,
            None,
            ProviderUsageRouteSnapshot::new(
                "codex".to_string(),
                "error".to_string(),
                timestamp(1_700_000_001),
                None,
                Some("being deleted".to_string()),
            ),
        );
        let payload = serde_json::to_value(entry).expect("serialize entry");

        assert!(payload["account_id"].is_null());
        assert_eq!(payload["label"].as_str(), Some("Default"));
        assert!(payload["email"].is_null());
        assert!(payload["plan_type"].is_null());
        assert!(payload["last_used_at"].is_null());
        assert_eq!(payload["usage"]["error"].as_str(), Some("being deleted"));
        assert!(payload["usage"].get("payload").is_none());
    }

    #[test]
    fn error_display_is_message() {
        let error = ProviderUsageRouteError::new("parsing agent server config failed");

        assert_eq!(error.message(), "parsing agent server config failed");
        assert_eq!(error.to_string(), "parsing agent server config failed");
    }
}
