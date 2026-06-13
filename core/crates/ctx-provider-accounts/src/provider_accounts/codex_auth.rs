use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::shared::{container_runtime_data_roots, write_secure_file_atomic};
use super::{
    codex_account_deletion_in_progress, codex_account_dir, codex_broker_home, codex_runtime_home,
    codex_runtime_owner_path, codex_secret_path, default_codex_api_shape, default_codex_auth_type,
    default_codex_credential_kind, ensure_safe_account_id, legacy_codex_runtime_home,
    load_codex_registry, normalize_label, require_codex_account_exists, save_codex_registry,
    set_active_codex_account, upsert_codex_account, CodexAccountEntry, CodexAccountRegistry,
    CodexAuthImportOutcome, CodexEndpointProfile, CodexHostImportProbe, CODEX_AUTH_TYPE_BEARER,
    CODEX_CREDENTIAL_KIND_API_KEY, CODEX_CREDENTIAL_KIND_OAUTH, CODEX_SECRET_VERSION,
    CTX_CODEX_HOST_AUTH_PATH_ENV, CTX_SEED_CODEX_AUTH_FROM_HOST_ENV,
};

mod continuity;
mod host;
mod runtime;
mod runtime_cleanup;
mod runtime_oauth;
mod runtime_usage;
mod secret_store;

pub use self::continuity::acquire_codex_runtime_continuity_lock_from_env;
pub use self::host::{
    host_codex_auth_path, probe_host_codex_auth_candidate, seed_codex_auth_from_host,
    seeding_codex_auth_from_host_enabled,
};
pub use self::runtime::{
    codex_env_for_active_account, codex_env_for_active_account_with_runtime_root,
    codex_env_for_runtime_home, codex_has_active_auth, codex_has_active_auth_with_runtime_root,
    ensure_codex_auth_ready,
};
pub(crate) use self::runtime_cleanup::{
    clear_legacy_runtime_auth_projection_for_runtime_roots, clear_runtime_auth_projection,
    clear_runtime_auth_projection_for_runtime_roots,
};
pub use self::runtime_usage::{codex_usage_env_for_account, codex_usage_env_for_active_account};
pub use self::secret_store::{
    hydrate_codex_account_home_from_secret, import_codex_auth_value_to_secret_store,
    import_host_codex_auth_to_secret_store, ingest_codex_account_auth_to_secret_store,
    remove_codex_account_home_auth_if_present,
};
#[cfg(test)]
pub(crate) use self::{
    continuity::{expose_legacy_codex_state_from_home, expose_legacy_codex_state_to_broker_home},
    runtime::migrate_owned_runtime_oauth_projection_to_broker_if_needed,
    runtime_cleanup::write_runtime_owner_marker,
    runtime_oauth::codex_oauth_runtime_home,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexSecretEnvelope {
    version: u32,
    auth: serde_json::Value,
}

pub(crate) fn normalize_endpoint_profile(profile: &mut CodexEndpointProfile) {
    profile.api_shape = profile
        .api_shape
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_");
    if profile.api_shape.is_empty() {
        profile.api_shape = default_codex_api_shape();
    }
    profile.auth_type = profile.auth_type.trim().to_ascii_lowercase();
    if profile.auth_type.is_empty() {
        profile.auth_type = default_codex_auth_type();
    }
    if let Some(url) = profile.base_url.as_ref() {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            profile.base_url = None;
        } else {
            profile.base_url = Some(trimmed.to_string());
        }
    }
}

pub fn ensure_codex_endpoint_profile_compatible(profile: &CodexEndpointProfile) -> Result<()> {
    let shape = profile
        .api_shape
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_");
    if !matches!(shape.as_str(), "openai_responses" | "responses") {
        anyhow::bail!(
            "codex requires endpoint api_shape=openai_responses; found {}",
            profile.api_shape
        );
    }
    let auth = profile.auth_type.trim().to_ascii_lowercase();
    if auth != CODEX_AUTH_TYPE_BEARER {
        anyhow::bail!(
            "codex requires endpoint auth_type=bearer; found {}",
            profile.auth_type
        );
    }
    Ok(())
}
