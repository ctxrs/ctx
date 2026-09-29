//! Offline copy and activation of the managed history root.
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use ctx_history_platform::{
    managed_data_root,
    managed_root::{self, ManagedRootMove},
    platform_security,
};

use crate::output::JsonOutputFormat;

mod copy;

#[derive(Debug, Args)]
pub(crate) struct DataRootArgs {
    #[command(subcommand)]
    command: DataRootCommand,
}

#[derive(Debug, Subcommand)]
enum DataRootCommand {
    /// Print the managed data root without creating or opening history storage.
    Show {
        #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
        format: JsonOutputFormat,
    },
    /// Copy all managed data to an empty directory and activate it; retain the source.
    #[command(
        after_help = "Upgrade ctx before moving its managed root. Close long-running ctx and MCP clients. The destination must support private permissions and file locking. A failed copy leaves the source active; remove the incomplete destination and retry. Automatic indexing resumes at the new root; if restarting fails, run `ctx index mode auto`. Older ctx versions do not understand the locator."
    )]
    Move {
        #[arg(
            long,
            value_name = "PATH",
            help = "Absolute destination on a mounted local filesystem (absent or empty)"
        )]
        to: PathBuf,
    },
}

impl DataRootArgs {
    pub(crate) fn json_output(&self) -> bool {
        matches!(
            self.command,
            DataRootCommand::Show {
                format: JsonOutputFormat::Json
            }
        )
    }
}

pub(crate) fn run(args: DataRootArgs, custom_root: Option<PathBuf>) -> Result<()> {
    match args.command {
        DataRootCommand::Show { format } => {
            let path = managed_data_root()?;
            crate::output::with_stdout_writer(|out| -> Result<()> {
                if format.is_json() {
                    serde_json::to_writer(
                        &mut *out,
                        &serde_json::json!({ "schema_version": 1, "path": path }),
                    )?;
                    writeln!(out)?;
                } else {
                    writeln!(out, "{}", path.display())?;
                }
                Ok(())
            })
        }
        DataRootCommand::Move { to } => {
            if custom_root.is_some() {
                bail!("data-root move relocates the managed root; unset CTX_DATA_ROOT and omit --data-root");
            }
            move_root(&to)
        }
    }
}

fn move_root(destination: &Path) -> Result<()> {
    let mut admission = ManagedRootMove::acquire()?;
    let source = managed_data_root()?;
    copy::validate_destination(&source, destination)?;
    let config = ctx_app_config::AppConfig::load_read_only(&source)?;
    validate_provider_overlap(&source, destination, &config)?;
    // External installers and replacement helpers share this installation lock.
    // Source-only binaries have no managed installation mutation to serialize.
    let installation = ctx_upgrade_engine::managed_install_executable()?;
    let _installation_lock = installation
        .as_deref()
        .map(|executable| {
            ctx_upgrade_engine::try_acquire_managed_installation_mutation(executable)?.context(
                "ctx installation is being upgraded or uninstalled; finish it before moving data",
            )
        })
        .transpose()?;
    if ctx_upgrade_engine::installation_hosted_uninstall_is_active()? {
        bail!("installation removal is active; finish it before moving data");
    }
    crate::semantic::initialize()?;
    let (quiescence, supervisor) =
        ctx_daemon_cli::quiesce_managed_root_move(&source, &admission, |supervisor| {
            validate_saved_provider_overlap(
                &source,
                destination,
                &config,
                supervisor.provider_environment(),
            )
        })?;
    admission.drain().context("source remains active and background maintenance is stopped; close ctx/MCP clients and retry the move, or run `ctx index mode auto` to resume")?;
    // Re-read after all foreground users have drained. Configuration and
    // provider roots may have changed while the move was stopping daemons.
    let config = ctx_app_config::AppConfig::load_read_only(&source)?;
    validate_provider_overlap(&source, destination, &config)?;
    validate_saved_provider_overlap(
        &source,
        destination,
        &config,
        supervisor.provider_environment(),
    )?;
    let install_id = crate::identity::installation_id(&source)?;
    copy::copy_root(&source, destination).with_context(|| format!(
        "managed root remains {}; remove the incomplete destination {} and retry `ctx data-root move`; run `ctx index mode auto` to resume background maintenance at the source",
        source.display(), destination.display()
    ))?;
    supervisor.persist(destination)?;
    admission.activate(destination, &install_id).with_context(|| format!(
        "could not finish managed-root activation; use `ctx data-root show` to check the active root before cleanup; both {} and {} have been retained",
        source.display(), destination.display()
    ))?;
    drop(quiescence);
    drop(_installation_lock);
    drop(admission);
    let _active_use = ctx_history_platform::managed_root::ManagedRootUse::acquire()?;
    if managed_data_root()? != destination {
        bail!("another relocation changed the managed root before background maintenance resumed; run `ctx index mode auto`");
    }
    if config.automatic_indexing_enabled() {
        ctx_daemon_cli::resume_managed_root_supervisor(destination, &supervisor).with_context(|| format!(
            "managed data root is now {}; both copies are retained, but background maintenance did not resume; run `ctx index mode auto`",
            destination.display()
        ))?;
    }
    crate::output::with_stdout_writer(|out| -> Result<()> {
        writeln!(out, "Managed data root: {}", destination.display())?;
        writeln!(
            out,
            "Source retained at {}. After verifying the new root, you may remove that retained copy.",
            source.display()
        )?;
        Ok(())
    })
}

fn validate_saved_provider_overlap<'a>(
    source: &Path,
    destination: &Path,
    config: &ctx_app_config::AppConfig,
    environment: impl Iterator<Item = (&'static str, &'a str)>,
) -> Result<()> {
    if let Some(home) = dirs_home() {
        let mut context = ctx_history_capture::DiscoveryContext::from_process(home)
            .with_data_root(source)
            .with_configured_provider_roots(config.provider_root_definitions())
            .with_automatic_provider_discovery(config.automatic_source_discovery_enabled());
        for (name, value) in environment {
            context = context.with_env(name, value);
        }
        for provider in
            ctx_history_capture::discover_provider_sources_with_context(&context).sources
        {
            platform_security::validate_provider_source_outside_data_root(
                destination,
                &provider.path,
            )?;
        }
    }
    Ok(())
}

fn validate_provider_overlap(
    source: &Path,
    destination: &Path,
    config: &ctx_app_config::AppConfig,
) -> Result<()> {
    let roots = config.provider_root_definitions();
    for root in &roots {
        platform_security::validate_provider_source_outside_data_root(destination, &root.path)?;
    }
    let home = dirs_home();
    let discovery = ctx_history_cli::discovered_sources_report_with_data_root_and_provider_roots(
        home.as_deref(),
        source,
        config.automatic_source_discovery_enabled(),
        &roots,
    );
    for provider in discovery.sources {
        platform_security::validate_provider_source_outside_data_root(destination, &provider.path)
            .with_context(|| {
                format!(
                    "destination overlaps provider source {}",
                    provider.path.display()
                )
            })?;
    }
    for plugin in ctx_history_ingest_application::discover_history_source_plugins(source, &[])? {
        if let Some(path) = plugin.source_path {
            platform_security::validate_provider_source_outside_data_root(destination, &path)?;
            platform_security::validate_provider_source_outside_data_root(source, &path)?;
        }
    }
    Ok(())
}

fn dirs_home() -> Option<PathBuf> {
    // Use the same platform home authority, without introducing an XDG override.
    managed_root::managed_control_root()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

#[cfg(test)]
mod tests;
