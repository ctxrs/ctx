use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use ctx_history_archive::{ArchiveIdentity, ImportBinding, RestoreOptions, Selection};
use serde_json::json;

use crate::{output::JsonOutputFormat, ui::Ui};

#[derive(Debug, Args)]
pub(crate) struct ArchiveArgs {
    #[arg(long, value_enum, default_value = "text", global = true)]
    pub(crate) format: JsonOutputFormat,
    #[command(subcommand)]
    command: ArchiveCommand,
}

#[derive(Debug, Subcommand)]
enum ArchiveCommand {
    /// Save a coherent snapshot of retained normalized history; no provider files or credentials.
    Export {
        /// New output directory for the completed archive.
        #[arg(long)]
        output: PathBuf,
        /// Stable origin for native history; selects the original origin in a restored root.
        #[arg(long)]
        origin: String,
        /// Stable view for native history; selects the original view in a restored root.
        #[arg(long, default_value = "personal")]
        view: String,
        /// Include this full Core source digest (repeat to select more).
        #[arg(long, value_parser = digest)]
        source: Vec<String>,
        /// Include this full Core session digest (repeat to select more).
        #[arg(long, value_parser = digest)]
        session: Vec<String>,
    },
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
        ArchiveCommand::Export {
            output,
            origin,
            view,
            source,
            session,
        } => {
            let root = super::data_root(root)?;
            let exported = ctx_history_archive::export_data_root(
                &root,
                output,
                ArchiveIdentity {
                    origin: origin.clone(),
                    view: view.clone(),
                },
                &Selection {
                    sources: source.iter().cloned().collect(),
                    sessions: session.iter().cloned().collect(),
                },
            )?;
            let mut text = format!(
                "Exported {} sessions and {} records to {}\nOrigin: {}. View: {}. This archive contains one identity.\nRetained normalized history only; native agent resumption is not included.",
                exported.manifest.members,
                exported.manifest.records,
                output.display(),
                exported.manifest.identity.origin,
                exported.manifest.identity.view
            );
            for identity in &exported.excluded_identities {
                text.push_str(&format!("\nNot included: origin {}, view {}. Export it separately with --origin and --view.", identity.origin, identity.view));
            }
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "archive_export", "archive": output,
                    "manifest": exported.manifest, "excluded_identities": exported.excluded_identities
                }),
                text,
                ui,
            )
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

pub(super) fn digest(value: &str) -> std::result::Result<String, String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(value.to_owned())
    } else {
        Err("expected a full 64-character lowercase Core identity digest".to_owned())
    }
}
