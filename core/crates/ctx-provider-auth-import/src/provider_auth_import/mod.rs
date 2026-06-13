use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use ctx_harness_sources as harness_sources;
use ctx_provider_accounts as provider_accounts;

const DEFAULT_OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
const CTX_PROVIDER_AUTH_IMPORT_HOME_ENV: &str = "CTX_PROVIDER_AUTH_IMPORT_HOME";
const CTX_PROVIDER_AUTH_IMPORT_XDG_CONFIG_HOME_ENV: &str =
    "CTX_PROVIDER_AUTH_IMPORT_XDG_CONFIG_HOME";
const CTX_PROVIDER_AUTH_IMPORT_XDG_DATA_HOME_ENV: &str = "CTX_PROVIDER_AUTH_IMPORT_XDG_DATA_HOME";
const CTX_PROVIDER_AUTH_IMPORT_CODEX_HOME_ENV: &str = "CTX_PROVIDER_AUTH_IMPORT_CODEX_HOME";

mod catalog;
mod importers;
mod legacy;
mod parsers;
mod route_contract;

pub use route_contract::{
    ProviderAuthImportCandidatesRouteResponse, ProviderAuthImportProfilesRouteResponse,
    ProviderAuthImportRouteError, ProviderAuthImportRouteRequest, ProviderAuthImportRouteResponse,
};

#[cfg(test)]
use catalog::{host_roots, scan_with_roots, sha256_hex, summarize_env};
#[cfg(test)]
use importers::import_codex_candidate;
#[cfg(test)]
use legacy::{imported_secret_path, legacy_migration_marker_path};
#[cfg(test)]
use parsers::parse_env_file;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAuthImportCandidate {
    pub id: String,
    pub provider_id: String,
    pub provider_label: String,
    pub kind: String,
    pub path: String,
    pub signal_strength: String,
    pub confidence: String,
    pub parse_status: String,
    #[serde(default)]
    pub unsupported_reason: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub account_identity: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub auth_type: Option<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub last_modified: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAuthImportResult {
    pub candidate_id: String,
    pub provider_id: String,
    pub status: String,
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

pub fn provider_auth_import_result_mutates_effective_auth(
    result: &ProviderAuthImportResult,
) -> bool {
    // `already_imported` can still mutate active account selection through
    // dedupe/upsert paths, so it is auth-affecting even when the secret is not new.
    matches!(
        result.status.as_str(),
        "imported" | "updated" | "already_imported"
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderImportedAuthProfile {
    pub id: String,
    pub provider_id: String,
    pub provider_label: String,
    pub label: String,
    #[serde(default)]
    pub account_identity: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub auth_type: Option<String>,
    pub source_path: String,
    pub source_kind: String,
    pub secret_fingerprint: String,
    pub imported_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderImportedAuthRegistry {
    #[serde(default)]
    pub profiles: Vec<ProviderImportedAuthProfile>,
}

#[derive(Debug, Clone)]
struct CandidateMaterial {
    candidate: ProviderAuthImportCandidate,
    importable: bool,
    secret_bytes: Option<Vec<u8>>,
    label: Option<String>,
}

#[derive(Debug, Clone)]
struct HostRoots {
    home: PathBuf,
    xdg_config: PathBuf,
    xdg_data: PathBuf,
    codex_home: PathBuf,
}

#[derive(Debug, Clone)]
struct PathSpec {
    provider_id: &'static str,
    provider_label: &'static str,
    kind: &'static str,
    signal_strength: &'static str,
    confidence: &'static str,
    importable: bool,
    unsupported_reason: Option<&'static str>,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct AuthImportScanner {
    roots: HostRoots,
}

struct CanonicalAuthImporter<'a> {
    data_root: &'a Path,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSecretMaterial {
    kind: String,
    source_path: String,
    #[serde(default)]
    content_b64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyMigrationMarker {
    version: u32,
    completed_at: DateTime<Utc>,
}

pub async fn load_imported_registry(data_root: &Path) -> Result<ProviderImportedAuthRegistry> {
    legacy::load_imported_registry(data_root).await
}

pub async fn save_imported_registry(
    data_root: &Path,
    registry: &ProviderImportedAuthRegistry,
) -> Result<()> {
    legacy::save_imported_registry(data_root, registry).await
}

pub async fn list_provider_auth_import_candidates() -> Result<Vec<ProviderAuthImportCandidate>> {
    catalog::list_provider_auth_import_candidates().await
}

pub async fn list_provider_auth_profiles(
    data_root: &Path,
) -> Result<Vec<ProviderImportedAuthProfile>> {
    importers::list_provider_auth_profiles(data_root).await
}

#[cfg(test)]
async fn import_candidate_to_canonical(
    data_root: &Path,
    material: &CandidateMaterial,
) -> Result<ProviderAuthImportResult> {
    importers::import_candidate_to_canonical(data_root, material).await
}

#[cfg(test)]
async fn migrate_legacy_imported_profiles_once(data_root: &Path) -> Result<()> {
    importers::migrate_legacy_imported_profiles_once(data_root).await
}

pub async fn import_provider_auth_candidates(
    data_root: &Path,
    candidate_ids: &[String],
) -> Result<Vec<ProviderAuthImportResult>> {
    importers::import_provider_auth_candidates(data_root, candidate_ids).await
}

#[cfg(test)]
mod tests;
