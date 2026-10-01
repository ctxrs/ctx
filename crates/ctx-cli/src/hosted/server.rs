use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use ctx_history_server::{Grants, HistoryServer, ServerConfig};
use serde_json::json;

use crate::{output::JsonOutputFormat, ui::Ui};

mod admin;
use admin::run_remote;

#[derive(Debug, Args)]
pub(crate) struct ServerArgs {
    /// Server storage (default: the native ctx data root / server).
    #[arg(long, conflicts_with = "remote", global = true)]
    root: Option<PathBuf>,
    /// Saved connection for authenticated administration while the server is running.
    #[arg(long, global = true)]
    remote: Option<String>,
    #[arg(long, value_enum, default_value = "text", global = true)]
    pub(crate) format: JsonOutputFormat,
    #[command(subcommand)]
    command: ServerCommand,
}

impl ServerArgs {
    pub(super) fn needs_live_observers(&self) -> bool {
        matches!(self.command, ServerCommand::Run { .. })
    }

    fn local_root(&self, data_root: Option<&Path>) -> Result<PathBuf> {
        match &self.root {
            Some(root) => Ok(root.clone()),
            None => Ok(super::data_root(data_root)?.join("server")),
        }
    }

    pub(super) fn restore_ownership_marker(&self, data_root: Option<&Path>) -> Option<PathBuf> {
        if self.remote.is_some() || !matches!(self.command, ServerCommand::Restore { .. }) {
            return None;
        }
        self.local_root(data_root)
            .ok()
            .map(|root| root.join("authority.sqlite"))
    }

    pub(super) fn observed_operation(&self) -> Option<super::HostedOperation> {
        use super::HostedOperation as Operation;
        if let Some(operation) = self.telemetry_operation() {
            return Some(Operation::Existing(operation));
        }
        Some(match &self.command {
            ServerCommand::Run { .. } => return None,
            ServerCommand::Collection { .. } => Operation::ServerCollectionCreate,
            ServerCommand::User { command } => match command {
                UserCommand::Create { .. } => Operation::ServerUserCreate,
                UserCommand::List { .. } => Operation::ServerUserList,
                UserCommand::Credentials { .. } => Operation::ServerUserCredentials,
                UserCommand::Credential { .. } => Operation::ServerUserCredential,
            },
            ServerCommand::Publications { .. } => Operation::ServerPublications,
            ServerCommand::Status => Operation::ServerStatus,
            _ => return None,
        })
    }

    pub(super) fn telemetry_operation(
        &self,
    ) -> Option<ctx_client_observability::analytics::HostedOperationV1> {
        use ctx_client_observability::analytics::HostedOperationV1 as Operation;
        Some(match self.command {
            ServerCommand::Init { .. } => Operation::ServerInit,
            ServerCommand::Invite { .. } => Operation::ServerInvite,
            ServerCommand::Grant { .. } => Operation::ServerGrant,
            ServerCommand::Revoke { .. } => Operation::ServerRevoke,
            ServerCommand::Withdraw { .. } => Operation::ServerWithdraw,
            ServerCommand::Backup { .. } => Operation::ServerBackup,
            ServerCommand::Restore { .. } => Operation::ServerRestore,
            _ => return None,
        })
    }
}

#[derive(Debug, Subcommand)]
enum ServerCommand {
    /// Create the first collection/operator and save scoped credentials privately.
    ///
    /// Hosted history is beta. Sharing is opt-in.
    Init {
        #[arg(default_value = "team")]
        name: String,
        /// Protected operator file (default: server root / operator.json).
        #[arg(long)]
        credentials_out: Option<PathBuf>,
    },
    /// Serve in the foreground; use --remote for administration while serving.
    Run {
        #[arg(long, default_value = "127.0.0.1:7332")]
        bind: SocketAddr,
        /// Allow non-loopback HTTP behind an explicitly configured TLS reverse proxy.
        #[arg(long)]
        trusted_ingress: bool,
    },
    /// Create a collection with its own read audience.
    Collection {
        #[command(subcommand)]
        command: CollectionCommand,
    },
    /// List users and credentials, create a user, or issue a scoped credential.
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
    /// Invite a new member or enroll another device for an explicit user ID.
    Invite {
        /// Optional display label for a new user; labels never select existing users.
        #[arg(conflicts_with = "user")]
        name: Option<String>,
        /// Existing user ID for another device; requires that user or server-owner access.
        #[arg(long)]
        user: Option<String>,
        /// Invite a reader instead of a read-and-publish member.
        #[arg(long)]
        read_only: bool,
        /// Invite a publisher without read access, including a backup-only device.
        #[arg(long, conflicts_with = "read_only")]
        publish_only: bool,
        /// Also authorize collection administration.
        #[arg(long)]
        manage: bool,
        #[arg(long, default_value_t = 900, value_parser = clap::value_parser!(u64).range(1..=3600))]
        ttl_seconds: u64,
        /// Device credential lifetime; zero means valid until explicitly revoked.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=7_776_000))]
        credential_ttl_seconds: u64,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Review retained publications before granting access to restored history.
    Publications {
        /// Review another collection using this root's owner credential.
        #[arg(long)]
        collection: Option<String>,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=100))]
        limit: u32,
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
        /// With --remote, remove collection membership unless --server-wide is set.
        /// Local root administration always revokes this user across the server.
        #[arg(
            long,
            required_unless_present = "credential",
            conflicts_with = "credential"
        )]
        user: Option<String>,
        /// Credential identifier, never its bearer secret.
        /// Revoke only this device credential; remote use requires server-owner access.
        #[arg(long, required_unless_present = "user")]
        credential: Option<String>,
        /// Revoke the user across the server; remote use requires server-owner access.
        #[arg(long, requires = "user")]
        server_wide: bool,
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
    /// Restore privately with fresh owner access; review history before inviting anyone.
    Restore {
        checkpoint: PathBuf,
        /// Protected fresh owner file (default: new server root / operator.json).
        #[arg(long)]
        credentials_out: Option<PathBuf>,
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
    /// List server-wide user IDs and labels; requires server-owner access.
    List {
        #[command(flatten)]
        page: AccessPage,
    },
    /// List a user's safe credential IDs, collection scopes, rights, expiry and revocations.
    /// Requires server-owner access; bearer secrets are never displayed.
    Credentials {
        user: String,
        #[command(flatten)]
        page: AccessPage,
    },
    Create {
        name: String,
    },
    /// Write a scoped credential directly to a new protected file.
    Credential {
        user: String,
        /// Credential's collection (default: this root's initialized collection).
        #[arg(long)]
        collection: Option<String>,
        #[command(flatten)]
        rights: Rights,
        /// Credential lifetime; zero means valid until explicitly revoked.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=7_776_000))]
        ttl_seconds: u64,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Args)]
struct AccessPage {
    #[arg(long)]
    after: Option<String>,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=100))]
    limit: u32,
}

impl AccessPage {
    fn request(&self) -> ctx_history_server::AccessListRequest {
        ctx_history_server::AccessListRequest {
            after: self.after.clone(),
            limit: self.limit as usize,
        }
    }
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

pub(super) fn run(
    args: &ServerArgs,
    data_root: Option<&Path>,
    ui: &mut Ui,
    observers: &super::HostedObservers,
) -> Result<()> {
    let started = std::time::Instant::now();
    let mut startup_covered = false;
    let result = run_inner(args, data_root, ui, observers, &mut startup_covered);
    if matches!(args.command, ServerCommand::Run { .. }) && !startup_covered {
        if let Err(error) = &result {
            startup_failed(
                observers,
                started.elapsed(),
                ctx_history_server::ServerStage::Configuration,
                error,
            );
        }
    }
    result
}

fn startup_failed(
    observers: &super::HostedObservers,
    duration: std::time::Duration,
    stage: ctx_history_server::ServerStage,
    error: &anyhow::Error,
) {
    use ctx_history_server::{
        ServerFailure, ServerLifecycle, ServerObservation, ServerRuntimeTick,
    };
    if let Some(observer) = &observers.server {
        let failure = error
            .downcast_ref::<ctx_history_server::Error>()
            .map(ServerFailure::from)
            .unwrap_or(if error.is::<std::io::Error>() {
                ServerFailure::Io
            } else {
                ServerFailure::Invalid
            });
        observer(ServerObservation::Lifecycle {
            kind: ServerLifecycle::Failed,
            stage,
            duration,
            failure: Some(failure),
            backlog: None,
        });
    }
    if let Some(hook) = &observers.runtime {
        hook(ServerRuntimeTick::Failed);
    }
}

fn run_inner(
    args: &ServerArgs,
    data_root: Option<&Path>,
    ui: &mut Ui,
    observers: &super::HostedObservers,
    startup_covered: &mut bool,
) -> Result<()> {
    if let Some(name) = &args.remote {
        return run_remote(
            args,
            &super::remote_store(&super::data_root(data_root)?, name)?.remote_client()?,
            &super::data_root(data_root)?.join("invitations"),
            ui,
        );
    }
    let root = args.local_root(data_root)?;
    let root = root.as_path();
    if matches!(
        args.command,
        ServerCommand::Invite { .. }
            | ServerCommand::Publications { .. }
            | ServerCommand::Withdraw {
                token_file: None,
                ..
            }
    ) {
        let client = local_client(root, &args.command).context(
            "local admin connection is unavailable; run ctx server init first, or use --remote NAME for an HTTPS admin connection",
        )?;
        return run_remote(args, &client, &root.join("invitations"), ui);
    }
    if let ServerCommand::Restore {
        checkpoint,
        credentials_out,
    } = &args.command
    {
        let credentials_out = credentials_out
            .clone()
            .unwrap_or_else(|| root.join("operator.json"));
        if credentials_out == Path::new("-") {
            bail!("owner credentials require a protected file, not stdout");
        }
        let info = HistoryServer::restore_checkpoint(checkpoint, root, &credentials_out)
            .map_err(|error| {
                let message = format!(
                    "could not restore from {}: {error}; expected a checkpoint directory created by ctx server backup",
                    checkpoint.display()
                );
                anyhow::Error::new(error).context(message)
            })?;
        remember_operator_file(root, &credentials_out)?;
        let owner = save_admin(root, &credentials_out, "http://127.0.0.1:7332")?;
        return super::print_result(args.format, json!({
            "schema_version": 1, "operation": "server_restore", "checkpoint": info.checkpoint,
            "collections": info.collections,
            "previous_access_revoked": true,
            "principal": owner.principal, "collection": owner.collection,
            "credentials_file": credentials_out, "root": root
        }), format!(
            "Restored privately with fresh owner access. Previous credentials, invitations, and grants are invalid.\nOwner credentials saved to {}.\nCollections: {}\nReview restored history and withdraw anything that must stay private before granting access or inviting members.\nRun ctx server --root {} run, then ctx server --root {} publications to begin review.\nFor reading sessions at the default bind: ctx remote connect http://127.0.0.1:7332 --name recovered --token-file {:?}\nThen ctx show session CITATION --server recovered. Select another collection with --collection when connecting.",
            credentials_out.display(), info.collections.join(", "), root.display(), root.display(), credentials_out
        ), ui);
    }
    if !matches!(args.command, ServerCommand::Init { .. })
        && !root.join("authority.sqlite").is_file()
    {
        bail!("server root is not initialized; run ctx server init (use the same --root override if selected)");
    }
    let mut config = ServerConfig::new(root);
    if let ServerCommand::Run {
        bind,
        trusted_ingress,
        ..
    } = &args.command
    {
        config.bind = *bind;
        config.trusted_ingress = *trusted_ingress;
    }
    *startup_covered = true;
    let server_observer = if matches!(args.command, ServerCommand::Run { .. }) {
        observers.server.clone()
    } else {
        None
    };
    let opened = HistoryServer::open_with_observer(config, server_observer);
    if opened.is_err() && matches!(args.command, ServerCommand::Run { .. }) {
        if let Some(hook) = &observers.runtime {
            hook(ctx_history_server::ServerRuntimeTick::Failed);
        }
    }
    let server = match opened {
        Ok(server) => server,
        // An active listener holds the server root lock. Reuse its saved
        // authenticated client for operations supported both online and offline.
        Err(ctx_history_server::Error::Unavailable)
            if matches!(
                args.command,
                ServerCommand::Grant { .. }
                    | ServerCommand::Revoke { .. }
                    | ServerCommand::User {
                        command: UserCommand::List { .. } | UserCommand::Credentials { .. }
                    }
                    | ServerCommand::Status
            ) =>
        {
            return run_remote(
                args,
                &local_client(root, &args.command)?,
                &root.join("invitations"),
                ui,
            );
        }
        Err(ctx_history_server::Error::Unavailable)
            if matches!(args.command, ServerCommand::Init { .. }) =>
        {
            bail!("server root is in use; stop its server before rerunning init. Existing identity and credentials have not been changed");
        }
        Err(error) => return Err(error.into()),
    };
    match &args.command {
        ServerCommand::Init {
            name,
            credentials_out,
        } => {
            let credentials_out = operator_file(root, credentials_out.as_deref())?;
            if credentials_out == Path::new("-") {
                bail!("bootstrap credentials require a protected file, not stdout");
            }
            let initialized = if credentials_out.try_exists()? {
                let owner: ctx_history_server::TokenFile =
                    super::credentials::read_json(&credentials_out)?;
                server
                    .whoami(&owner.credential.secret, &owner.collection)
                    .and_then(|identity| {
                        if identity.server_owner {
                            Ok(())
                        } else {
                            Err(ctx_history_server::Error::Forbidden)
                        }
                    })
                    .context("existing operator credential lacks server-owner authority; use its original credential file, or restore a checkpoint into a new root")?;
                false
            } else {
                server.bootstrap(name, &credentials_out)
                    .context("cannot initialize server; an existing server needs its original --credentials-out file, and a new credential path must have an existing writable parent")?;
                true
            };
            remember_operator_file(root, &credentials_out)?;
            let url = admin_store(root)
                .connection()?
                .map(|current| current.endpoint.as_str().to_owned())
                .unwrap_or_else(|| "http://127.0.0.1:7332".into());
            let owner = save_admin(root, &credentials_out, &url)?;
            super::print_result(args.format, json!({
                "schema_version": 1, "operation": "server_init", "initialized": initialized,
                "feature_maturity": "beta", "sharing": "opt_in",
                "principal": owner.principal, "collection": owner.collection,
                "credentials_file": credentials_out, "root": root, "admin_endpoint": url
            }), format!(
                "{} server. Operator credentials: {}.\n{}Local administration configured for {url}.\nNext: ctx server run, then ctx server invite in another terminal. Use this same --root override for each command if selected.",
                if initialized { "Initialized" } else { "Reused existing" }, credentials_out.display(),
                if initialized { "Hosted history is beta. Sharing is opt-in.\n" } else { "" }
            ), ui)
        }
        ServerCommand::Run { .. } => {
            let prepared = std::time::Instant::now();
            let file = operator_file(root, None).inspect_err(|error| {
                startup_failed(
                    observers,
                    prepared.elapsed(),
                    ctx_history_server::ServerStage::ReadyCallback,
                    error,
                );
            })?;
            ctx_history_server::serve_blocking_with_hooks(
                std::sync::Arc::new(server),
                |bound| {
                    if bound.ip().is_loopback() || bound.ip().is_unspecified() {
                        let ip = if bound.ip().is_unspecified() {
                            if bound.is_ipv4() {
                                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
                            } else {
                                std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
                            }
                        } else {
                            bound.ip()
                        };
                        let endpoint = format!("http://{}", SocketAddr::new(ip, bound.port()));
                        save_admin(root, &file, &endpoint).map_err(|error| {
                            ctx_history_server::Error::Io(std::io::Error::other(error))
                        })?;
                    } else {
                        use std::io::Write;
                        admin_store(root).remove().map_err(|error| {
                            ctx_history_server::Error::Io(std::io::Error::other(error))
                        })?;
                        writeln!(ctx_terminal::output::stderr_writer(), "Local plaintext administration is unavailable for this bind. Connect an HTTPS admin endpoint with ctx remote connect, then use ctx server --remote NAME invite.")?;
                    }
                    Ok(())
                },
                observers.runtime.clone(),
            )?;
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
        ServerCommand::User { command } => run_user(command, &server, root, args.format, ui),
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
            let collection = match collection {
                Some(collection) => collection.clone(),
                None => admin_store(root).connection()?.ok_or_else(|| anyhow::anyhow!("select --collection or run server init to configure local administration"))?.collection,
            };
            server.set_grants(user, &collection, grants)?;
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
        ServerCommand::Revoke {
            user, credential, ..
        } => {
            let (kind, id) = if let Some(user) = user {
                server.revoke_principal(user)?;
                ("user", user)
            } else if let Some(credential) = credential {
                server.revoke_credential(credential)?;
                ("credential", credential)
            } else {
                bail!("select a user or credential to revoke");
            };
            admin::print_revoked(args.format, kind, id, ui)
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
                "Collections: {}\nPending operations: {}\nStaged uploads: {}",
                health.collections, health.pending_operations, health.staged_uploads
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
        ServerCommand::Restore { .. }
        | ServerCommand::Invite { .. }
        | ServerCommand::Publications { .. } => {
            unreachable!("handled before opening the server")
        }
    }
}

fn run_user(
    command: &UserCommand,
    server: &HistoryServer,
    root: &Path,
    format: JsonOutputFormat,
    ui: &mut Ui,
) -> Result<()> {
    if matches!(
        command,
        UserCommand::List { .. } | UserCommand::Credentials { .. }
    ) {
        let owner: ctx_history_server::TokenFile =
            super::credentials::read_json(&operator_file(root, None)?)?;
        return match command {
            UserCommand::List { page } => admin::print_users(
                format,
                server.list_principals(&owner.credential.secret, page.request())?,
                ui,
            ),
            UserCommand::Credentials { user, page } => admin::print_credentials(
                format,
                user,
                server.list_credentials(&owner.credential.secret, user, page.request())?,
                ui,
            ),
            _ => unreachable!(),
        };
    }
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
    let (user, collection, rights, output) = match command {
        UserCommand::Credential {
            user,
            collection,
            rights,
            output,
            ..
        } => (user, collection, *rights, output),
        _ => unreachable!(),
    };
    if !rights.read && !rights.publish && !rights.manage {
        bail!("select at least one credential right: --read, --publish, or --manage");
    }
    let collection = match collection {
        Some(collection) => collection.clone(),
        None => {
            admin_store(root)
                .connection()?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "select --collection or run server init to configure local administration"
                    )
                })?
                .collection
        }
    };
    // Reserve the protected file before issuing anything. A failed issuance
    // removes only that file, never an existing credential.
    let mut file = super::credentials::create(output)?;
    let issued = match command {
        UserCommand::Credential { ttl_seconds, .. } => {
            server.issue_credential(user, &collection, rights.into(), *ttl_seconds)
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
            "scope": "collection", "collection": collection,
            "id": secret.id, "expires_at": secret.expires_at, "grants": secret.grants, "file": output
        }),
        format!(
            "Saved {kind} {} for collection {collection} to {} ({})",
            secret.id,
            output.display(),
            if secret.expires_at == 0 {
                "valid until revoked".to_owned()
            } else {
                format!("expires at {}", secret.expires_at)
            }
        ),
        ui,
    )
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

fn admin_store(root: &Path) -> ctx_history_sharing::SharingStore {
    ctx_history_sharing::SharingStore::new(root.join("admin"))
}

fn local_client(root: &Path, command: &ServerCommand) -> Result<ctx_history_sharing::RemoteClient> {
    use ctx_history_sharing::{Credentials, RemoteClient};
    let client = admin_store(root).remote_client()?;
    let selected = match command {
        ServerCommand::Publications { collection, .. }
        | ServerCommand::Grant { collection, .. }
        | ServerCommand::Withdraw { collection, .. } => collection.as_ref(),
        _ => None,
    };
    let Some(selected) = selected.filter(|selected| *selected != &client.connection().collection)
    else {
        return Ok(client);
    };
    let owner: ctx_history_server::TokenFile =
        super::credentials::read_json(&operator_file(root, None)?)?;
    let mut connection = client.connection().clone();
    connection.collection = selected.clone();
    Ok(RemoteClient::new(
        connection,
        Credentials::device(owner.credential.secret)?,
    )?)
}

fn save_admin(root: &Path, file: &Path, url: &str) -> Result<ctx_history_server::TokenFile> {
    use ctx_history_sharing::{Connection, Credentials, Endpoint};
    let owner: ctx_history_server::TokenFile = super::credentials::read_json(file)?;
    let connection = Connection {
        endpoint: Endpoint::parse(url)?,
        collection: owner.collection.clone(),
    };
    let store = admin_store(root);
    if store
        .connection()?
        .is_some_and(|current| current != connection)
    {
        // This store owns only local administration, never a sharing policy.
        store.remove()?;
    }
    if owner.collection.is_empty() {
        return Ok(owner);
    }
    store.connect(
        connection,
        Credentials::device(owner.credential.secret.clone())?,
    )?;
    Ok(owner)
}

fn operator_file(root: &Path, selected: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = selected {
        if path == Path::new("-") {
            bail!("operator credentials require a protected file, not stdout");
        }
        return Ok(std::path::absolute(path)?);
    }
    let saved = root.join("operator-file.json");
    if saved.try_exists()? {
        super::credentials::read_json(&saved)
    } else {
        Ok(root.join("operator.json"))
    }
}

fn remember_operator_file(root: &Path, selected: &Path) -> Result<()> {
    let selected = std::path::absolute(selected)?;
    let path = root.join("operator-file.json");
    if path.try_exists()? {
        let existing: PathBuf = super::credentials::read_json(&path)?;
        if existing != selected {
            bail!("this server already uses a different operator credential file; omit --credentials-out to reuse its saved location");
        }
        return Ok(());
    }
    super::credentials::write(&mut super::credentials::create(&path)?, &selected)
}
