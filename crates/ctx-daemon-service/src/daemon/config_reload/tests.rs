use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

use anyhow::{anyhow, Result};
use ctx_history_capture::DiscoveryContext;

use super::*;
use crate::{DaemonConfigSnapshot, DaemonIpcService, DaemonProductConfig};

struct ReloadTestConfig {
    config: Mutex<Option<DaemonConfigSnapshot>>,
    load_error: Mutex<Option<String>>,
}

impl ReloadTestConfig {
    fn initialize(config: DaemonConfigSnapshot) -> &'static Self {
        *RELOAD_TEST_CONFIG
            .config
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(config);
        *RELOAD_TEST_CONFIG
            .load_error
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        &RELOAD_TEST_CONFIG
    }

    fn replace(&self, config: DaemonConfigSnapshot) {
        *self
            .config
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(config);
    }

    fn fail_load(&self, error: &str) {
        *self
            .load_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.to_owned());
    }
}

static RELOAD_TEST_SERIAL: Mutex<()> = Mutex::new(());
static RELOAD_TEST_CONFIG: ReloadTestConfig = ReloadTestConfig {
    config: Mutex::new(None),
    load_error: Mutex::new(None),
};

impl DaemonConfigPort for ReloadTestConfig {
    fn load(&self, _data_root: &Path) -> Result<DaemonConfigSnapshot> {
        if let Some(error) = self
            .load_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            return Err(anyhow!(error));
        }
        self.config
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| anyhow!("reload test configuration is uninitialized"))
    }

    fn semantic_model_config(&self, data_root: &Path) -> SemanticModelConfig {
        crate::test_support::CONFIG.semantic_model_config(data_root)
    }

    fn discovery_context(&self, data_root: &Path) -> Result<DiscoveryContext> {
        crate::test_support::CONFIG.discovery_context(data_root)
    }
}

fn semantic_config(executor: SemanticEmbeddingExecutorConfig) -> DaemonConfigSnapshot {
    DaemonConfigSnapshot {
        daemon: DaemonProductConfig {
            enabled: true,
            mode: DaemonMode::Full,
        },
        semantic_enabled: true,
        semantic_executor: executor,
        ..DaemonConfigSnapshot::default()
    }
}

struct ReloadTestContext {
    temp: tempfile::TempDir,
    config_port: &'static ReloadTestConfig,
    args: DaemonRunArgs,
    runtime: DaemonRuntime,
    wakeup: Arc<DaemonWakeup>,
    lifecycle: Arc<DaemonLifecycleState>,
    query_service: Option<DaemonQueryService>,
    refresh_service: Option<DaemonQueryService>,
    reload: DaemonConfigReloadState,
    _serial: MutexGuard<'static, ()>,
}

impl ReloadTestContext {
    fn new(config: DaemonConfigSnapshot) -> Self {
        let serial = RELOAD_TEST_SERIAL
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let temp = tempfile::tempdir().expect("create reload test root");
        ctx_history_platform::platform_security::establish_private_data_root(temp.path())
            .expect("secure reload test root");
        let config_port = ReloadTestConfig::initialize(config.clone());
        let runtime = DaemonRuntime {
            config: config.clone(),
            source_refresh_coordinator: Some(Arc::new(
                crate::source_backed_refresh_coordinator::CoreRefreshEngine::new(),
            )),
            ..DaemonRuntime::default()
        };
        Self {
            temp,
            config_port,
            args: DaemonRunArgs {
                loop_interval_seconds: None,
                max_chunks: None,
                handle_process_signals: false,
                force: false,
                profile: DaemonRunProfile::Persistent,
                start_mode: None,
                trigger_command: None,
                supervisor: crate::DaemonSupervisor::User,
            },
            runtime,
            wakeup: Arc::new(DaemonWakeup::default()),
            lifecycle: Arc::new(DaemonLifecycleState::starting()),
            query_service: None,
            refresh_service: None,
            reload: DaemonConfigReloadState::pending(&config),
            _serial: serial,
        }
    }

    fn reload(&mut self) -> DaemonConfigReloadOutcome {
        reload_daemon_runtime_config(
            self.temp.path(),
            &self.args,
            &mut self.runtime,
            DaemonConfigReloadContext {
                query_service: &mut self.query_service,
                refresh_service: &mut self.refresh_service,
                state: &mut self.reload,
                wakeup: &self.wakeup,
                lifecycle: &self.lifecycle,
                config_port: self.config_port,
            },
        )
    }

    #[cfg(any(unix, windows))]
    fn activate_initial_executor(&mut self) {
        assert_eq!(self.reload(), DaemonConfigReloadOutcome::Continue);
        assert_eq!(self.reload.status, "applied");
        assert!(self.runtime.semantic_executor.is_some());
        assert!(self.query_service.is_some());
        assert!(self.refresh_service.is_some());
    }
}

#[cfg(any(unix, windows))]
fn assert_failed_executor_switch(
    initial: SemanticEmbeddingExecutorConfig,
    replacement: SemanticEmbeddingExecutorConfig,
) {
    let mut context = ReloadTestContext::new(semantic_config(initial));
    context.activate_initial_executor();
    let old_executor = Arc::downgrade(
        context
            .runtime
            .semantic_executor
            .as_ref()
            .expect("initial semantic executor"),
    );
    let replacement_config = semantic_config(replacement.clone());
    context.config_port.replace(replacement_config);
    let query_endpoint = crate::query_service::daemon_service_endpoint_path(
        context.temp.path(),
        DaemonIpcService::SemanticQuery,
    );
    assert!(query_endpoint.exists());

    let outcome = reload_daemon_runtime_config_with_executor_builder(
        context.temp.path(),
        &context.args,
        &mut context.runtime,
        DaemonConfigReloadContext {
            query_service: &mut context.query_service,
            refresh_service: &mut context.refresh_service,
            state: &mut context.reload,
            wakeup: &context.wakeup,
            lifecycle: &context.lifecycle,
            config_port: context.config_port,
        },
        |selection, _, _, _| -> Result<SemanticEmbeddingExecutorHandle> {
            assert_eq!(selection, replacement);
            assert!(
                old_executor.upgrade().is_none(),
                "old executor must be cleared before replacement construction"
            );
            assert!(
                !query_endpoint.exists(),
                "old query service must stop before replacement construction"
            );
            Err(anyhow!("replacement executor construction failed"))
        },
    );

    assert_eq!(outcome, DaemonConfigReloadOutcome::Continue);
    assert_eq!(context.reload.status, "activation_failed");
    assert!(context
        .reload
        .last_error
        .as_deref()
        .is_some_and(|error| error.contains("replacement executor construction failed")));
    assert_eq!(context.runtime.config.semantic_executor, replacement);
    assert!(context.runtime.semantic_executor.is_none());
    assert!(context.query_service.is_none());
    assert!(context.refresh_service.is_some());
    assert!(!daemon_semantic_runtime_active(
        &context.runtime,
        context.query_service.as_ref()
    ));
    assert!(!query_endpoint.exists());
}

#[cfg(any(unix, windows))]
#[test]
fn failed_builtin_to_http_switch_clears_old_runtime_without_fallback() {
    assert_failed_executor_switch(
        SemanticEmbeddingExecutorConfig::builtin(),
        SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:41001")
            .expect("loopback HTTP executor config"),
    );
}

#[cfg(any(unix, windows))]
#[test]
fn failed_http_to_http_switch_clears_old_runtime_without_fallback() {
    assert_failed_executor_switch(
        SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:41002")
            .expect("initial loopback HTTP executor config"),
        SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:41003")
            .expect("replacement loopback HTTP executor config"),
    );
}

#[cfg(any(unix, windows))]
#[test]
fn successful_executor_switches_replace_runtime_and_query_service_including_builtin_return() {
    let builtin = SemanticEmbeddingExecutorConfig::builtin();
    let http_a = SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:41004")
        .expect("first loopback HTTP executor config");
    let http_b = SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:41005")
        .expect("second loopback HTTP executor config");
    let mut context = ReloadTestContext::new(semantic_config(builtin.clone()));
    context.activate_initial_executor();

    for expected in [http_a, http_b, builtin] {
        let old_executor = Arc::downgrade(
            context
                .runtime
                .semantic_executor
                .as_ref()
                .expect("active semantic executor"),
        );
        context
            .config_port
            .replace(semantic_config(expected.clone()));
        assert_eq!(context.reload(), DaemonConfigReloadOutcome::Continue);

        assert_eq!(context.reload.status, "applied");
        assert_eq!(context.runtime.config.semantic_executor, expected);
        let executor = context
            .runtime
            .semantic_executor
            .as_ref()
            .expect("replacement semantic executor");
        assert_eq!(executor.kind(), expected.kind());
        assert_eq!(executor.endpoint(), expected.http_endpoint());
        assert!(old_executor.upgrade().is_none());
        assert!(context.query_service.is_some());
        assert!(daemon_semantic_runtime_active(
            &context.runtime,
            context.query_service.as_ref()
        ));
    }
}

#[cfg(any(unix, windows))]
#[test]
fn disable_and_reenable_same_executor_clears_permanent_block_state() {
    let endpoint = SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:41006")
        .expect("loopback HTTP executor config");
    let mut context = ReloadTestContext::new(semantic_config(endpoint.clone()));
    context.runtime.semantic_retry.record_failure();
    context.runtime.semantic_blocked_job = Some(json!({
        "status": "failed",
        "failure_class": "permanent",
    }));

    let mut disabled = semantic_config(endpoint.clone());
    disabled.semantic_enabled = false;
    context.config_port.replace(disabled);
    assert_eq!(
        reload_daemon_runtime_config_with_executor_builder(
            context.temp.path(),
            &context.args,
            &mut context.runtime,
            DaemonConfigReloadContext {
                query_service: &mut context.query_service,
                refresh_service: &mut context.refresh_service,
                state: &mut context.reload,
                wakeup: &context.wakeup,
                lifecycle: &context.lifecycle,
                config_port: context.config_port,
            },
            |_, _, _, _| panic!("disabled semantic reload must not build an executor"),
        ),
        DaemonConfigReloadOutcome::Continue
    );
    assert_eq!(context.runtime.semantic_retry.consecutive_failures, 0);
    assert!(context.runtime.semantic_blocked_job.is_none());
    assert!(context.runtime.semantic_executor.is_none());

    context
        .config_port
        .replace(semantic_config(endpoint.clone()));
    assert_eq!(context.reload(), DaemonConfigReloadOutcome::Continue);
    assert_eq!(context.runtime.config.semantic_executor, endpoint);
    assert!(context.runtime.semantic_executor.is_some());
    assert!(context.query_service.is_some());
}

#[cfg(any(unix, windows))]
#[test]
fn config_load_failure_deactivates_semantic_runtime_without_stopping_core_refresh() {
    let endpoint = SemanticEmbeddingExecutorConfig::http("http://127.0.0.1:41007")
        .expect("loopback HTTP executor config");
    let mut context = ReloadTestContext::new(semantic_config(endpoint));
    context.activate_initial_executor();
    let old_executor = Arc::downgrade(
        context
            .runtime
            .semantic_executor
            .as_ref()
            .expect("active semantic executor"),
    );
    let query_endpoint = crate::query_service::daemon_service_endpoint_path(
        context.temp.path(),
        DaemonIpcService::SemanticQuery,
    );
    assert!(query_endpoint.exists());
    context.runtime.semantic_retry.record_failure();
    context.runtime.semantic_blocked_job = Some(json!({"status": "failed"}));
    context
        .config_port
        .fail_load("semantic executor configuration is malformed");

    assert_eq!(context.reload(), DaemonConfigReloadOutcome::Continue);

    assert_eq!(context.reload.status, "failed");
    assert!(context
        .reload
        .last_error
        .as_deref()
        .is_some_and(|error| error.contains("configuration is malformed")));
    assert!(old_executor.upgrade().is_none());
    assert!(context.runtime.semantic_executor.is_none());
    assert!(context.query_service.is_none());
    assert!(context.refresh_service.is_some());
    assert_eq!(context.runtime.semantic_retry.consecutive_failures, 0);
    assert!(context.runtime.semantic_blocked_job.is_none());
    assert!(!query_endpoint.exists());
}
