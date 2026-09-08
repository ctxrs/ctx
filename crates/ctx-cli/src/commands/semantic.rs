use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use ctx_cli_presentation::commands::{
    render_semantic_disabled, render_semantic_runtime_install, render_semantic_runtime_status,
    render_semantic_status, SemanticArgs, SemanticCommand, SemanticRuntimeBackendArg,
    SemanticRuntimeCommand,
};
use ctx_history_cli::HistoryConfigPort;
use ctx_semantic_model::{
    detected_accelerator_backend, install_operator_runtime, installed_runtime_report,
    supported_local_runtime_backends, SemanticRuntimeBackend, SemanticRuntimeInstall,
    SemanticRuntimeReport,
};
use serde_json::{json, Value};

use crate::{
    history_config::CliHistoryConfigAdapter,
    output::{compact_json, print_json},
    ui::Ui,
};
use ctx_app_config as config;

pub(crate) fn run_semantic(
    args: SemanticArgs,
    data_root: PathBuf,
    quiet: bool,
    config: &mut config::AppConfig,
    ui: &mut Ui,
) -> Result<()> {
    match args.command {
        SemanticCommand::Status(args) => {
            let report = semantic_report(&data_root, config, "status", true)?;
            render_report(report, args.format.is_json(), quiet, ui)
        }
        SemanticCommand::Enable(args) => {
            let previous_executor_was_http = config
                .semantic_embedding_executor()
                .http_endpoint()
                .is_some();
            let explicit_executor_selection = args.executor.is_some();
            if args.wait && !config.automatic_indexing_enabled() {
                bail!(
                    "semantic --wait requires automatic indexing; run `ctx index mode auto` or omit --wait and use an explicit semantic search with --refresh wait"
                );
            }
            if let Some(executor) = args.executor.as_deref() {
                set_semantic_executor_and_enable(&data_root, config, executor)?;
            } else {
                set_semantic_policy(&data_root, config, true)?;
            }
            if config.automatic_indexing_enabled() {
                let credential_boundary_may_have_changed =
                    semantic_mutation_requires_daemon_restart(
                        previous_executor_was_http,
                        config
                            .semantic_embedding_executor()
                            .http_endpoint()
                            .is_some(),
                        explicit_executor_selection,
                    );
                if credential_boundary_may_have_changed {
                    crate::semantic::restart_daemon_with_current_environment_and_wait(
                        &data_root,
                        config,
                        crate::DaemonTriggerCommandArg::Semantic,
                    )?;
                } else {
                    crate::semantic::autostart_daemon_and_wait(
                        &data_root,
                        config,
                        crate::DaemonTriggerCommandArg::Semantic,
                    )?;
                }
            }

            if args.wait {
                let mut telemetry = crate::analytics::IndexTelemetry::default();
                return super::index::run_index(
                    ctx_cli_presentation::commands::index::IndexArgs::semantic_wait(args.format),
                    data_root,
                    quiet,
                    &mut telemetry,
                    ui,
                );
            }
            let report = semantic_report(&data_root, config, "enable", false)?;
            render_report(report, args.format.is_json(), quiet, ui)
        }
        SemanticCommand::Disable(args) => {
            let selected_executor_is_http = config
                .semantic_embedding_executor()
                .http_endpoint()
                .is_some();
            set_semantic_policy(&data_root, config, false)?;
            crate::semantic::clear_embedding_auth_endpoint();
            if config.automatic_indexing_enabled() && selected_executor_is_http {
                crate::semantic::restart_daemon_with_current_environment_and_wait(
                    &data_root,
                    config,
                    crate::DaemonTriggerCommandArg::Semantic,
                )?;
            }
            let report = semantic_report(&data_root, config, "disable", false)?;
            if args.format.is_json() {
                print_json(report)
            } else if !quiet {
                ui.write_stdout(&render_semantic_disabled(ui.stdout_context(), &report))?;
                Ok(())
            } else {
                Ok(())
            }
        }
        SemanticCommand::Runtime(args) => match args.command {
            SemanticRuntimeCommand::Install(args) => {
                let runtime_root = selected_runtime_root(&data_root)?;
                let expected_archive_sha256 =
                    expected_archive_sha256(args.sha256.as_deref(), &args.archive)?;
                let installed = install_operator_runtime(&SemanticRuntimeInstall {
                    archive: &args.archive,
                    expected_archive_sha256: &expected_archive_sha256,
                    runtime_root: &runtime_root,
                    backend: args.backend.map(runtime_backend),
                    replace_existing: args.force,
                })?;
                // The archive decided which runtime this was, so the report
                // names the resolved backend rather than a requested one.
                let report = single_runtime_report(
                    "runtime_install",
                    false,
                    installed.backend,
                    &runtime_root,
                    Some(&installed),
                );
                if args.format.is_json() {
                    print_json(report)
                } else if !quiet {
                    ui.write_stdout(&render_semantic_runtime_install(
                        ui.stdout_context(),
                        &report,
                    ))?;
                    Ok(())
                } else {
                    Ok(())
                }
            }
            SemanticRuntimeCommand::Status(args) => {
                let runtime_root = selected_runtime_root(&data_root)?;
                let report = match args.backend {
                    Some(requested) => {
                        let backend = runtime_backend(requested);
                        let installed = installed_runtime_report(&runtime_root, backend)?;
                        single_runtime_report(
                            "runtime_status",
                            true,
                            backend,
                            &runtime_root,
                            installed.as_ref(),
                        )
                    }
                    None => every_runtime_report(&runtime_root)?,
                };
                if args.format.is_json() {
                    print_json(report)
                } else if !quiet {
                    ui.write_stdout(&render_semantic_runtime_status(
                        ui.stdout_context(),
                        &report,
                    ))?;
                    Ok(())
                } else {
                    Ok(())
                }
            }
        },
    }
}

/// Runtime root shared by the operator installer and the runtime loader.
/// `CTX_RUNTIME_DIR` overrides the data-root default and must be absolute so a
/// provisioned runtime is never bound to a process working directory.
fn selected_runtime_root(data_root: &Path) -> Result<PathBuf> {
    let (source, root) = match std::env::var_os("CTX_RUNTIME_DIR") {
        Some(value) => ("CTX_RUNTIME_DIR", PathBuf::from(value)),
        None => ("selected ctx data root", data_root.join("runtime")),
    };
    if root.as_os_str().is_empty()
        || root
            .to_str()
            .is_some_and(|value| value.trim().is_empty() || value.trim() != value)
    {
        bail!("{source} must not be empty or whitespace-padded");
    }
    if !root.is_absolute() {
        bail!("{source} must be an absolute path");
    }
    Ok(root)
}

/// Expected archive digest for an operator install. Release sidecars publish
/// `<archive>.sha256`, so the common case needs no retyped digest; an absent
/// sidecar and no `--sha256` is refused rather than installing unverified bytes.
fn expected_archive_sha256(explicit: Option<&str>, archive: &Path) -> Result<String> {
    if let Some(digest) = explicit {
        return Ok(digest.trim().to_ascii_lowercase());
    }
    let mut sidecar = archive.as_os_str().to_owned();
    sidecar.push(".sha256");
    let sidecar = PathBuf::from(sidecar);
    let recorded = match std::fs::read_to_string(&sidecar) {
        Ok(recorded) => recorded,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "--sha256 was not given and no checksum file exists at {}; pass --sha256 <digest> or place the published checksum file next to the archive",
                sidecar.display()
            )
        }
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", sidecar.display()));
        }
    };
    let digest = recorded.split_whitespace().next().ok_or_else(|| {
        anyhow!(
            "{} records no checksum; pass --sha256 <digest> instead",
            sidecar.display()
        )
    })?;
    Ok(digest.to_ascii_lowercase())
}

/// Translates one `--backend` value. The core installer owns which backends
/// this build can provision locally and which runtime an archive carries, so
/// the CLI only translates the value instead of restating either policy.
fn runtime_backend(requested: SemanticRuntimeBackendArg) -> SemanticRuntimeBackend {
    match requested {
        SemanticRuntimeBackendArg::Cpu => SemanticRuntimeBackend::Cpu,
        SemanticRuntimeBackendArg::Cuda => SemanticRuntimeBackend::Cuda,
        SemanticRuntimeBackendArg::WindowsMl => SemanticRuntimeBackend::WindowsMl,
    }
}

/// Credential-free report for one backend. `backend` and `runtime_root`
/// describe the runtime ctx acted on; the nested `runtime` object appears
/// only when a verified install is present. `locally_installable` distinguishes
/// a backend that is merely absent from one this build cannot provision from a
/// local archive at all, so a report of the second never reads as a missing
/// install step. `detected_accelerator` is the host fact, independent of what
/// is installed, and is null when this machine has no accelerator to use.
fn single_runtime_report(
    operation: &str,
    read_only: bool,
    backend: SemanticRuntimeBackend,
    runtime_root: &Path,
    installed: Option<&SemanticRuntimeReport>,
) -> Value {
    let mut report = json!({
        "schema_version": 1,
        "operation": operation,
        "backend": backend.as_str(),
        "detected_accelerator": detected_accelerator_backend().map(SemanticRuntimeBackend::as_str),
        "runtime_root": runtime_root.display().to_string(),
        "installed": installed.is_some(),
        "locally_installable": supported_local_runtime_backends().contains(&backend),
        "read_only": read_only,
    });
    if let Some(installed) = installed {
        report["runtime"] = installed_runtime_value(installed);
    }
    report
}

/// Credential-free report across every backend this build can install locally.
/// Per-backend facts stay in an array so no single `installed`/`runtime` pair
/// has to stand for several backends at once. `detected_accelerator` stays
/// top-level: it describes the host, not any one backend, and is the same fact
/// the single-backend shape reports.
fn every_runtime_report(runtime_root: &Path) -> Result<Value> {
    let mut runtimes = Vec::with_capacity(supported_local_runtime_backends().len());
    for backend in supported_local_runtime_backends() {
        let installed = installed_runtime_report(runtime_root, *backend)?;
        let mut entry = json!({
            "backend": backend.as_str(),
            "installed": installed.is_some(),
        });
        if let Some(installed) = installed.as_ref() {
            entry["runtime"] = installed_runtime_value(installed);
        }
        runtimes.push(entry);
    }
    Ok(json!({
        "schema_version": 1,
        "operation": "runtime_status",
        "runtime_root": runtime_root.display().to_string(),
        "detected_accelerator": detected_accelerator_backend().map(SemanticRuntimeBackend::as_str),
        "read_only": true,
        "runtimes": runtimes,
    }))
}

fn installed_runtime_value(installed: &SemanticRuntimeReport) -> Value {
    json!({
        "backend": installed.backend.as_str(),
        "platform": installed.platform,
        "version": installed.version,
        "root": installed.root.display().to_string(),
        "library": installed.library.display().to_string(),
        "archive_sha256": installed.archive_sha256,
        "manager": installed.manager,
        "metadata_trust": installed.metadata_trust,
        "files": installed.files,
        "identity": installed.identity,
    })
}

fn semantic_mutation_requires_daemon_restart(
    previous_executor_was_http: bool,
    selected_executor_is_http: bool,
    explicit_executor_selection: bool,
) -> bool {
    previous_executor_was_http || selected_executor_is_http || explicit_executor_selection
}

fn set_semantic_executor_and_enable(
    data_root: &Path,
    config: &mut config::AppConfig,
    executor: &str,
) -> Result<()> {
    crate::semantic::rebind_embedding_auth_for_explicit_selection(executor);
    let accepted = if executor == "builtin" {
        ctx_daemon_cli::SemanticEmbeddingExecutorConfig::builtin()
    } else {
        ctx_daemon_cli::SemanticEmbeddingExecutorConfig::discover_http(
            executor,
            ctx_daemon_cli::semantic_embedding_executor_auth_from_environment()?,
        )?
    };
    config::set_semantic_search_enabled_with_executor(data_root, &accepted)?;
    reload_and_validate_semantic_policy(data_root, config, true)?;
    // `--executor` is the explicit authority to bind the inherited token to a
    // newly selected remote endpoint. Ordinary config loads preserve an
    // existing independent binding and therefore fail closed on mismatch.
    crate::semantic::rebind_embedding_auth_endpoint(config);
    Ok(())
}

pub(crate) fn set_semantic_policy(
    data_root: &Path,
    config: &mut config::AppConfig,
    enabled: bool,
) -> Result<()> {
    CliHistoryConfigAdapter::new(data_root, config).set_semantic_search_enabled(enabled)?;
    reload_and_validate_semantic_policy(data_root, config, enabled)
}

fn reload_and_validate_semantic_policy(
    data_root: &Path,
    config: &mut config::AppConfig,
    enabled: bool,
) -> Result<()> {
    *config = config::AppConfig::load(data_root)?;
    crate::semantic::bind_embedding_auth_endpoint(config);
    if config.semantic_search_enabled() != enabled {
        if enabled {
            bail!(
                "semantic search was enabled in config, but an active process override keeps it disabled; unset CTX_SEARCH_SEMANTIC or set it to true"
            );
        }
        bail!(
            "semantic search was disabled in config, but an active process override keeps it enabled; unset CTX_SEARCH_SEMANTIC or set it to false"
        );
    }
    Ok(())
}

fn semantic_report(
    data_root: &Path,
    config: &config::AppConfig,
    operation: &str,
    read_only: bool,
) -> Result<Value> {
    let source = crate::semantic::source_epoch_status_report(data_root, config)?;
    let semantic = &source.report["semantic"];
    let daemon = &source.report["daemon"];
    let daemon_semantic = daemon
        .get("jobs")
        .and_then(|jobs| jobs.get("semantic_index"));
    let (status, reason) = semantic_lifecycle_state(semantic, daemon, daemon_semantic, config);
    let executor = config.semantic_embedding_executor();
    let executor_scope = executor.scope();
    let token_present =
        std::env::var_os(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV).is_some();
    let token_bound_to_selected_endpoint = token_present
        && executor.http_endpoint().is_some_and(|endpoint| {
            std::env::var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV)
                .ok()
                .and_then(|binding| {
                    match executor.external_space() {
                        Some(space) => ctx_daemon_cli::SemanticEmbeddingExecutorConfig::http(
                            binding,
                            space.clone(),
                        ),
                        None => ctx_daemon_cli::SemanticEmbeddingExecutorConfig::legacy_fixed_http(
                            binding,
                        ),
                    }
                    .ok()
                })
                .and_then(|binding| binding.http_endpoint().map(str::to_owned))
                .is_some_and(|binding| binding == endpoint)
        });
    let reported_space = executor.external_space();
    let mut report = compact_json(json!({
        "schema_version": 1,
        "operation": operation,
        "enabled": semantic.get("enabled"),
        "status": status,
        "reason": reason,
        "config_source": config.semantic_search_source(),
        "indexing": {
            "mode": config.indexing.mode.as_str(),
        },
        "projection": semantic.get("flat_f32"),
        "catch_up": semantic.get("catch_up"),
        "daemon": {
            "status": daemon.get("status"),
            "running": daemon.get("running"),
            "semantic_index": daemon_semantic,
        },
        "executor": {
            "kind": executor.kind().as_str(),
            "protocol_schema_version": executor.http_protocol_schema_version(),
            "endpoint": executor.http_endpoint(),
            "space_id": reported_space.map(|space| space.space_id()),
            "dimensions": reported_space.map(|space| space.dimensions()),
            "scope": executor_scope.as_str(),
            "content_leaves_machine": executor_scope.content_leaves_machine(),
            "authentication": {
                "token_environment": ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV,
                "token_present_in_current_process": token_present,
                "token_bound_to_selected_endpoint_in_current_process":
                    token_bound_to_selected_endpoint,
            },
        },
        "builtin_throttling": {
            "configured": config.semantic_builtin_throttling_configured(),
            "effective": config.semantic_builtin_throttling_effective(),
            "config_source": config.semantic_builtin_throttling_source(),
            "reason": config.semantic_builtin_throttling_reason(),
        },
        // An external loopback process can forward content after ctx's first
        // hop, so only the in-process builtin can truthfully claim local-only.
        "local_only": executor.http_endpoint().is_none(),
        "read_only": read_only,
    }));
    if config.semantic_builtin_throttling_effective().is_none() {
        report["builtin_throttling"]["effective"] = Value::Null;
    }
    Ok(report)
}

fn semantic_lifecycle_state(
    semantic: &Value,
    daemon: &Value,
    daemon_semantic: Option<&Value>,
    config: &config::AppConfig,
) -> (Value, Value) {
    let enabled = semantic
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let daemon_still_semantic = daemon_semantic.is_some_and(|job| {
        [
            "semantic_enabled",
            "runtime_active",
            "configuration_pending",
        ]
        .into_iter()
        .any(|field| job.get(field).and_then(Value::as_bool).unwrap_or(false))
    });
    if !enabled && daemon_still_semantic {
        return (json!("disabling"), json!("daemon_config_reload_pending"));
    }
    if enabled {
        let daemon_job_status = daemon_semantic
            .and_then(|job| job.get("status"))
            .and_then(Value::as_str);
        if matches!(daemon_job_status, Some("failed" | "unavailable")) {
            let reason = daemon_semantic
                .and_then(|job| job.get("reason"))
                .cloned()
                .unwrap_or_else(|| json!("daemon_semantic_job_failed"));
            return (json!("failed"), reason);
        }
        // The aggregated job status stays `pending` while the daemon runs with
        // semantic enabled but no active runtime, so a persisted run error is
        // the only evidence that the last iteration failed. Reporting `pending`
        // here hid actionable model, runtime, and provisioning failures behind
        // ordinary background progress.
        if daemon_semantic_run_error(daemon_semantic).is_some() {
            let reason = daemon_semantic
                .and_then(|job| job.get("last_run_reason"))
                .filter(|reason| reason.as_str().is_some_and(|reason| !reason.is_empty()))
                .or_else(|| daemon_semantic.and_then(|job| job.get("reason")))
                .cloned()
                .unwrap_or_else(|| json!("daemon_semantic_job_failed"));
            return (json!("failed"), reason);
        }
        let source_pending = semantic.get("status").and_then(Value::as_str) == Some("pending");
        let daemon_running = daemon
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if source_pending && config.automatic_indexing_enabled() && !daemon_running {
            return (json!("unavailable"), json!("daemon_not_running"));
        }
    }
    (
        semantic.get("status").cloned().unwrap_or(Value::Null),
        semantic.get("reason").cloned().unwrap_or(Value::Null),
    )
}

/// Persisted error text from the last semantic job iteration. Successful runs
/// and resource deferrals omit it, so a non-empty value means the last run
/// failed. This matches how `ctx daemon status` classifies job failure.
fn daemon_semantic_run_error(job: Option<&Value>) -> Option<&str> {
    job.and_then(|job| job.get("last_error"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|error| !error.is_empty())
}

fn render_report(report: Value, json: bool, quiet: bool, ui: &mut Ui) -> Result<()> {
    if json {
        print_json(report)
    } else if !quiet {
        ui.write_stdout(&render_semantic_status(ui.stdout_context(), &report))?;
        Ok(())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn external_executor(
        endpoint: &str,
        space_id: &str,
        dimensions: usize,
    ) -> ctx_daemon_cli::SemanticEmbeddingExecutorConfig {
        ctx_daemon_cli::SemanticEmbeddingExecutorConfig::http(
            endpoint,
            ctx_daemon_cli::ExternalSemanticSpace::new(space_id, dimensions).unwrap(),
        )
        .unwrap()
    }

    struct TestEnvRestore {
        name: &'static str,
        value: Option<std::ffi::OsString>,
    }

    impl TestEnvRestore {
        fn capture(name: &'static str) -> Self {
            Self {
                name,
                value: std::env::var_os(name),
            }
        }
    }

    impl Drop for TestEnvRestore {
        fn drop(&mut self) {
            match self.value.take() {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn lifecycle_surfaces_daemon_semantic_failure_reason() {
        let mut config = config::AppConfig::default();
        config.search.semantic = Some(true);
        let semantic = json!({"enabled": true, "status": "pending"});
        let daemon = json!({"running": true});
        let job = json!({"status": "failed", "reason": "model_checksum_mismatch"});

        let (status, reason) = semantic_lifecycle_state(&semantic, &daemon, Some(&job), &config);

        assert_eq!(status, "failed");
        assert_eq!(reason, "model_checksum_mismatch");
    }

    #[test]
    fn lifecycle_reports_a_persisted_model_load_failure_instead_of_pending() {
        let mut config = config::AppConfig::default();
        config.search.semantic = Some(true);
        let semantic = json!({"enabled": true, "status": "pending"});
        let daemon = json!({"running": true});
        // The runtime never activated, so the aggregated job keeps reporting
        // pending while the persisted iteration result holds the real failure.
        let job = json!({
            "status": "pending",
            "reason": "semantic_runtime_inactive",
            "last_run_status": "skipped",
            "last_run_reason": "model_load_failed",
            "last_error": "no ONNX Runtime dynamic library candidates were found for linux-x64",
            "failure_class": "retryable",
        });

        let (status, reason) = semantic_lifecycle_state(&semantic, &daemon, Some(&job), &config);

        assert_eq!(status, "failed");
        assert_eq!(reason, "model_load_failed");
    }

    #[test]
    fn lifecycle_keeps_resource_deferred_work_pending() {
        let mut config = config::AppConfig::default();
        config.search.semantic = Some(true);
        let semantic = json!({"enabled": true, "status": "pending", "reason": "flat_f32_projection_missing"});
        let daemon = json!({"running": true});
        // Deferrals persist no error text, so they remain ordinary progress.
        let job = json!({
            "status": "pending",
            "reason": "semantic_runtime_inactive",
            "last_run_status": "resource_deferred",
            "last_run_reason": "memory_pressure",
            "failure_class": "resource_pressure",
        });

        let (status, reason) = semantic_lifecycle_state(&semantic, &daemon, Some(&job), &config);

        assert_eq!(status, "pending");
        assert_eq!(reason, "flat_f32_projection_missing");
    }

    #[test]
    fn executor_and_credential_boundary_mutations_require_daemon_restart() {
        assert!(!semantic_mutation_requires_daemon_restart(
            false, false, false
        ));
        assert!(semantic_mutation_requires_daemon_restart(
            false, true, false
        ));
        assert!(semantic_mutation_requires_daemon_restart(
            true, false, false
        ));
        assert!(semantic_mutation_requires_daemon_restart(true, true, false));
        assert!(semantic_mutation_requires_daemon_restart(
            false, false, true
        ));
    }

    #[test]
    fn semantic_scope_treats_only_builtin_and_exact_loopback_ips_as_local() {
        let builtin = ctx_daemon_cli::SemanticEmbeddingExecutorConfig::builtin();
        let ipv4 = external_executor("http://127.0.0.1:8080/", "space-v1", 384);
        let ipv6 = external_executor("http://[::1]:8080/", "space-v1", 384);
        let remote = external_executor("https://embed.example.test/", "space-v1", 384);
        assert!(!builtin.scope().content_leaves_machine());
        assert!(!ipv4.scope().content_leaves_machine());
        assert!(!ipv6.scope().content_leaves_machine());
        assert!(remote.scope().content_leaves_machine());
        assert!(external_executor("https://localhost/", "space-v1", 384)
            .scope()
            .content_leaves_machine());
    }

    #[test]
    fn status_json_is_offline_redacted_and_uses_canonical_auth_binding() {
        let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _token = TestEnvRestore::capture(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV);
        let _binding =
            TestEnvRestore::capture(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV);
        std::env::remove_var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV);
        std::env::remove_var(ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV);
        let temp = tempfile::tempdir().unwrap();
        let mut config = config::AppConfig::default();
        config.search.semantic = Some(true);

        let builtin = semantic_report(temp.path(), &config, "status", true).unwrap();
        assert_eq!(builtin["executor"]["kind"], "builtin");
        assert_eq!(builtin["executor"]["scope"], "builtin");
        assert_eq!(builtin["local_only"], true);
        assert_eq!(builtin["read_only"], true);
        assert_eq!(
            builtin["builtin_throttling"],
            json!({
                "configured": true,
                "effective": true,
                "config_source": "default",
            })
        );

        config.semantic.executor = external_executor("http://127.0.0.1:9", "loopback-v1", 128);
        std::env::set_var(
            ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV,
            "loopback-secret",
        );
        let loopback = semantic_report(temp.path(), &config, "status", true).unwrap();
        assert_eq!(loopback["executor"]["scope"], "loopback");
        assert_eq!(loopback["executor"]["content_leaves_machine"], false);
        assert_eq!(loopback["local_only"], false);
        assert_eq!(loopback["builtin_throttling"]["configured"], true);
        assert_eq!(loopback["builtin_throttling"]["effective"], Value::Null);
        assert_eq!(loopback["builtin_throttling"]["config_source"], "default");
        assert_eq!(
            loopback["builtin_throttling"]["reason"],
            "external_executor"
        );
        assert_eq!(
            loopback["executor"]["authentication"]
                ["token_bound_to_selected_endpoint_in_current_process"],
            false
        );

        config.semantic.executor = external_executor(
            "https://embed.example.test/base",
            "acme/multilingual-v2",
            768,
        );
        std::env::set_var(
            ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV,
            "remote-secret",
        );
        std::env::set_var(
            ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
            "https://embed.example.test/base",
        );
        let remote = semantic_report(temp.path(), &config, "status", true).unwrap();
        assert_eq!(remote["executor"]["scope"], "remote");
        assert_eq!(remote["executor"]["content_leaves_machine"], true);
        assert_eq!(remote["local_only"], false);
        assert_eq!(remote["read_only"], true);
        assert_eq!(remote["executor"]["space_id"], "acme/multilingual-v2");
        assert_eq!(remote["executor"]["dimensions"], 768);
        assert_eq!(
            remote["executor"]["authentication"]
                ["token_bound_to_selected_endpoint_in_current_process"],
            true
        );
        let encoded = serde_json::to_string(&remote).unwrap();
        assert!(!encoded.contains("remote-secret"));
        assert!(!encoded.contains("loopback-secret"));

        let enable = semantic_report(temp.path(), &config, "enable", false).unwrap();
        assert_eq!(enable["executor"]["space_id"], "acme/multilingual-v2");
        assert_eq!(enable["executor"]["dimensions"], 768);
    }

    #[test]
    fn semantic_status_reports_explicitly_disabled_builtin_throttling() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(config::CONFIG_FILE),
            "[semantic]\nbuiltin_throttling = false\n",
        )
        .unwrap();
        let config = config::AppConfig::load(temp.path()).unwrap();

        let report = semantic_report(temp.path(), &config, "status", true).unwrap();

        assert_eq!(
            report["builtin_throttling"],
            json!({
                "configured": false,
                "effective": false,
                "config_source": "config",
            })
        );
    }

    #[test]
    fn bare_semantic_enable_preserves_explicitly_disabled_builtin_throttling() {
        let _lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _semantic_override = TestEnvRestore::capture("CTX_SEARCH_SEMANTIC");
        std::env::remove_var("CTX_SEARCH_SEMANTIC");
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(config::CONFIG_FILE),
            "[semantic]\nbuiltin_throttling = false\n",
        )
        .unwrap();
        let mut config = config::AppConfig::load(temp.path()).unwrap();

        set_semantic_policy(temp.path(), &mut config, true).unwrap();

        assert!(config.semantic_search_enabled());
        assert!(!config.semantic_builtin_throttling_configured());
        assert_eq!(config.semantic_builtin_throttling_effective(), Some(false));
        assert!(
            std::fs::read_to_string(temp.path().join(config::CONFIG_FILE))
                .unwrap()
                .contains("builtin_throttling = false")
        );
    }

    fn runtime_report_fixture(
        backend: SemanticRuntimeBackend,
        platform: &str,
    ) -> SemanticRuntimeReport {
        SemanticRuntimeReport {
            backend,
            platform: platform.to_owned(),
            version: "1.27.0".to_owned(),
            root: PathBuf::from(format!("/data/runtime/onnxruntime/1.27.0/{platform}")),
            library: PathBuf::from(format!(
                "/data/runtime/onnxruntime/1.27.0/{platform}/lib/libonnxruntime.so"
            )),
            archive_sha256: "a".repeat(64),
            manager: "ctx-local-operator",
            metadata_trust: "operator-pinned-digest",
            files: 5,
            identity: format!("onnxruntime|platform={platform}"),
        }
    }

    #[test]
    fn runtime_install_report_publishes_the_verified_runtime_facts() {
        let installed = runtime_report_fixture(SemanticRuntimeBackend::Cpu, "linux-x64");

        let report = single_runtime_report(
            "runtime_install",
            false,
            SemanticRuntimeBackend::Cpu,
            Path::new("/data/runtime"),
            Some(&installed),
        );

        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["operation"], "runtime_install");
        assert_eq!(report["backend"], "cpu");
        assert_eq!(report["runtime_root"], "/data/runtime");
        assert_eq!(report["installed"], true);
        assert_eq!(report["read_only"], false);
        assert_eq!(report["runtime"]["backend"], "cpu");
        assert_eq!(report["runtime"]["platform"], "linux-x64");
        assert_eq!(report["runtime"]["version"], "1.27.0");
        assert_eq!(
            report["runtime"]["root"],
            "/data/runtime/onnxruntime/1.27.0/linux-x64"
        );
        assert_eq!(
            report["runtime"]["library"],
            "/data/runtime/onnxruntime/1.27.0/linux-x64/lib/libonnxruntime.so"
        );
        assert_eq!(report["runtime"]["archive_sha256"], "a".repeat(64));
        assert_eq!(report["runtime"]["manager"], "ctx-local-operator");
        assert_eq!(
            report["runtime"]["metadata_trust"],
            "operator-pinned-digest"
        );
        assert_eq!(report["runtime"]["files"], 5);
        assert_eq!(
            report["runtime"]["identity"],
            "onnxruntime|platform=linux-x64"
        );
    }

    #[test]
    fn accelerator_install_report_names_the_accelerator_backend() {
        let installed = runtime_report_fixture(SemanticRuntimeBackend::Cuda, "linux-x64-cuda12");

        let report = single_runtime_report(
            "runtime_install",
            false,
            SemanticRuntimeBackend::Cuda,
            Path::new("/data/runtime"),
            Some(&installed),
        );

        assert_eq!(report["backend"], "cuda");
        assert_eq!(report["runtime"]["platform"], "linux-x64-cuda12");
    }

    #[test]
    fn runtime_status_report_omits_the_runtime_object_when_nothing_is_installed() {
        let report = single_runtime_report(
            "runtime_status",
            true,
            SemanticRuntimeBackend::Cpu,
            Path::new("/data/runtime"),
            None,
        );

        assert_eq!(report["operation"], "runtime_status");
        assert_eq!(report["backend"], "cpu");
        assert_eq!(report["installed"], false);
        assert_eq!(report["read_only"], true);
        assert!(report.get("runtime").is_none(), "{report}");
    }

    #[test]
    fn status_marks_a_backend_no_build_can_install_from_a_local_archive() {
        // The Windows ML sidecar is a zip, so the hosted installer owns it on
        // every platform and status must not read as a missing install step.
        let report = single_runtime_report(
            "runtime_status",
            true,
            SemanticRuntimeBackend::WindowsMl,
            Path::new("/data/runtime"),
            None,
        );

        assert_eq!(report["backend"], "windowsml");
        assert_eq!(report["installed"], false);
        assert_eq!(report["locally_installable"], false);
    }

    #[test]
    fn every_backend_status_reports_one_entry_per_locally_installable_backend() {
        let temp = tempfile::tempdir().unwrap();

        let report = every_runtime_report(temp.path()).unwrap();

        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["operation"], "runtime_status");
        assert_eq!(report["runtime_root"], temp.path().display().to_string());
        assert_eq!(report["read_only"], true);
        // One `installed`/`backend` pair cannot stand for several backends, so
        // multi-backend status must not publish an ambiguous one.
        assert!(report.get("installed").is_none(), "{report}");
        assert!(report.get("backend").is_none(), "{report}");
        let runtimes = report["runtimes"].as_array().unwrap();
        assert_eq!(
            runtimes
                .iter()
                .map(|entry| entry["backend"].as_str().unwrap())
                .collect::<Vec<_>>(),
            supported_local_runtime_backends()
                .iter()
                .map(|backend| backend.as_str())
                .collect::<Vec<_>>()
        );
        for entry in runtimes {
            assert_eq!(entry["installed"], false, "{entry}");
            assert!(entry.get("runtime").is_none(), "{entry}");
        }
    }

    #[test]
    fn both_status_shapes_publish_the_hosts_detected_accelerator() {
        let temp = tempfile::tempdir().unwrap();

        let every = every_runtime_report(temp.path()).unwrap();
        let single = single_runtime_report(
            "runtime_status",
            true,
            SemanticRuntimeBackend::Cpu,
            temp.path(),
            None,
        );

        // The detected accelerator is a host fact, so both shapes must carry
        // the same answer; a status that reported it in only one shape would
        // make the guidance depend on which command was typed.
        let detected = &every["detected_accelerator"];
        assert_eq!(&single["detected_accelerator"], detected, "{every}");
        assert!(
            detected.is_null() || detected.as_str().is_some_and(|backend| backend != "cpu"),
            "the CPU runtime is not an accelerator: {every}"
        );
    }

    #[test]
    fn missing_checksum_sidecar_refuses_to_install_unverified_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("ctx-onnxruntime-linux-x64-cuda12.tar.zst");
        std::fs::write(&archive, b"archive").unwrap();

        let error = expected_archive_sha256(None, &archive).unwrap_err();

        let error = error.to_string();
        assert!(error.contains("--sha256"), "{error}");
        assert!(
            error.contains("ctx-onnxruntime-linux-x64-cuda12.tar.zst.sha256"),
            "{error}"
        );
    }

    #[test]
    fn checksum_sidecar_supplies_the_expected_digest() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("runtime.tar.zst");
        std::fs::write(&archive, b"archive").unwrap();
        let digest = "B".repeat(64);
        std::fs::write(
            temp.path().join("runtime.tar.zst.sha256"),
            format!("{digest}  runtime.tar.zst\n"),
        )
        .unwrap();

        assert_eq!(
            expected_archive_sha256(None, &archive).unwrap(),
            "b".repeat(64)
        );
        assert_eq!(
            expected_archive_sha256(Some("  C0FFEE  "), &archive).unwrap(),
            "c0ffee"
        );
    }

    #[test]
    fn runtime_root_defaults_to_the_data_root_and_rejects_a_relative_override() {
        let _override = TestEnvRestore::capture("CTX_RUNTIME_DIR");
        std::env::remove_var("CTX_RUNTIME_DIR");
        assert_eq!(
            selected_runtime_root(Path::new("/data")).unwrap(),
            PathBuf::from("/data/runtime")
        );

        std::env::set_var("CTX_RUNTIME_DIR", "relative/runtime");
        let error = selected_runtime_root(Path::new("/data"))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "CTX_RUNTIME_DIR must be an absolute path");

        std::env::set_var("CTX_RUNTIME_DIR", " /tmp/ctx-runtime");
        let error = selected_runtime_root(Path::new("/data"))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "CTX_RUNTIME_DIR must not be empty or whitespace-padded"
        );

        std::env::set_var("CTX_RUNTIME_DIR", "/tmp/ctx-runtime");
        assert_eq!(
            selected_runtime_root(Path::new("/data")).unwrap(),
            PathBuf::from("/tmp/ctx-runtime")
        );
    }
}
