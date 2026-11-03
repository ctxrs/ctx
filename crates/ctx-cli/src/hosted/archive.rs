use std::path::{Path, PathBuf};

pub(super) use ctx_cli_presentation::commands::archive_export::digest;
use ctx_cli_presentation::commands::archive_export::{self, ExportArgs};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use ctx_history_archive::{ImportBinding, RestoreOptions};
use serde_json::json;

use crate::{output::JsonOutputFormat, ui::Ui};

#[derive(Debug, Args)]
pub(crate) struct ArchiveArgs {
    #[arg(long, value_enum, default_value = "text", global = true)]
    pub(crate) format: JsonOutputFormat,
    #[command(subcommand)]
    command: ArchiveCommand,
}

impl ArchiveArgs {
    pub(super) fn telemetry_operation(
        &self,
    ) -> Option<ctx_client_observability::analytics::HostedOperationV1> {
        use ctx_client_observability::analytics::HostedOperationV1 as Operation;
        Some(match self.command {
            ArchiveCommand::Export(_) => Operation::ArchiveExport,
            ArchiveCommand::Restore { .. } => Operation::ArchiveRestore,
            _ => return None,
        })
    }
}

#[derive(Debug, Subcommand)]
enum ArchiveCommand {
    /// Save a coherent snapshot of retained normalized history; no provider files or credentials.
    Export(ExportArgs),
    /// Check the complete inventory, normalized records, and content digests.
    Verify { archive: PathBuf },
    /// Restore retained history into a new or archive-owned local data root.
    Restore {
        archive: PathBuf,
        /// JSON object mapping member paths to exact old digests, for explicit corrections.
        #[arg(long)]
        expected_predecessors: Option<PathBuf>,
    },
}

pub(super) fn run(args: &ArchiveArgs, root: Option<&Path>, ui: &mut Ui) -> Result<()> {
    match &args.command {
        ArchiveCommand::Export(options) => {
            let root = super::data_root(root)?;
            let (value, text) = archive_export::run(options, &root, ui.stdout_context())?;
            super::print_result(args.format, value, text, ui)
        }
        ArchiveCommand::Verify { archive } => {
            let manifest = ctx_history_archive::verify(archive)?;
            let text = format!(
                "Verified {} sessions and {} records in {}",
                manifest.members,
                manifest.records,
                archive.display()
            );
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "archive_verify", "archive": archive,
                    "manifest": manifest
                }),
                text,
                ui,
            )
        }
        ArchiveCommand::Restore {
            archive,
            expected_predecessors,
        } => {
            let root = super::data_root(root)?;
            let manifest = ctx_history_archive::verify(archive)?;
            let mut options = RestoreOptions::new(ImportBinding {
                namespace: "personal".to_owned(),
                identity: manifest.identity,
            });
            if let Some(path) = expected_predecessors {
                options.expected_predecessors = serde_json::from_reader(
                    std::fs::File::open(path).context("open expected-predecessor file")?,
                )
                .context("read expected-predecessor mapping")?;
            }
            // Ownership refusal must precede configuration writes. Persist the
            // ordinary indexing/discovery policy before publishing any index.
            let restore_lock = ctx_history_archive::prepare_restore_root(&root)?;
            ctx_app_config::set_daemon_enabled(&root, false)
                .context("disable automatic indexing for the archive root")?;
            ctx_app_config::set_automatic_source_discovery_enabled(&root, false)
                .context("disable automatic provider discovery for the archive root")?;
            drop(restore_lock);
            let receipt = ctx_history_archive::restore(archive, &root, &options)?;
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "archive_restore", "receipt": receipt,
                    "automatic_indexing": false, "automatic_source_discovery": false
                }),
                format!("Restored retained history into {}\nAutomatic indexing and default provider discovery are off for this archive root.", root.display()),
                ui,
            )
        }
    }
}
