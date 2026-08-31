use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use ctx_cli_presentation::commands::{
    render_semantic_disabled, render_semantic_status, SemanticArgs, SemanticCommand,
};
use ctx_history_cli::HistoryConfigPort;
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
    }
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
    Ok(compact_json(json!({
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
        // An external loopback process can forward content after ctx's first
        // hop, so only the in-process builtin can truthfully claim local-only.
        "local_only": executor.http_endpoint().is_none(),
        "read_only": read_only,
    })))
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

        config.semantic.executor = external_executor("http://127.0.0.1:9", "loopback-v1", 128);
        std::env::set_var(
            ctx_daemon_cli::SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV,
            "loopback-secret",
        );
        let loopback = semantic_report(temp.path(), &config, "status", true).unwrap();
        assert_eq!(loopback["executor"]["scope"], "loopback");
        assert_eq!(loopback["executor"]["content_leaves_machine"], false);
        assert_eq!(loopback["local_only"], false);
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
}
