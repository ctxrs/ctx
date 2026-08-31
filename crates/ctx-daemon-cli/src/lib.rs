#[cfg(test)]
fn committed_generation_recovery_error(
    recovery: ctx_history_index::CommittedPredecessorMigrationRecovery,
) -> ctx_history_index::IndexError {
    ctx_history_index::IndexError::CommittedGenerationNeedsRecovery {
        generation_id: recovery.generation_id().to_owned(),
        stage: "predecessor migration recovery",
        detail: recovery.detail().to_owned(),
    }
}

mod composition;
pub use composition::{install_host, DaemonCliHost, DaemonConfig, DaemonMode, DaemonRuntimeConfig};
pub use ctx_daemon_application::DaemonHostRunRequest;
pub use ctx_daemon_runtime::apply_supervisor_environment_handoff;
pub use ctx_daemon_service::{
    CoreGenerationPublished, DaemonConfigSnapshot, DaemonUpgradePorts, SemanticFailureClass,
};
pub use ctx_semantic_model::{
    ExternalSemanticSpace, SemanticEmbeddingExecutorAuth, SemanticEmbeddingExecutorConfig,
    SemanticEmbeddingExecutorHandle, SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
    SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV,
};

#[cfg(test)]
pub(crate) mod test_environment {
    use std::{
        ffi::{OsStr, OsString},
        sync::{Mutex, MutexGuard},
    };

    static TEST_ENVIRONMENT_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) struct EnvironmentGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvironmentGuard {
        pub(crate) fn capture(names: &[&'static str]) -> Self {
            let lock = TEST_ENVIRONMENT_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let saved = names
                .iter()
                .map(|name| (*name, std::env::var_os(name)))
                .collect();
            Self { _lock: lock, saved }
        }

        pub(crate) fn set(&self, name: &'static str, value: Option<&OsStr>) {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    impl Drop for EnvironmentGuard {
        fn drop(&mut self) {
            for (name, value) in self.saved.drain(..).rev() {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

pub fn semantic_embedding_executor_auth_from_environment(
) -> anyhow::Result<SemanticEmbeddingExecutorAuth> {
    let endpoint_binding = match std::env::var(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV) {
        Ok(binding) => binding,
        // An unbound inherited token is deliberately ignored. This keeps a
        // remote credential out of an unauthenticated loopback executor; a
        // remote executor subsequently fails closed because it has no auth.
        Err(std::env::VarError::NotPresent) => return Ok(SemanticEmbeddingExecutorAuth::none()),
        Err(std::env::VarError::NotUnicode(_)) => anyhow::bail!(
            "semantic embedding authentication endpoint binding must be valid Unicode"
        ),
    };
    let token = match std::env::var(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV) {
        Ok(token) => token,
        Err(std::env::VarError::NotPresent) => return Ok(SemanticEmbeddingExecutorAuth::none()),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("semantic embedding authentication token must be valid Unicode")
        }
    };
    Ok(SemanticEmbeddingExecutorAuth::bearer(
        token,
        endpoint_binding,
    ))
}

pub fn supervisor_environment_allowlist_names() -> Vec<&'static str> {
    ctx_daemon_application::supervisor_environment_allowlist_names()
}

#[cfg(test)]
#[test]
fn daemon_environment_preserves_the_endpoint_bound_semantic_embedding_token() {
    let allowlist = supervisor_environment_allowlist_names();
    assert!(allowlist.contains(&SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV));
    assert!(allowlist.contains(&SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV));
}

#[cfg(test)]
mod semantic_executor_auth_tests {
    use std::{ffi::OsStr, path::PathBuf};

    use ctx_semantic_model::{
        SemanticModelConfig, SemanticModelPaths, SemanticOnnxRuntimePaths, SharedSemanticRuntime,
    };

    use super::*;

    fn loopback_executor() -> SemanticEmbeddingExecutorHandle {
        let auth = semantic_embedding_executor_auth_from_environment().unwrap();
        SemanticEmbeddingExecutorHandle::build_with_auth(
            SemanticEmbeddingExecutorConfig::http(
                "http://127.0.0.1:41007",
                ExternalSemanticSpace::new("test-space", 384).unwrap(),
            )
            .unwrap(),
            auth,
            SharedSemanticRuntime::default(),
            SemanticModelConfig::new(SemanticModelPaths::new(
                PathBuf::from("test-semantic-model-cache"),
                SemanticOnnxRuntimePaths::new(PathBuf::from("test-semantic-runtime-cache")),
            )),
        )
        .unwrap()
    }

    #[test]
    fn unbound_token_is_ignored_until_an_exact_endpoint_binding_is_present() {
        let environment = crate::test_environment::EnvironmentGuard::capture(&[
            SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV,
            SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
        ]);
        environment.set(
            SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV,
            Some(OsStr::new("loopback-token")),
        );
        environment.set(SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV, None);
        assert!(!loopback_executor()
            .http_executor()
            .unwrap()
            .authentication_configured());

        environment.set(
            SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
            Some(OsStr::new("http://127.0.0.1:41007/")),
        );
        assert!(loopback_executor()
            .http_executor()
            .unwrap()
            .authentication_configured());
    }
}

use ctx_terminal::compact_json;

mod identity {
    pub fn home_dir() -> Option<std::path::PathBuf> {
        crate::composition::host().home_dir()
    }
}

mod analytics {
    use std::path::Path;

    use ctx_client_observability::analytics::PublicEventV1;

    pub fn send_batch(data_root: &Path, events: &[PublicEventV1]) {
        crate::composition::host().deliver_daemon_events(data_root, events);
    }
}

mod net {
    use std::{io::Write, time::Duration};

    use anyhow::Result;

    pub fn get_to_writer_limited(
        endpoint: &str,
        max_bytes: u64,
        timeout: Duration,
        writer: &mut dyn Write,
    ) -> Result<u64> {
        crate::composition::host().fetch_to_writer(endpoint, max_bytes, timeout, writer)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("CLI error was already rendered")]
pub struct RenderedCliError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStartModeArg {
    Auto,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonTriggerCommandArg {
    Setup,
    Import,
    Search,
    Semantic,
}

impl DaemonTriggerCommandArg {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Import => "import",
            Self::Search => "search",
            Self::Semantic => "semantic",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DaemonArgs {
    pub command: DaemonCommand,
}

#[derive(Debug, Clone)]
pub enum DaemonCommand {
    Run(DaemonRunArgs),
    Status(FormatArgs),
    Enable(FormatArgs),
    Disable(DaemonDisableArgs),
}

#[derive(Debug, Clone)]
pub struct FormatArgs {
    pub format: ctx_terminal::JsonOutputFormat,
}

#[derive(Debug)]
pub struct IndexingModeUpdate {
    pub automatic: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub persistent: bool,
    pub supervisor: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct DaemonDisableArgs {
    pub format: ctx_terminal::JsonOutputFormat,
    pub prepare_uninstall: bool,
}

#[derive(Debug, Clone)]
pub struct DaemonRunArgs {
    pub loop_interval_seconds: Option<u64>,
    pub max_chunks: Option<usize>,
    pub finite_core_worker: bool,
    pub force: bool,
    pub start_mode: Option<DaemonStartModeArg>,
    pub trigger_command: Option<DaemonTriggerCommandArg>,
    pub format: ctx_terminal::JsonOutputFormat,
}

#[allow(unused_imports)]
pub use ctx_semantic_model::{
    prepare_platform_semantic_acceleration, semantic_managed_model_snapshot_dir,
    semantic_native_accelerator_target, semantic_provisioning_coreml_asset_matches,
    semantic_provisioning_model_contract_matches, semantic_provisioning_model_path_count,
    semantic_provisioning_model_path_matches, semantic_query_service_supported,
    semantic_required_model_file_count, semantic_required_model_file_matches,
    SemanticNativeAcceleratorTarget, SemanticOrtModelVariant,
};
#[cfg(test)]
#[allow(unused_imports)]
use ctx_semantic_model::{
    semantic_model_cache_available, semantic_model_key, SemanticDaemonCpuFallbackRequired,
    SemanticDaemonModelAcquisition, SemanticModelLoadDeferred, SharedSemanticRuntime,
    SEMANTIC_DIMENSIONS,
};
mod model_config;
pub use model_config::{semantic_runtime_cache_dir, semantic_worker_cache_dir};
mod runtime_limits;
pub use ctx_semantic_index::SemanticNotReady;
#[allow(unused_imports)]
pub use runtime_limits::SEMANTIC_WORKER_BATCH_MAX;
mod query_adapter;
pub use query_adapter::{wait_for_daemon_semantic_generation, SemanticQueryAdapter};
mod semantic_completion;
pub use semantic_completion::{
    complete_semantic_generation_foreground,
    complete_semantic_generation_foreground_with_checkpoint, DaemonSemanticCompletion,
    SemanticCompletionBudgets, SemanticCompletionCheckpoint, SemanticCompletionDaemonConfig,
    SemanticCompletionError,
};
mod query_service;
pub use query_service::{wait_for_daemon_query_service, wait_for_daemon_query_service_cancellable};
mod daemon;
mod paths_status;
pub use daemon::{run_daemon_command, update_indexing_mode};
pub mod daemon_service_ports;
mod daemon_status;
mod daemon_supervisor;
mod source_status;
pub use source_status::{
    current_rejected_record_count, source_epoch_status_report, SourceEpochStatus,
};
mod source_backed_refresh_coordinator;
pub use source_backed_refresh_coordinator::{
    coordinate_import_source_backed_refresh_with_progress,
    coordinate_setup_source_backed_refresh_with_progress, coordinate_source_backed_refresh,
    coordinate_source_backed_refresh_with_progress, pin_active_verified_generation,
    published_explicit_source_relocation_authority, PinnedSourceBackedGeneration, RefreshStatus,
    SourceBackedRefreshDaemonUnavailable, SourceBackedRefreshMode, SourceBackedRefreshObservation,
    SourceBackedRefreshPendingPublication, SourceBackedRefreshTerminalError,
};
mod finite_worker_owner;
pub use finite_worker_owner::{
    checkpoint as foreground_checkpoint, finish_foreground_result, finite_worker_interrupted,
    foreground_interrupt_epoch, foreground_operation_active, foreground_result_interrupted,
    record_foreground_interrupt, with_foreground_guard_since, FiniteWorkerInterrupted,
};
mod daemon_autostart;
#[allow(unused_imports)]
pub use daemon_autostart::{
    autostart_daemon_and_wait, autostart_daemon_for_setup_and_wait,
    begin_current_daemon_upgrade_handoff, begin_daemon_upgrade_handoff,
    begin_legacy_daemon_upgrade_handoff, complete_replacement_daemon_handoff,
    daemon_autostart_suppression_reason, finish_replacement_daemon_handoff,
    mark_replacement_helper_handoff, maybe_autostart_daemon, observe_daemon_for_setup_and_wait,
    replacement_helper_owns_daemon_handoff, restart_daemon_with_current_environment_and_wait,
    DaemonHandoff, DaemonSetupHandoff, DaemonUpgradeHandoff,
};

/// Persists the final-binary restart intent consumed only after daemon readiness.
pub fn publish_daemon_restart_intent(
    data_root: &std::path::Path,
    trigger: DaemonTriggerCommandArg,
    request_id: &str,
) -> anyhow::Result<std::path::PathBuf> {
    daemon_autostart::write_daemon_restart_request(data_root, trigger, request_id)
}

/// Reports whether a recognized final-binary restart intent remains durable.
pub fn daemon_restart_intent_pending(data_root: &std::path::Path) -> bool {
    daemon_autostart::read_daemon_restart_request(data_root).is_some()
}
mod health_search;
#[cfg(test)]
mod tests;
