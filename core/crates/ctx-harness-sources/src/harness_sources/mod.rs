use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use url::Url;

use ctx_core::provider_ids::CODEX_PROVIDER_ID;
use ctx_provider_accounts::CODEX_AUTH_TYPE_BEARER;

const REGISTRY_VERSION: u32 = 1;
const SECRET_VERSION: u32 = 1;

const PROVIDER_CODEX: &str = CODEX_PROVIDER_ID;
const PROVIDER_CLAUDE: &str = "claude-crp";
const PROVIDER_GEMINI: &str = "gemini";
const PROVIDER_KIMI: &str = "kimi";
const PROVIDER_QWEN: &str = "qwen";
const PROVIDER_OPENCODE: &str = "opencode";
const PROVIDER_MISTRAL: &str = "mistral";
const PROVIDER_GOOSE: &str = "goose";
const PROVIDER_AMP: &str = "amp";
const PROVIDER_DROID: &str = "droid";
const PROVIDER_OPENHANDS: &str = "openhands";
const PROVIDER_COPILOT: &str = "copilot";
const PROVIDER_AUGGIE: &str = "auggie";
const PROVIDER_PI: &str = "pi";
const PROVIDER_CURSOR: &str = "cursor";
const PROVIDER_CLINE: &str = "cline";
const CTX_DROID_HOST_AUTH_PATH_ENV: &str = "CTX_DROID_HOST_AUTH_PATH";
pub const CTX_PROVIDER_ROUTE_BACKEND_ENV: &str = "CTX_PROVIDER_ROUTE_BACKEND";
pub const CTX_LLM_RELAY_BASE_URL_ENV: &str = "CTX_LLM_RELAY_BASE_URL";
pub const CTX_LLM_RELAY_MODEL_ENV: &str = "CTX_LLM_RELAY_MODEL";
const BUILT_IN_CTX_MANAGED_RELAY_PREFIXES: &[&str] = &[
    "https://api.ctx.rs/relay",
    "https://relay.ctx.rs/relay",
    "https://llm.ctx.rs/relay",
];

const CLAUDE_AUTH_TYPE_API_KEY: &str = "api_key";
const GEMINI_AUTH_TYPE_GEMINI_API_KEY: &str = "gemini_api_key";
const GEMINI_AUTH_TYPE_VERTEX_AI: &str = "vertex_ai";
const ENDPOINT_MODEL_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);
const ENDPOINT_MODEL_CATALOG_TTL: Duration = Duration::from_secs(60 * 60 * 24);
const GENERIC_ENDPOINT_NAMESPACE_LABELS: &[&str] = &[
    "api",
    "www",
    "app",
    "gateway",
    "proxy",
    "chat",
    "inference",
    "llm",
];

static REGISTRY_WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

mod model_catalog;
mod registry;
mod runtime_resolution;
mod secrets;
mod selection;
mod validation;

pub use model_catalog::{endpoint_model_catalog_is_stale, endpoint_model_catalog_ttl};
pub use runtime_resolution::{
    resolve_provider_source_for_probe, resolve_provider_source_for_probe_with_runtime_root,
    resolve_provider_source_for_run, resolve_provider_source_for_run_with_runtime_root,
};
pub use selection::{
    delete_provider_endpoint, find_provider_endpoint_import_match, get_provider_source_config,
    mark_endpoint_verification, refresh_provider_endpoint_model_catalog,
    set_provider_endpoint_manual_models, set_provider_source_selection, upsert_provider_endpoint,
};
pub use validation::{
    default_shape_for_provider, ensure_shape_compatible, supports_harness_endpoint,
};

pub fn droid_cli_model_id_for_endpoint_model(
    model_id: Option<&str>,
    base_url: Option<&str>,
) -> Option<String> {
    runtime_resolution::droid_cli_model_id_for_endpoint_model(model_id, base_url)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HarnessSourceKind {
    #[default]
    Subscription,
    Endpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessRouteBackend {
    UserManaged,
    CtxManagedRelay,
}

impl HarnessRouteBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserManaged => "user_managed",
            Self::CtxManagedRelay => "ctx_managed",
        }
    }

    pub fn from_runtime_marker(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "user_managed" | "direct" => Some(Self::UserManaged),
            "ctx_managed" | "ctx_managed_relay" | "relay" => Some(Self::CtxManagedRelay),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessRuntimeSourceMode {
    Subscription,
    Endpoint(HarnessRouteBackend),
}

impl HarnessRuntimeSourceMode {
    pub fn source_kind(self) -> HarnessSourceKind {
        match self {
            Self::Subscription => HarnessSourceKind::Subscription,
            Self::Endpoint(_) => HarnessSourceKind::Endpoint,
        }
    }

    pub fn route_backend(self) -> HarnessRouteBackend {
        match self {
            Self::Subscription => HarnessRouteBackend::UserManaged,
            Self::Endpoint(backend) => backend,
        }
    }

    pub fn is_ctx_managed(self) -> bool {
        self.route_backend() == HarnessRouteBackend::CtxManagedRelay
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HarnessApiShape {
    OpenaiResponses,
    AnthropicMessages,
}

impl HarnessApiShape {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiResponses => "openai_responses",
            Self::AnthropicMessages => "anthropic_messages",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HarnessEndpointVerificationStatus {
    Unknown,
    Valid,
    Invalid,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EndpointModelCatalogStatus {
    #[default]
    Unknown,
    Ready,
    ManualOnly,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EndpointModelRecord {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessEndpointRecord {
    pub id: String,
    pub provider_id: String,
    pub name: String,
    #[serde(default)]
    pub base_url: Option<String>,
    pub api_shape: HarnessApiShape,
    pub auth_type: String,
    #[serde(default)]
    pub model_override: Option<String>,
    #[serde(default)]
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub updated_at: DateTime<Utc>,
    pub last_verification_status: HarnessEndpointVerificationStatus,
    #[serde(default)]
    pub last_verification_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_error: Option<String>,
    pub has_api_key: bool,
    #[serde(default)]
    pub model_catalog_status: EndpointModelCatalogStatus,
    #[serde(default)]
    pub model_catalog_fetched_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub model_catalog_error: Option<String>,
    #[serde(default)]
    pub model_catalog_models: Vec<EndpointModelRecord>,
    #[serde(default)]
    pub manual_model_ids: Vec<String>,
    #[serde(default)]
    pub model_catalog_source: Option<String>,
}

impl HarnessEndpointRecord {
    pub fn route_backend(&self) -> HarnessRouteBackend {
        route_backend_for_base_url(self.base_url.as_deref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessProviderSourceConfig {
    pub provider_id: String,
    pub selected_source_kind: HarnessSourceKind,
    #[serde(default)]
    pub selected_endpoint_id: Option<String>,
    #[serde(default)]
    pub endpoints: Vec<HarnessEndpointRecord>,
}

impl HarnessProviderSourceConfig {
    pub fn selected_runtime_source_mode(&self) -> HarnessRuntimeSourceMode {
        if self.selected_source_kind != HarnessSourceKind::Endpoint {
            return HarnessRuntimeSourceMode::Subscription;
        }
        let backend = self
            .selected_endpoint_id
            .as_deref()
            .and_then(|selected_endpoint_id| {
                self.endpoints
                    .iter()
                    .find(|endpoint| endpoint.id == selected_endpoint_id)
            })
            .map(HarnessEndpointRecord::route_backend)
            .unwrap_or(HarnessRouteBackend::UserManaged);
        HarnessRuntimeSourceMode::Endpoint(backend)
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedHarnessSource {
    pub source_kind: HarnessSourceKind,
    pub endpoint: Option<HarnessEndpointRecord>,
    pub env: HashMap<String, String>,
}

impl ResolvedHarnessSource {
    pub fn runtime_source_mode(&self) -> HarnessRuntimeSourceMode {
        if self.source_kind != HarnessSourceKind::Endpoint {
            return HarnessRuntimeSourceMode::Subscription;
        }
        let backend = self
            .env
            .get(CTX_PROVIDER_ROUTE_BACKEND_ENV)
            .and_then(|value| HarnessRouteBackend::from_runtime_marker(value))
            .or_else(|| {
                self.endpoint
                    .as_ref()
                    .map(HarnessEndpointRecord::route_backend)
            })
            .unwrap_or(HarnessRouteBackend::UserManaged);
        HarnessRuntimeSourceMode::Endpoint(backend)
    }
}

#[derive(Debug, Clone)]
pub struct HarnessEndpointUpsert {
    pub endpoint_id: Option<String>,
    pub name: String,
    pub base_url: Option<String>,
    pub api_shape: Option<HarnessApiShape>,
    pub auth_type: Option<String>,
    pub model_override: Option<String>,
    pub api_key: Option<String>,
    pub service_account_json: Option<String>,
    pub project_id: Option<String>,
    pub location: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessEndpointImportMatchKind {
    ExactCredentials,
    SameConfig,
}

#[derive(Debug, Clone)]
pub struct HarnessEndpointImportMatch {
    pub endpoint_id: String,
    pub kind: HarnessEndpointImportMatchKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarnessEndpointRecordInternal {
    id: String,
    provider_id: String,
    name: String,
    base_url: String,
    api_shape: HarnessApiShape,
    auth_type: String,
    #[serde(default)]
    model_override: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    last_verification_status: HarnessEndpointVerificationStatus,
    #[serde(default)]
    last_verification_at: Option<DateTime<Utc>>,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    model_catalog_status: EndpointModelCatalogStatus,
    #[serde(default)]
    model_catalog_fetched_at: Option<DateTime<Utc>>,
    #[serde(default)]
    model_catalog_error: Option<String>,
    #[serde(default)]
    model_catalog_models: Vec<EndpointModelRecord>,
    #[serde(default)]
    manual_model_ids: Vec<String>,
    #[serde(default)]
    model_catalog_source: Option<String>,
    secret_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct HarnessProviderConfigInternal {
    #[serde(default)]
    selected_source_kind: HarnessSourceKind,
    #[serde(default)]
    selected_endpoint_id: Option<String>,
    #[serde(default)]
    endpoints: Vec<HarnessEndpointRecordInternal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HarnessSourceRegistryInternal {
    version: u32,
    #[serde(default)]
    providers: BTreeMap<String, HarnessProviderConfigInternal>,
}

fn route_backend_for_base_url(base_url: Option<&str>) -> HarnessRouteBackend {
    if base_url.is_some_and(base_url_uses_ctx_managed_relay) {
        HarnessRouteBackend::CtxManagedRelay
    } else {
        HarnessRouteBackend::UserManaged
    }
}

fn base_url_uses_ctx_managed_relay(base_url: &str) -> bool {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return false;
    }

    let Ok(parsed) = Url::parse(trimmed) else {
        return false;
    };

    if let Ok(prefix) = std::env::var("CTX_MANAGED_RELAY_BASE_URL") {
        let prefix = prefix.trim().trim_end_matches('/');
        if !prefix.is_empty()
            && Url::parse(prefix).is_ok_and(|parsed_prefix| {
                base_url_matches_ctx_managed_prefix(&parsed, &parsed_prefix)
            })
        {
            return true;
        }
    }

    BUILT_IN_CTX_MANAGED_RELAY_PREFIXES.iter().any(|prefix| {
        Url::parse(prefix)
            .is_ok_and(|parsed_prefix| base_url_matches_ctx_managed_prefix(&parsed, &parsed_prefix))
    })
}

fn base_url_matches_ctx_managed_prefix(base_url: &Url, prefix: &Url) -> bool {
    let same_host = base_url
        .host_str()
        .zip(prefix.host_str())
        .is_some_and(|(base_host, prefix_host)| base_host.eq_ignore_ascii_case(prefix_host));
    if base_url.scheme() != prefix.scheme()
        || !same_host
        || base_url.port_or_known_default() != prefix.port_or_known_default()
    {
        return false;
    }

    path_matches_prefix_boundary(base_url.path(), prefix.path())
}

fn path_matches_prefix_boundary(path: &str, prefix: &str) -> bool {
    let normalized_path = path.trim_end_matches('/');
    let normalized_prefix = prefix.trim_end_matches('/');
    if normalized_prefix.is_empty() {
        return false;
    }
    normalized_path == normalized_prefix
        || normalized_path
            .strip_prefix(normalized_prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

impl Default for HarnessSourceRegistryInternal {
    fn default() -> Self {
        Self {
            version: REGISTRY_VERSION,
            providers: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests;
