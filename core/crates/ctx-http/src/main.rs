use anyhow::Result;
use tracing::warn;

mod agent_work_cli;
mod cli;
mod logging;

use cli::{Cli, Commands};

#[cfg(feature = "daemon-heap-prof")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse_args();
    let open_file_limit_adjustment = match &cli.command {
        Commands::Serve { .. } => Some(
            ctx_resource_utilization::process_limits::ensure_min_open_file_limit(
                ctx_resource_utilization::process_limits::RECOMMENDED_DAEMON_OPEN_FILE_SOFT_LIMIT,
            ),
        ),
        Commands::AgentWork(_) | Commands::Init { .. } | Commands::SelfUpdate { .. } => None,
    };

    let _file_guard = logging::init_logging_for_command(&cli.command)?;

    if let Err(err) = rustls::crypto::aws_lc_rs::default_provider().install_default() {
        warn!("failed to install rustls crypto provider: {err:?}");
    }
    if let Some(result) = open_file_limit_adjustment {
        match result {
            Ok(Some(limit)) => {
                tracing::info!(
                    nofile_soft_before = limit.before.soft,
                    nofile_soft_after = limit.after.soft,
                    nofile_hard = limit.after.hard,
                    nofile_target_soft = limit.target_soft,
                    nofile_changed = limit.changed,
                    "daemon RLIMIT_NOFILE initialized"
                );
            }
            Ok(None) => {}
            Err(err) => {
                eprintln!("warning: failed to raise daemon RLIMIT_NOFILE: {err:#}");
                tracing::warn!("failed to raise daemon RLIMIT_NOFILE: {err:#}");
            }
        }
    }

    match cli.command {
        Commands::AgentWork(command) => {
            agent_work_cli::run(command);
        }
        Commands::Serve { bind, data_dir } => {
            ctx_http::serve(bind, data_dir).await?;
        }
        Commands::Init { root } => {
            ctx_repo_onboarding_service::init_workspace(root).await?;
        }
        Commands::SelfUpdate {
            channel,
            base_url,
            yes,
            check,
        } => {
            let base_url = base_url.unwrap_or_else(ctx_update_service::default_download_base_url);
            let current_version =
                ctx_update_service::current_build_identity(env!("CARGO_PKG_VERSION"))?
                    .exact_version
                    .clone();
            ctx_update_service::self_update_daemon(
                &channel,
                &base_url,
                &current_version,
                yes,
                check,
            )
            .await?;
        }
    }
    Ok(())
}
