use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use clap::{Args, Subcommand, ValueEnum};
use ctx_history_sharing::{
    Collector, Connection, Credentials, Endpoint, RemoteClient, SelectionDecision, SharingStatus,
    SharingStore, TickOutcome,
};
use serde_json::json;

use crate::{output::JsonOutputFormat, ui::Ui};

#[derive(Debug, Args)]
pub(crate) struct RemoteArgs {
    #[arg(long, value_enum, default_value = "text", global = true)]
    pub(crate) format: JsonOutputFormat,
    #[command(subcommand)]
    command: RemoteCommand,
}

impl RemoteArgs {
    pub(super) fn telemetry_operation(
        &self,
    ) -> Option<ctx_client_observability::analytics::HostedOperationV1> {
        use ctx_client_observability::analytics::HostedOperationV1 as Operation;
        Some(match self.command {
            RemoteCommand::Connect { .. } => Operation::RemoteConnect,
            RemoteCommand::Share(_) => Operation::RemoteShare,
            RemoteCommand::Sync { .. } => Operation::RemoteSync,
            _ => return None,
        })
    }
}

#[derive(Debug, Subcommand)]
enum RemoteCommand {
    /// Save a named destination and credentials; does not upload or enable sharing.
    Connect {
        /// Server URL. Paste a one-time invitation at the hidden terminal prompt.
        url: String,
        #[arg(long, default_value = "team")]
        name: String,
        /// Token's selected collection, or an explicit check of an invitation's audience.
        #[arg(long)]
        collection: Option<String>,
        /// Owner-private token file, or '-' for piped stdin.
        #[arg(long, conflicts_with = "enrollment_file")]
        token_file: Option<PathBuf>,
        /// Redeem a short-lived enrollment from a protected file or piped stdin.
        #[arg(long)]
        enrollment_file: Option<PathBuf>,
        /// Disable publishing from this client; keep authorized read/admin access.
        #[arg(long)]
        read_only: bool,
    },
    /// Authorize a reviewed snapshot of selected history; future updates require opt-in.
    Share(ShareArgs),
    /// Capture and send a bounded batch under the saved sharing policy.
    Sync { name: String },
    /// Pause future uploads; --resume continues under the same saved policy.
    Pause {
        name: String,
        #[arg(long)]
        resume: bool,
    },
    /// Inspect connection/policy/backlog; optionally fetch current server status.
    Status {
        name: String,
        /// Contact the server to verify access and stored/searchable coverage.
        #[arg(long)]
        online: bool,
    },
    /// Remove a saved connection and pending uploads; retained server history remains.
    Remove { name: String },
}

#[derive(Debug, Args)]
#[command(
    after_help = "List indexed sources with ctx sources, or register and import a profile first:\n  ctx sources add work --provider codex --root /absolute/profile\n  ctx import --all\n  ctx remote share team --profile-root /absolute/profile --whole-source\nStandalone exact-path imports do not establish a registered profile identity."
)]
struct ShareArgs {
    name: String,
    /// Full Core source digest (repeat to authorize additional sources).
    #[arg(long, required_unless_present = "profile_root", value_parser = super::archive::digest)]
    source: Vec<String>,
    /// Explicit registered provider profile root (repeat for additional roots).
    #[arg(long, required_unless_present = "source")]
    profile_root: Vec<PathBuf>,
    /// Automatic authorizes later coherent revisions; reviewed pins current bytes.
    #[arg(long, value_enum, default_value = "reviewed")]
    mode: Mode,
    /// Include all existing sessions or only sessions created after this baseline.
    #[arg(long, value_enum, default_value = "all")]
    backfill: BackfillArg,
    /// With --mode automatic, authorize future sessions and revisions in this scope.
    #[arg(long)]
    include_future: bool,
    /// Authorize full source content, including unknown/mixed project metadata.
    #[arg(
        long,
        required_unless_present = "work_root",
        conflicts_with = "work_root"
    )]
    whole_source: bool,
    /// Hold sessions with unknown or outside work-directory claims for review.
    #[arg(long, required_unless_present = "whole_source")]
    work_root: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mode {
    Automatic,
    Reviewed,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BackfillArg {
    All,
    None,
}

/// The same named state root is used by remote readers and the sharing worker.
pub(crate) fn store(data_root: &Path, name: &str) -> Result<SharingStore> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("remote name must contain 1..64 ASCII letters, digits, '-' or '_'");
    }
    Ok(SharingStore::new(data_root.join("sharing").join(name)))
}

pub(super) fn run(args: &RemoteArgs, root: Option<&Path>, ui: &mut Ui) -> Result<()> {
    let root = super::data_root(root)?;
    let name = match &args.command {
        RemoteCommand::Connect { name, .. }
        | RemoteCommand::Sync { name }
        | RemoteCommand::Pause { name, .. }
        | RemoteCommand::Status { name, .. }
        | RemoteCommand::Remove { name } => name,
        RemoteCommand::Share(args) => &args.name,
    };
    let store = store(&root, name)?;
    match &args.command {
        RemoteCommand::Connect {
            url,
            collection,
            token_file,
            enrollment_file,
            read_only,
            ..
        } => {
            let endpoint = Endpoint::parse(url)?;
            let current = store.connection()?;
            if token_file.is_none() && current.is_some() {
                let status = store.status()?;
                if status.enabled || status.pending != 0 || status.stored_sessions != 0 {
                    bail!("a new invitation cannot replace a connection with sharing history or queued uploads; run ctx remote remove {name}, then connect and select history again. Remove discards local backlog; retained server history remains and re-sharing can create duplicate backfill");
                }
            }
            if current
                .as_ref()
                .is_some_and(|current| current.endpoint != endpoint)
            {
                return Err(ctx_history_sharing::Error::DestinationChanged.into());
            }
            let input = if let Some(file) = token_file.as_ref().or(enrollment_file.as_ref()) {
                super::credentials::read_input(file)?
            } else {
                super::credentials::prompt()?
            };
            if token_file.is_none()
                && collection
                    .as_ref()
                    .zip(input.collection.as_ref())
                    .is_some_and(|(selected, issued)| selected != issued)
            {
                bail!("credential belongs to a different collection");
            }
            let selected = collection.as_ref().or(input.collection.as_ref());
            if current
                .as_ref()
                .zip(selected)
                .is_some_and(|(current, selected)| &current.collection != selected)
            {
                return Err(ctx_history_sharing::Error::DestinationChanged.into());
            }
            let (collection, token, only_read) = if token_file.is_some() {
                let collection = selected.ok_or_else(|| anyhow::anyhow!("a bare token requires --collection; an invitation or operator JSON file includes it"))?;
                (collection.clone(), input.secret, *read_only)
            } else {
                // A raw enrollment needs an explicit collection so a saved
                // destination can be checked before consuming the one-use value.
                let collection = selected.ok_or_else(|| anyhow::anyhow!("use the complete invitation JSON, or supply --collection for a bare enrollment"))?;
                let issued = RemoteClient::enroll(&endpoint, &input.secret)?;
                if &issued.collection != collection {
                    bail!("enrollment belongs to a different collection");
                }
                let only_read = *read_only || !issued.credential.grants.publish;
                (issued.collection, issued.credential.secret, only_read)
            };
            let connection = Connection {
                endpoint,
                collection: collection.clone(),
            };
            let credentials = if only_read {
                Credentials::read_only(token)?
            } else {
                Credentials::device(token)?
            };
            store.connect(connection.clone(), credentials)?;
            let status = store.status()?;
            super::print_result(args.format, json!({
                "schema_version": 1, "operation": "remote_connect", "name": name,
                "connection": connection, "local": status
            }), format!("Saved connection {name} to {} / {}. Sharing enabled: {}. No history was uploaded by this command.\nCheck access and searchable coverage: ctx remote status {name} --online",
                connection.endpoint.as_str(), collection, status.enabled), ui)
        }
        RemoteCommand::Share(selection) => share(selection, &root, &store, args.format, ui),
        RemoteCommand::Sync { .. } => {
            let collector = Collector::new(root, store.root().to_path_buf());
            let mut steps = 0;
            while steps < 64 {
                match collector.tick() {
                    TickOutcome::Progress => steps += 1,
                    TickOutcome::Idle => break,
                    TickOutcome::Disabled => {
                        bail!("sharing is not enabled; select history with remote share first")
                    }
                    TickOutcome::Paused => {
                        return Err(ctx_history_sharing::Error::PolicyDenied.into())
                    }
                    TickOutcome::Failed(error) => return Err(error.into()),
                }
            }
            let status = store.status()?;
            if let Some(error) = status.last_error {
                return Err(error.into());
            }
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "remote_sync", "name": name,
                    "steps": steps, "local": status, "searchable_verified": false
                }),
                format!(
                    "{}\nUse remote status {name} --online to inspect searchable coverage.",
                    sharing_summary(&status)
                ),
                ui,
            )
        }
        RemoteCommand::Pause { resume, .. } => {
            store.pause(!resume)?;
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "remote_pause", "name": name,
                    "paused": !resume, "retained_history_unchanged": true
                }),
                if *resume {
                    format!("Resumed {name} under its saved policy.")
                } else {
                    format!("Paused new uploads to {name}. Previously shared history is retained.")
                },
                ui,
            )
        }
        RemoteCommand::Status { online, .. } => {
            let local = store.status()?;
            let connection = store.connection()?;
            let server = if *online {
                Some(store.remote_client()?.status()?)
            } else {
                None
            };
            let mut text = format!(
                "Remote {name}: connected={}, enabled={}, paused={}\n{}",
                local.connected,
                local.enabled,
                local.paused,
                sharing_summary(&local)
            );
            if let Some(status) = &server {
                text.push_str(&format!(
                    "\nServer stored sequence: {}. Searchable sequence: {}. Reads available: {}.",
                    status.stored_sequence, status.searchable_sequence, status.reads_available
                ));
            } else {
                text.push_str(&format!("\nLocal state only. Check server access and coverage: ctx remote status {name} --online"));
            }
            if let Some(error) = &local.last_error {
                text.push_str(&format!("\nLast sharing error: {error}"));
            }
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "remote_status", "name": name,
                    "connection": connection, "local": local, "server": server
                }),
                text,
                ui,
            )
        }
        RemoteCommand::Remove { .. } => {
            store.remove()?;
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "remote_remove", "name": name,
                    "retained_history_unchanged": true
                }),
                format!("Removed connection {name}. Previously shared history is retained."),
                ui,
            )
        }
    }
}

fn share(
    args: &ShareArgs,
    root: &Path,
    store: &SharingStore,
    format: JsonOutputFormat,
    ui: &mut Ui,
) -> Result<()> {
    use ctx_history_sharing::{Backfill, PublicationMode, SourceSelection};
    use std::collections::{BTreeMap, BTreeSet};

    let connection = store
        .connection()?
        .ok_or(ctx_history_sharing::Error::NotConnected)?;
    if matches!(args.mode, Mode::Reviewed)
        && (args.include_future || matches!(args.backfill, BackfillArg::None))
    {
        bail!("reviewed mode authorizes the current snapshot; use --backfill all without --include-future");
    }
    if matches!(args.backfill, BackfillArg::None) && !args.include_future {
        bail!("--backfill none requires --include-future to select any history");
    }
    let work_roots = args
        .work_root
        .iter()
        .map(std::path::absolute)
        .collect::<std::io::Result<Vec<_>>>()?;
    let make_source = |source_id, profile_root| SourceSelection {
        source_id,
        profile_root,
        baseline_revisions: BTreeMap::new(),
        backfill: match args.backfill {
            BackfillArg::All => Backfill::All,
            BackfillArg::None => Backfill::None,
        },
        include_future: args.include_future,
        whole_source: args.whole_source,
        work_roots: work_roots.clone(),
    };
    let mut sources = args
        .source
        .iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| make_source(Some(id.clone()), None))
        .collect::<Vec<_>>();
    for path in args.profile_root.iter().collect::<BTreeSet<_>>() {
        sources.push(make_source(None, Some(std::path::absolute(path)?)));
    }
    let preview = store
        .root()
        .join(format!("preview-{}", uuid::Uuid::new_v4()));
    let mode = match args.mode {
        Mode::Automatic => PublicationMode::Automatic,
        Mode::Reviewed => PublicationMode::Reviewed {
            revisions: BTreeSet::new(),
        },
    };
    let policy = store.prepare_policy(root, &preview, mode, sources).map_err(|error| match error {
        ctx_history_sharing::Error::PolicyDenied => anyhow::Error::new(error).context(
            "no indexed source matches this selection; list sources with ctx sources. For a new profile, run ctx sources add NAME --provider PROVIDER --root /absolute/profile, then ctx import --all and retry with that registered --profile-root"
        ),
        error => error.into(),
    })?;
    store.set_policy(policy.clone())?;
    let status = store.status()?;
    let paused = status.paused;
    super::print_result(format, json!({
        "schema_version": 1, "operation": "remote_share", "name": args.name,
        "connection": connection, "policy": policy, "paused": paused,
        "payload": "retained_normalized", "preview": preview, "local": status
    }), format!(
        "Authorized policy {} for {} / {}. Paused: {paused}.\n{}\nInspectable snapshot: {}\nSource and repository labels do not guarantee that every sentence concerns one project.\nRun ctx remote sync {} now. An enabled local daemon picks up this policy within 30 seconds; a disabled daemon stays disabled.",
        policy.revision, connection.endpoint.as_str(), connection.collection, sharing_summary(&status), preview.display(), args.name
    ), ui)
}

fn sharing_summary(status: &SharingStatus) -> String {
    let mut text = format!(
        "This client's publication state: {} stored sessions.\nThis client's upload queue: {} pending revisions, {} held.",
        status.stored_sessions, status.pending, status.held
    );
    let Some(selection) = &status.selection else {
        text.push_str("\nSelection not yet observed under the current policy.");
        return text;
    };
    text.push_str(&format!(
        "\nObserved sessions within the selected scope only (policy {}): {} selected, {} held, {} excluded.",
        selection.policy_revision,
        selection.selected(),
        selection.held(),
        selection.omitted()
    ));
    for (reason, count) in &selection.counts {
        if *count == 0 {
            continue;
        }
        let label = match reason {
            SelectionDecision::Selected => continue,
            SelectionDecision::ChangedProfile => "Held: profile changed",
            SelectionDecision::OutsideWorkRoots => "Held: outside or mixed work roots",
            SelectionDecision::UnknownWorkRoot => "Held: unknown work root",
            SelectionDecision::NeedsReview => "Held: revision needs review",
            SelectionDecision::UnselectedSource => "Excluded: source not selected",
            SelectionDecision::BackfillExcluded => "Excluded: backfill policy",
            SelectionDecision::FutureExcluded => "Excluded: future updates not authorized",
        };
        text.push_str(&format!("\n{label}: {count}"));
    }
    text
}
