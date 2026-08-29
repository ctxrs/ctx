use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::Duration,
};

use anyhow::{anyhow, Result};
use ctx_history_capture::DiscoveryContext;
use ctx_semantic_model::ExternalSemanticSpace;

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

fn http_executor(
    endpoint: &str,
    space_id: &str,
    dimensions: usize,
) -> SemanticEmbeddingExecutorConfig {
    SemanticEmbeddingExecutorConfig::http(
        endpoint,
        ExternalSemanticSpace::new(space_id, dimensions).expect("external semantic space"),
    )
    .expect("loopback HTTP executor config")
}

fn contract_response_endpoint(
    space_id: &str,
    dimensions: usize,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind contract response server");
    let endpoint = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("read contract response server address")
    );
    let body = json!({
        "schema_version": 1,
        "space_id": space_id,
        "dimensions": dimensions,
    })
    .to_string();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept contract verification");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("bound contract request read");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("read contract request");
            assert!(read > 0, "contract request ended before headers");
            request.extend_from_slice(&buffer[..read]);
        }
        let request = String::from_utf8(request).expect("contract request is UTF-8");
        assert!(
            request.starts_with("GET /v1/contract HTTP/1.1\r\n") && request.ends_with("\r\n\r\n"),
            "verification must be a content-free contract GET: {request}"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write contract response");
    });
    (endpoint, server)
}

fn unavailable_http_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve unavailable HTTP endpoint");
    let endpoint = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("read unavailable HTTP endpoint")
    );
    drop(listener);
    endpoint
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
fn assert_refresh_service_usable(context: &ReloadTestContext) {
    let response = crate::query_service::daemon_source_refresh_request(
        context.temp.path(),
        json!({"schema_version": 1, "op": "ping"}),
        Duration::from_secs(1),
        64 * 1024,
    )
    .expect("source refresh IPC request")
    .expect("source refresh ping response");
    assert_eq!(response["ok"], true);
    assert_eq!(response["service"], "source_refresh");
}

#[cfg(any(unix, windows))]
fn assert_external_activation_failed(context: &ReloadTestContext) {
    assert_eq!(context.reload.status, "activation_failed");
    assert!(context.reload.last_error.is_some());
    assert!(context.runtime.semantic_executor.is_none());
    assert!(context.query_service.is_none());
    assert!(context.refresh_service.is_some());
    assert!(!daemon_semantic_runtime_active(
        &context.runtime,
        context.query_service.as_ref()
    ));
    assert!(!crate::query_service::daemon_service_endpoint_path(
        context.temp.path(),
        DaemonIpcService::SemanticQuery,
    )
    .exists());
    let status = context.reload.to_json();
    assert_eq!(status["applied"]["semantic_enabled"], false);
    assert!(status["applied"]["semantic_executor"].is_null());
    assert!(status["applied"]["semantic_contract_fingerprint"].is_null());
    assert_refresh_service_usable(context);
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
    context.runtime.sidecar_drain.generation = Some("old-generation".to_owned());
    context.runtime.sidecar_drain.semantic_attempted_generation = Some("old-generation".to_owned());
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
    assert!(context.runtime.sidecar_drain.generation.is_none());
    assert!(context
        .runtime
        .sidecar_drain
        .semantic_attempted_generation
        .is_none());
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
        http_executor("http://127.0.0.1:41001", "space-a", 128),
    );
}

#[cfg(any(unix, windows))]
#[test]
fn endpoint_space_and_dimension_drift_each_replace_the_executor_boundary() {
    for replacement_kind in 0..3 {
        let (initial_endpoint, server) = contract_response_endpoint("space-a", 128);
        let initial = http_executor(&initial_endpoint, "space-a", 128);
        let replacement = match replacement_kind {
            0 => http_executor("http://127.0.0.1:41003", "space-a", 128),
            1 => http_executor(&initial_endpoint, "space-b", 128),
            _ => http_executor(&initial_endpoint, "space-a", 256),
        };
        assert_failed_executor_switch(initial.clone(), replacement);
        server.join().expect("contract response server");
    }
}

#[cfg(any(unix, windows))]
#[test]
fn successful_executor_switches_replace_runtime_and_query_service_including_builtin_return() {
    let builtin = SemanticEmbeddingExecutorConfig::builtin();
    let (http_a_endpoint, http_a_server) = contract_response_endpoint("space-a", 128);
    let (http_b_endpoint, http_b_server) = contract_response_endpoint("space-b", 256);
    let http_a = http_executor(&http_a_endpoint, "space-a", 128);
    let http_b = http_executor(&http_b_endpoint, "space-b", 256);
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
        context.runtime.sidecar_drain.generation = Some("old-generation".to_owned());
        context.runtime.sidecar_drain.semantic_attempted_generation =
            Some("old-generation".to_owned());
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
        assert_eq!(executor.executor().contract(), expected.contract());
        let status = context.reload.to_json();
        assert_eq!(
            status["requested"]["semantic_contract_fingerprint"],
            expected.contract().fingerprint()
        );
        assert_eq!(
            status["applied"]["semantic_contract_fingerprint"],
            expected.contract().fingerprint()
        );
        assert!(old_executor.upgrade().is_none());
        assert!(context.runtime.sidecar_drain.generation.is_none());
        assert!(context
            .runtime
            .sidecar_drain
            .semantic_attempted_generation
            .is_none());
        assert!(context.query_service.is_some());
        assert!(daemon_semantic_runtime_active(
            &context.runtime,
            context.query_service.as_ref()
        ));
    }
    http_a_server
        .join()
        .expect("first contract response server");
    http_b_server
        .join()
        .expect("second contract response server");
}

#[cfg(any(unix, windows))]
#[test]
fn disable_and_reenable_same_executor_clears_permanent_block_state() {
    let (server_endpoint, server) = contract_response_endpoint("space-a", 128);
    let endpoint = http_executor(&server_endpoint, "space-a", 128);
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
    server.join().expect("contract response server");
}

#[cfg(any(unix, windows))]
#[test]
fn source_refresh_only_round_trip_resets_worker_state_and_reactivates_same_contract() {
    let executor = SemanticEmbeddingExecutorConfig::builtin();
    let mut context = ReloadTestContext::new(semantic_config(executor.clone()));
    context.activate_initial_executor();
    context.runtime.semantic_retry.record_failure();
    context.runtime.semantic_blocked_job = Some(json!({
        "status": "failed",
        "failure_class": "permanent",
    }));
    context.runtime.sidecar_drain.generation = Some("blocked-generation".to_owned());
    context.runtime.sidecar_drain.semantic_attempted_generation =
        Some("blocked-generation".to_owned());

    let mut source_only = semantic_config(executor.clone());
    source_only.daemon.mode = DaemonMode::SourceRefreshOnly;
    context.config_port.replace(source_only);
    assert_eq!(context.reload(), DaemonConfigReloadOutcome::Continue);

    assert_eq!(context.runtime.semantic_retry.consecutive_failures, 0);
    assert!(context.runtime.semantic_blocked_job.is_none());
    assert!(context.runtime.sidecar_drain.generation.is_none());
    assert!(context
        .runtime
        .sidecar_drain
        .semantic_attempted_generation
        .is_none());
    assert!(context.runtime.semantic_executor.is_none());
    assert!(context.query_service.is_none());
    assert_refresh_service_usable(&context);

    context
        .config_port
        .replace(semantic_config(executor.clone()));
    assert_eq!(context.reload(), DaemonConfigReloadOutcome::Continue);

    assert_eq!(context.reload.status, "applied");
    assert_eq!(context.runtime.config.semantic_executor, executor);
    assert_eq!(context.runtime.semantic_retry.consecutive_failures, 0);
    assert!(context.runtime.semantic_blocked_job.is_none());
    assert!(context.runtime.semantic_executor.is_some());
    assert!(context.query_service.is_some());
    assert!(daemon_semantic_runtime_active(
        &context.runtime,
        context.query_service.as_ref()
    ));
    assert_refresh_service_usable(&context);
}

#[cfg(any(unix, windows))]
#[test]
fn finite_core_worker_applies_selected_contract_without_activating_semantic_runtime() {
    let endpoint = http_executor("http://127.0.0.1:41008", "space-finite", 192);
    let mut context = ReloadTestContext::new(semantic_config(endpoint.clone()));
    context.args.profile = DaemonRunProfile::FiniteCoreWorker;

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
            |_, _, _, _| panic!("finite Core worker must not build a semantic executor"),
        ),
        DaemonConfigReloadOutcome::Continue
    );

    assert!(context.runtime.semantic_executor.is_none());
    assert!(context.query_service.is_none());
    assert!(context.refresh_service.is_some());
    let status = context.reload.to_json();
    assert_eq!(status["status"], "applied");
    assert_eq!(status["applied"]["semantic_enabled"], false);
    assert_eq!(
        status["applied"]["semantic_executor"],
        endpoint.http_endpoint().unwrap()
    );
    assert_eq!(
        status["applied"]["semantic_contract_fingerprint"],
        endpoint.contract().fingerprint()
    );
}

#[cfg(any(unix, windows))]
#[test]
fn unavailable_external_contract_fails_before_semantic_runtime_activation() {
    let endpoint = http_executor(&unavailable_http_endpoint(), "selected-space", 128);
    let mut context = ReloadTestContext::new(semantic_config(endpoint));

    assert_eq!(context.reload(), DaemonConfigReloadOutcome::Continue);

    assert_external_activation_failed(&context);
}

#[cfg(any(unix, windows))]
#[test]
fn drifted_external_contract_fails_before_semantic_runtime_activation() {
    let (server_endpoint, server) = contract_response_endpoint("drifted-space", 128);
    let endpoint = http_executor(&server_endpoint, "selected-space", 128);
    let mut context = ReloadTestContext::new(semantic_config(endpoint));

    assert_eq!(context.reload(), DaemonConfigReloadOutcome::Continue);
    server.join().expect("contract response server");

    assert_external_activation_failed(&context);
}

#[cfg(any(unix, windows))]
#[test]
fn config_load_failure_deactivates_semantic_runtime_without_stopping_core_refresh() {
    let (server_endpoint, server) = contract_response_endpoint("space-a", 128);
    let endpoint = http_executor(&server_endpoint, "space-a", 128);
    let mut context = ReloadTestContext::new(semantic_config(endpoint));
    context.activate_initial_executor();
    server.join().expect("contract response server");
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
