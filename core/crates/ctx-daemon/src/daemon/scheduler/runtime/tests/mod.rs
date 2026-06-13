use super::helpers;
use super::helpers::{runtime_provider_id_for_session_provider, strip_emitted_prefix};
use super::provider_env::provider_mode_id_for;
use super::turn_start::apply_crp_launch_policy_env_for_control_mode;
use chrono::Utc;
use ctx_core::provider_policy::{CTX_CRP_LAUNCH_POLICY_ENV, CTX_CRP_LAUNCH_POLICY_FULL};
use ctx_harness_sources::{
    EndpointModelCatalogStatus, HarnessApiShape, HarnessEndpointRecord,
    HarnessEndpointVerificationStatus, HarnessSourceKind, ResolvedHarnessSource,
};
use ctx_managed_installs as installer;
use ctx_managed_installs::{
    ensure_codex_cli_command_env_for_target, AgentServerCommand, AgentServerConfigFile,
    ManagedInstallMetadata,
};
use ctx_provider_accounts::{
    codex_env_for_active_account_with_runtime_root, codex_runtime_home, ensure_codex_account_dir,
    save_codex_registry, CodexAccountEntry, CodexAccountRegistry, CodexEndpointProfile,
    CODEX_CREDENTIAL_KIND_API_KEY,
};
use ctx_provider_install::install_state::InstallTarget;
use ctx_settings_model::ProviderControlMode;
use ctx_workspace_config as workspace_config;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn without(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.prev.as_deref() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

mod codex_env;
mod provider_modes;
mod runtime_path;
mod system_prompt;
