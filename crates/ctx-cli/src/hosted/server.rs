use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use ctx_history_server::{Grants, HistoryServer, ServerConfig};
use serde_json::json;

use crate::{output::JsonOutputFormat, ui::Ui};

#[derive(Debug, Args)]
pub(crate) struct ServerArgs {
    /// Explicit server storage, separate from the local history data root.
    #[arg(long, required_unless_present = "remote", conflicts_with = "remote")]
    root: Option<PathBuf>,
    /// Saved connection for authenticated administration while the server is running.
    #[arg(long, required_unless_present = "root")]
    remote: Option<String>,
    #[arg(long, value_enum, default_value = "text", global = true)]
    pub(crate) format: JsonOutputFormat,
    #[command(subcommand)]
    command: ServerCommand,
}

#[derive(Debug, Subcommand)]
enum ServerCommand {
    /// Create the first collection/operator and save scoped credentials privately.
    Init {
        name: String,
        #[arg(long)]
        credentials_out: PathBuf,
        /// Independently retain this recovery authority file outside the server root.
        #[arg(long)]
        authority_file: Option<PathBuf>,
    },
    /// Serve in the foreground; use --remote for administration while serving.
    Run {
        #[arg(long, default_value = "127.0.0.1:7332")]
        bind: SocketAddr,
        /// Allow non-loopback HTTP behind an explicitly configured TLS reverse proxy.
        #[arg(long)]
        trusted_ingress: bool,
        /// Retained recovery authority file; a previously configured path is reused.
        #[arg(long)]
        authority_file: Option<PathBuf>,
    },
    /// Create a collection with its own read audience.
    Collection {
        #[command(subcommand)]
        command: CollectionCommand,
    },
    /// Create a user or issue an individually revocable credential/enrollment.
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
    /// Set the exact collection rights for a user; omitted rights are removed.
    Grant {
        #[arg(long)]
        user: String,
        #[arg(long)]
        collection: Option<String>,
        #[arg(long)]
        read: bool,
        #[arg(long)]
        publish: bool,
        #[arg(long)]
        manage: bool,
    },
    /// Revoke future access without deleting already shared history.
    Revoke {
        #[arg(
            long,
            required_unless_present = "credential",
            conflicts_with = "credential"
        )]
        user: Option<String>,
        /// Credential identifier, never its bearer secret.
        #[arg(long, required_unless_present = "user")]
        credential: Option<String>,
    },
    /// Withdraw an exact publication revision using a manage credential.
    Withdraw {
        #[arg(long)]
        collection: Option<String>,
        #[arg(long)]
        publication: String,
        /// Optionally require this revision; otherwise inspect the current server revision.
        #[arg(long)]
        expected_revision: Option<String>,
        /// Protected credential file, or '-' to read piped stdin.
        #[arg(long)]
        token_file: Option<PathBuf>,
    },
    /// Write a coherent local checkpoint for an external backup tool.
    Backup {
        #[arg(long)]
        output: PathBuf,
    },
    /// Restore into a new server root; reopen only with exact current authority.
    Restore {
        checkpoint: PathBuf,
        /// Independently retained current authority file matching this checkpoint exactly.
        #[arg(long)]
        authority_file: Option<PathBuf>,
    },
    /// Inspect local health without starting a listener.
    Status,
}

#[derive(Debug, Subcommand)]
enum CollectionCommand {
    Create { name: String },
}

#[derive(Debug, Subcommand)]
enum UserCommand {
    Create {
        name: String,
    },
    /// Write a scoped credential directly to a new protected file.
    Credential {
        user: String,
        #[command(flatten)]
        rights: Rights,
        #[arg(long, default_value_t = 2_592_000, value_parser = clap::value_parser!(u64).range(1..=7_776_000))]
        ttl_seconds: u64,
        #[arg(long)]
        output: PathBuf,
    },
    /// Invite a named member in one step and save a single-use enrollment file.
    Invite {
        name: String,
        /// Invite a reader instead of a read-and-publish member.
        #[arg(long)]
        read_only: bool,
        /// Also authorize collection administration.
        #[arg(long)]
        manage: bool,
        #[arg(long, default_value_t = 900, value_parser = clap::value_parser!(u64).range(1..=3600))]
        ttl_seconds: u64,
        #[arg(long, default_value_t = 2_592_000, value_parser = clap::value_parser!(u64).range(1..=7_776_000))]
        credential_ttl_seconds: u64,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, Args)]
struct Rights {
    #[arg(long)]
    read: bool,
    #[arg(long)]
    publish: bool,
    #[arg(long)]
    manage: bool,
}

impl From<Rights> for Grants {
    fn from(rights: Rights) -> Self {
        Self {
            read: rights.read,
            publish: rights.publish,
            manage: rights.manage,
        }
    }
}

pub(super) fn run(args: &ServerArgs, data_root: Option<&Path>, ui: &mut Ui) -> Result<()> {
    if let Some(name) = &args.remote {
        return run_remote(
            args,
            &super::remote_store(&super::data_root(data_root)?, name)?.remote_client()?,
            ui,
        );
    }
    let root = args
        .root
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("select --root or --remote"))?;
    if let ServerCommand::Restore {
        checkpoint,
        authority_file,
    } = &args.command
    {
        let info = match authority_file {
            Some(path) => HistoryServer::restore_checkpoint_with_authority(checkpoint, root, path)?,
            None => HistoryServer::restore_checkpoint(checkpoint, root)?,
        };
        let recovery_closed = authority_file.is_none();
        return super::print_result(
            args.format,
            json!({
                "schema_version": 1, "operation": "server_restore", "checkpoint": info,
                "recovery_closed": recovery_closed
            }),
            if recovery_closed {
                "Restored server checkpoint. Recovery is closed; old credentials and shared reads remain disabled."
            } else {
                "Restored server checkpoint with matching current authority. Start the server to rebuild searchable coverage."
            },
            ui,
        );
    }
    if !matches!(args.command, ServerCommand::Init { .. })
        && !root.join("authority.sqlite").is_file()
    {
        bail!("server root is not initialized; run server --root PATH init first");
    }
    let mut config = ServerConfig::new(root);
    if let ServerCommand::Init { authority_file, .. } | ServerCommand::Run { authority_file, .. } =
        &args.command
    {
        config.authority_file = authority_file.clone();
    }
    if let ServerCommand::Run {
        bind,
        trusted_ingress,
        ..
    } = &args.command
    {
        config.bind = *bind;
        config.trusted_ingress = *trusted_ingress;
    }
    let server = HistoryServer::open(config)?;
    match &args.command {
        ServerCommand::Init {
            name,
            credentials_out,
            ..
        } => {
            if credentials_out == std::path::Path::new("-") {
                bail!("bootstrap credentials require a protected file, not stdout");
            }
            let info = server.bootstrap(name, credentials_out)?;
            let text = format!(
                "Initialized collection {} with operator {}\nCredentials saved to {}. No listener was started.",
                info.collection,
                info.principal,
                credentials_out.display()
            );
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "server_init", "principal": info.principal,
                    "collection": info.collection, "credentials_file": credentials_out
                }),
                text,
                ui,
            )
        }
        ServerCommand::Run { .. } => {
            // The server crate owns the listener and its lifecycle. Do not claim
            // readiness before it has successfully bound the selected address.
            ctx_history_server::serve_blocking(std::sync::Arc::new(server))?;
            Ok(())
        }
        ServerCommand::Collection {
            command: CollectionCommand::Create { name },
        } => {
            let id = server.create_collection(name)?;
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "collection_create", "collection": id, "name": name
                }),
                format!("Created collection {name}: {id}"),
                ui,
            )
        }
        ServerCommand::User { command } => run_user(command, &server, args.format, ui),
        ServerCommand::Grant {
            user,
            collection,
            read,
            publish,
            manage,
        } => {
            let grants = Grants {
                read: *read,
                publish: *publish,
                manage: *manage,
            };
            let collection = collection
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("local grant requires --collection"))?;
            server.set_grants(user, collection, grants)?;
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "grant", "user": user,
                    "collection": collection, "grants": grants
                }),
                format!(
                    "Set {user}'s rights in {collection}: read={read}, publish={publish}, manage={manage}"
                ),
                ui,
            )
        }
        ServerCommand::Revoke { user, credential } => {
            let (kind, id) = if let Some(user) = user {
                server.revoke_principal(user)?;
                ("user", user)
            } else if let Some(credential) = credential {
                server.revoke_credential(credential)?;
                ("credential", credential)
            } else {
                bail!("select a user or credential to revoke");
            };
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "revoke", "kind": kind, "id": id,
                    "retained_history_unchanged": true
                }),
                format!("Revoked {kind} {id}. Previously shared history is retained."),
                ui,
            )
        }
        ServerCommand::Withdraw {
            collection,
            publication,
            expected_revision,
            token_file,
        } => {
            let collection = collection
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("local withdrawal requires --collection"))?;
            let token_file = token_file
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("local withdrawal requires --token-file"))?;
            let token = super::credentials::read(token_file)?;
            let state = server.publication_state(&token, collection, publication)?;
            let receipt = server.remove_publication(
                &token,
                collection,
                removal(collection, &state, expected_revision.as_deref())?,
            )?;
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "withdraw", "receipt": receipt
                }),
                format!("Withdrew publication {publication} from {collection}"),
                ui,
            )
        }
        ServerCommand::Backup { output } => {
            let info = server.checkpoint(output)?;
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "server_backup", "checkpoint": info,
                    "path": output, "off_host_copy_verified": false
                }),
                format!(
                    "Checkpoint saved to {}\nThis local checkpoint has not been verified off-host.",
                    output.display()
                ),
                ui,
            )
        }
        ServerCommand::Status => {
            let health = server.local_health()?;
            let text = format!(
                "Recovery closed: {}\nCollections: {}\nPending operations: {}\nStaged uploads: {}",
                health.recovery_closed,
                health.collections,
                health.pending_operations,
                health.staged_uploads
            );
            super::print_result(
                args.format,
                json!({
                    "schema_version": 1, "operation": "server_status", "health": health
                }),
                text,
                ui,
            )
        }
        ServerCommand::Restore { .. } => {
            unreachable!("restore is handled without opening destination")
        }
    }
}

fn run_user(
    command: &UserCommand,
    server: &HistoryServer,
    format: JsonOutputFormat,
    ui: &mut Ui,
) -> Result<()> {
    if let UserCommand::Create { name } = command {
        let id = server.create_principal(name)?;
        return super::print_result(
            format,
            json!({
                "schema_version": 1, "operation": "user_create", "user": id, "name": name
            }),
            format!("Created user {name}: {id}"),
            ui,
        );
    }
    let (user, rights, output) = match command {
        UserCommand::Credential {
            user,
            rights,
            output,
            ..
        } => (user, *rights, output),
        UserCommand::Invite { .. } => {
            bail!("use server --remote NAME user invite to invite a member")
        }
        UserCommand::Create { .. } => unreachable!(),
    };
    if !rights.read && !rights.publish && !rights.manage {
        bail!("select at least one credential right: --read, --publish, or --manage");
    }
    // Reserve the protected file before issuing anything. A failed issuance
    // removes only that file, never an existing credential.
    let mut file = super::credentials::create(output)?;
    let issued = match command {
        UserCommand::Credential { ttl_seconds, .. } => {
            server.issue_credential(user, rights.into(), *ttl_seconds)
        }
        _ => unreachable!(),
    };
    let secret = match issued {
        Ok(secret) => secret,
        Err(error) => {
            drop(file);
            let _ = std::fs::remove_file(output);
            return Err(error.into());
        }
    };
    super::credentials::write(&mut file, &secret)?;
    let kind = "credential";
    super::print_result(
        format,
        json!({
            "schema_version": 1, "operation": format!("user_{kind}"), "user": user,
            "id": secret.id, "expires_at": secret.expires_at, "grants": secret.grants, "file": output
        }),
        format!(
            "Saved {kind} {} to {} (expires at {})",
            secret.id,
            output.display(),
            secret.expires_at
        ),
        ui,
    )
}

fn run_remote(
    args: &ServerArgs,
    client: &ctx_history_sharing::RemoteClient,
    ui: &mut Ui,
) -> Result<()> {
    let collection = &client.connection().collection;
    match &args.command {
        ServerCommand::User { command: UserCommand::Invite { name, read_only, manage, ttl_seconds, credential_ttl_seconds, output } } => {
            let mut file = super::credentials::create(output)?;
            let invitation = match client.invite(&ctx_history_server::InviteRequest {
                name: name.clone(), grants: Grants { read: true, publish: !read_only, manage: *manage },
                enrollment_ttl_seconds: *ttl_seconds, credential_ttl_seconds: *credential_ttl_seconds,
            }) {
                Ok(invitation) => invitation,
                Err(error) => {
                    drop(file);
                    let _ = std::fs::remove_file(output);
                    return Err(error.into());
                }
            };
            super::credentials::write(&mut file, &invitation)?;
            super::print_result(args.format, json!({
                "schema_version": 1, "operation": "user_invite", "user": invitation.principal,
                "collection": invitation.collection, "id": invitation.enrollment.id,
                "expires_at": invitation.enrollment.expires_at, "grants": invitation.enrollment.grants,
                "file": output
            }), format!("Invited {name} to {collection}; enrollment saved to {}", output.display()), ui)
        }
        ServerCommand::Grant { user, collection: selected, read, publish, manage } => {
            require_collection(selected.as_deref(), collection)?;
            let grants = Grants { read: *read, publish: *publish, manage: *manage };
            client.grant(&ctx_history_server::GrantRequest { principal: user.clone(), grants })?;
            super::print_result(args.format, json!({
                "schema_version": 1, "operation": "grant", "user": user, "collection": collection,
                "grants": grants
            }), format!("Set {user}'s rights in {collection}: read={read}, publish={publish}, manage={manage}"), ui)
        }
        ServerCommand::Revoke { user: Some(user), .. } => {
            client.revoke_member(user)?;
            super::print_result(args.format, json!({
                "schema_version": 1, "operation": "revoke", "kind": "membership", "id": user,
                "collection": collection, "retained_history_unchanged": true
            }), format!("Revoked {user}'s access to {collection}. Previously shared history is retained."), ui)
        }
        ServerCommand::Withdraw { collection: selected, publication, expected_revision, token_file } => {
            require_collection(selected.as_deref(), collection)?;
            if token_file.is_some() { bail!("remote administration uses the saved connection credential"); }
            let state = client.publication(publication)?;
            let receipt = client.remove(&removal(collection, &state, expected_revision.as_deref())?)?;
            super::print_result(args.format, json!({
                "schema_version": 1, "operation": "withdraw", "receipt": receipt
            }), format!("Withdrew publication {publication} from {collection}"), ui)
        }
        ServerCommand::Status => {
            let status = client.status()?;
            super::print_result(args.format, json!({
                "schema_version": 1, "operation": "server_status", "status": status
            }), format!("Collection {collection}: stored {}, searchable {}, reads available: {}",
                status.stored_sequence, status.searchable_sequence, status.reads_available), ui)
        }
        _ => bail!("this operation requires local --root administration; remote administration supports user invite, grant, revoke --user, withdraw, and status"),
    }
}

fn require_collection(selected: Option<&str>, connected: &str) -> Result<()> {
    if selected.is_some_and(|collection| collection != connected) {
        bail!("selected collection differs from the saved connection");
    }
    Ok(())
}

fn removal(
    collection: &str,
    state: &ctx_history_server::PublicationState,
    expected: Option<&str>,
) -> Result<ctx_history_server::WithdrawRequest> {
    use sha2::{Digest, Sha256};
    if state.withdrawn {
        bail!("publication is already withdrawn");
    }
    if expected.is_some_and(|revision| revision != state.revision) {
        return Err(ctx_history_server::Error::Conflict.into());
    }
    let key = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            "withdraw",
            collection,
            &state.publication,
            &state.revision,
            state.sequence,
            state.writer_epoch,
        ))?)
    );
    Ok(ctx_history_server::WithdrawRequest {
        operation: ctx_history_server::Operation {
            idempotency_key: key.clone(),
            publication: state.publication.clone(),
            writer_epoch: state.writer_epoch,
            policy_revision: state.policy_revision,
            expected_revision: Some(state.revision.clone()),
            expected_sequence: Some(state.sequence),
            revision: format!("withdraw:{key}"),
        },
    })
}
