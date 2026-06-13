use std::path::Path;

use anyhow::Context;
pub use ctx_settings_model::*;
use ctx_store::Store;
use serde::{Deserialize, Serialize};

mod dictation;
mod effective;
mod execution_policy;
mod overrides;
pub mod route_contract;

pub use dictation::DictationConfigError;
pub use effective::{
    apply_execution_environment, apply_workspace_execution_settings_override,
    effective_execution_settings, effective_execution_settings_classified,
    effective_execution_settings_for_environment, effective_install_target,
    effective_install_target_for_environment, install_target_for_settings,
    update_workspace_execution_config_for_loaded_settings,
    validate_execution_environment_against_settings,
    validate_workspace_execution_settings_override,
    workspace_execution_config_snapshot_for_loaded_settings, EffectiveExecutionSettingsError,
    WorkspaceExecutionConfigSnapshotError, WorkspaceExecutionConfigUpdateError,
};
pub use execution_policy::EXECUTION_POLICY_TEST_ENV_LOCK;
pub use execution_policy::{
    is_execution_policy_denial, ExecutionPolicyDenied, HostExecutionPolicy,
    CTX_HOST_EXECUTION_POLICY_ENV,
};

const SETTINGS_SCHEMA_VERSION: i64 = 1;
const RUNTIME_SETTINGS_SECRET_VERSION: u32 = 1;
const REMOVED_CLOUD_WORKER_SETTINGS_FIELD: &str = "cloud_workers";
const REMOVED_CLOUD_WORKER_SECRET_FIELDS: [&str; 2] = [
    "aws_cloud_workers_access_key_id",
    "aws_cloud_workers_secret_access_key",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeSettingsSecretEnvelope {
    version: u32,
    dictation_livekit_api_key: String,
    #[serde(default)]
    dictation_livekit_api_secret: Option<String>,
    title_generation_remote_api_key: String,
    oracle_api_key: String,
}

struct LoadedRuntimeSettingsSecretEnvelope {
    envelope: RuntimeSettingsSecretEnvelope,
    removed_cloud_worker_secrets_present: bool,
}

pub fn to_public(settings: &Settings) -> PublicSettings {
    ctx_settings_model::to_public(settings)
}

pub fn apply_update(current: Settings, req: UpdateSettingsReq) -> Settings {
    ctx_settings_model::apply_update(current, req)
}

#[cfg(test)]
mod effective_tests;
#[cfg(test)]
mod tests;

fn runtime_settings_secrets_from_settings(settings: &Settings) -> RuntimeSettingsSecretEnvelope {
    RuntimeSettingsSecretEnvelope {
        version: RUNTIME_SETTINGS_SECRET_VERSION,
        dictation_livekit_api_key: settings
            .dictation
            .as_ref()
            .and_then(|dictation| dictation.livekit.as_ref())
            .map(|livekit| livekit.api_key.clone())
            .unwrap_or_default(),
        dictation_livekit_api_secret: settings
            .dictation
            .as_ref()
            .and_then(|dictation| dictation.livekit.as_ref())
            .and_then(|livekit| livekit.api_secret.clone()),
        title_generation_remote_api_key: settings
            .title_generation
            .as_ref()
            .map(|title_generation| title_generation.remote.api_key.clone())
            .unwrap_or_default(),
        oracle_api_key: settings
            .oracle
            .as_ref()
            .map(|oracle| oracle.api_key.clone())
            .unwrap_or_default(),
    }
}

fn apply_runtime_settings_secrets(
    settings: &mut Settings,
    secrets: &RuntimeSettingsSecretEnvelope,
) {
    if let Some(livekit) = settings
        .dictation
        .as_mut()
        .and_then(|dictation| dictation.livekit.as_mut())
    {
        livekit.api_key = secrets.dictation_livekit_api_key.clone();
        livekit.api_secret = secrets.dictation_livekit_api_secret.clone();
    }
    if let Some(title_generation) = settings.title_generation.as_mut() {
        title_generation.remote.api_key = secrets.title_generation_remote_api_key.clone();
    }
    if let Some(oracle) = settings.oracle.as_mut() {
        oracle.api_key = secrets.oracle_api_key.clone();
    }
}

fn strip_runtime_settings_secrets(settings: &mut Settings) {
    if let Some(livekit) = settings
        .dictation
        .as_mut()
        .and_then(|dictation| dictation.livekit.as_mut())
    {
        livekit.api_key.clear();
        livekit.api_secret = None;
    }
    if let Some(title_generation) = settings.title_generation.as_mut() {
        title_generation.remote.api_key.clear();
    }
    if let Some(oracle) = settings.oracle.as_mut() {
        oracle.api_key.clear();
    }
}

fn settings_contain_runtime_secrets(settings: &Settings) -> bool {
    settings
        .dictation
        .as_ref()
        .and_then(|dictation| dictation.livekit.as_ref())
        .is_some_and(|livekit| {
            !livekit.api_key.trim().is_empty()
                || livekit
                    .api_secret
                    .as_deref()
                    .is_some_and(|secret| !secret.trim().is_empty())
        })
        || settings
            .title_generation
            .as_ref()
            .is_some_and(|title_generation| !title_generation.remote.api_key.trim().is_empty())
        || settings
            .oracle
            .as_ref()
            .is_some_and(|oracle| !oracle.api_key.trim().is_empty())
}

async fn load_runtime_settings_secret_envelope(
    store: &Store,
    secret_ref: &str,
) -> anyhow::Result<LoadedRuntimeSettingsSecretEnvelope> {
    let payload = store
        .read_runtime_settings_secrets_if_present(secret_ref)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "runtime settings secrets are missing for settings document (secret_ref={secret_ref})"
            )
        })?;
    let removed_cloud_worker_secrets_present =
        runtime_settings_secret_payload_contains_removed_cloud_worker_secrets(&payload);
    let envelope = serde_json::from_str::<RuntimeSettingsSecretEnvelope>(&payload)
        .context("parsing runtime settings secret envelope")?;
    if envelope.version != RUNTIME_SETTINGS_SECRET_VERSION {
        anyhow::bail!(
            "unsupported runtime settings secret version {}",
            envelope.version
        );
    }
    Ok(LoadedRuntimeSettingsSecretEnvelope {
        envelope,
        removed_cloud_worker_secrets_present,
    })
}

fn runtime_settings_json_contains_removed_cloud_worker_settings(settings_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(settings_json)
        .ok()
        .and_then(|value| {
            value
                .as_object()
                .map(|object| object.contains_key(REMOVED_CLOUD_WORKER_SETTINGS_FIELD))
        })
        .unwrap_or(false)
}

fn runtime_settings_secret_payload_contains_removed_cloud_worker_secrets(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .is_some_and(|object| {
            REMOVED_CLOUD_WORKER_SECRET_FIELDS
                .iter()
                .any(|field| object.contains_key(*field))
        })
}

pub async fn load_settings(store: &Store) -> anyhow::Result<Settings> {
    let host_execution_policy = HostExecutionPolicy::current()?;
    host_execution_policy.validate_process_execution_mode_override()?;
    let mut settings = match store.get_runtime_settings_document().await? {
        Some(doc) => {
            let mut settings = serde_json::from_str::<Settings>(&doc.settings_json)
                .context("parsing runtime settings document")?;
            let legacy_secrets_present = settings_contain_runtime_secrets(&settings);
            let removed_cloud_worker_settings_present =
                runtime_settings_json_contains_removed_cloud_worker_settings(&doc.settings_json);
            match doc.secret_ref.as_deref() {
                Some(secret_ref) => {
                    let loaded_secrets =
                        load_runtime_settings_secret_envelope(store, secret_ref).await?;
                    apply_runtime_settings_secrets(&mut settings, &loaded_secrets.envelope);
                    if legacy_secrets_present
                        || removed_cloud_worker_settings_present
                        || loaded_secrets.removed_cloud_worker_secrets_present
                    {
                        save_settings(store, &settings).await?;
                        store.checkpoint_wal_truncate().await?;
                    }
                }
                None => {
                    if legacy_secrets_present || removed_cloud_worker_settings_present {
                        save_settings(store, &settings).await?;
                        store.checkpoint_wal_truncate().await?;
                    }
                }
            }
            settings
        }
        None => Settings::default(),
    };
    ensure_settings_defaults(&mut settings);
    normalize_settings_in_place(&mut settings);

    overrides::apply_env_overrides(&mut settings);
    if let Some(execution) = settings.execution.as_mut() {
        host_execution_policy.normalize_loaded_execution_settings(execution);
    }

    Ok(settings)
}

/// Loads runtime settings from the canonical global database under a data root.
///
/// This opens the store and runs normal settings migrations/defaulting. Callers
/// should treat it as the settings-service entrypoint for data-root scoped
/// bootstrap reads, not as a generic read-only SQLite probe.
pub async fn load_settings_from_data_root(data_root: &Path) -> anyhow::Result<Settings> {
    let db_path = data_root.join("db").join("db.sqlite");
    let store = Store::open_sqlite(&db_path, Some(1))
        .await
        .with_context(|| format!("opening runtime settings store at {}", db_path.display()))?;
    let result = load_settings(&store).await;
    store.close().await;
    result
}

pub async fn save_settings(store: &Store, settings: &Settings) -> anyhow::Result<()> {
    let mut normalized = settings.clone();
    normalize_settings_in_place(&mut normalized);
    if settings_contain_runtime_secrets(&normalized) {
        let secrets_json =
            serde_json::to_string_pretty(&runtime_settings_secrets_from_settings(&normalized))?;
        strip_runtime_settings_secrets(&mut normalized);
        let settings_json = serde_json::to_string_pretty(&normalized)?;
        store
            .upsert_runtime_settings_document_with_secrets(
                SETTINGS_SCHEMA_VERSION,
                &settings_json,
                &secrets_json,
            )
            .await?;
    } else {
        let settings_json = serde_json::to_string_pretty(&normalized)?;
        store
            .upsert_runtime_settings_document(SETTINGS_SCHEMA_VERSION, &settings_json)
            .await?;
    }
    Ok(())
}
