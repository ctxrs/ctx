use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Result;
use ctx_observability::logs::{
    default_ctx_logs_dir, prepare_daemon_log_file_for_today_sync, spawn_daemon_log_maintenance,
    DaemonLogConfig,
};
use tracing_subscriber::Layer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::cli::Commands;

mod conditional_writer;
mod heap_profiler;

use self::conditional_writer::ConditionalMakeWriter;
use self::heap_profiler::spawn_daemon_heap_profiler;

#[cfg(feature = "daemon-heap-prof")]
fn env_bool(key: &str) -> Option<bool> {
    std::env::var(key)
        .ok()
        .as_deref()
        .and_then(ctx_core::boolish::parse_boolish)
}

#[cfg(feature = "daemon-heap-prof")]
fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
}

fn env_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn init_logging_for_command(
    command: &Commands,
) -> Result<Option<tracing_appender::non_blocking::WorkerGuard>> {
    let logs_dir: Option<std::path::PathBuf> = match command {
        Commands::Serve { data_dir, .. } => {
            let data_root = if let Some(p) = data_dir {
                std::path::PathBuf::from(p)
            } else {
                return Ok(Some(init_daemon_file_logging(default_ctx_logs_dir()?)?));
            };
            Some(data_root.join("logs"))
        }
        Commands::AgentWork(_) | Commands::Init { .. } | Commands::SelfUpdate { .. } => None,
    };

    let Some(logs_dir) = logs_dir else {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();
        return Ok(None);
    };

    Ok(Some(init_daemon_file_logging(logs_dir)?))
}

fn init_daemon_file_logging(
    logs_dir: std::path::PathBuf,
) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let daemon_log_config = DaemonLogConfig::from_env();
    let file_blocked = Arc::new(AtomicBool::new(false));
    prepare_daemon_log_file_for_today_sync(&logs_dir);
    let appender = tracing_appender::rolling::daily(&logs_dir, "daemon.log");
    let (file_writer, file_guard) = tracing_appender::non_blocking(appender);
    let file_writer = ConditionalMakeWriter {
        inner: file_writer,
        blocked: Arc::clone(&file_blocked),
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(file_writer);

    let stdout_layer = if daemon_log_config.stdout_enabled {
        let stdout_filter = env_string("CTX_DAEMON_LOG_STDOUT_FILTER")
            .and_then(|value| tracing_subscriber::EnvFilter::try_new(value).ok())
            .unwrap_or_else(|| tracing_subscriber::EnvFilter::new("error"));
        Some(
            tracing_subscriber::fmt::layer()
                .with_ansi(true)
                .with_filter(stdout_filter),
        )
    } else {
        None
    };

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stdout_layer)
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    spawn_daemon_log_maintenance(logs_dir.clone(), daemon_log_config, file_blocked);
    spawn_daemon_heap_profiler(&logs_dir);
    Ok(file_guard)
}
