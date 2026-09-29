use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use ctx_history_server::{Grants, HistoryServer, ServerConfig};
use serde_json::json;

use crate::{output::JsonOutputFormat, ui::Ui};

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
    /// Create a user or issue an individually revocable credential/enrollment.
    User {
        #[command(subcommand)]
        command: UserCommand,
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
    Create {
        name: String,
    },
    /// Write a scoped credential directly to a new protected file.
    Credential {
        user: String,
        #[command(flatten)]
        rights: Rights,
        /// Credential lifetime; zero means valid until explicitly revoked.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=7_776_000))]
        ttl_seconds: u64,
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
            &super::data_root(data_root)?.join("invitations"),
            ui,
        );
    }
    let root = match &args.root {
        Some(root) => root.clone(),
        None => super::data_root(data_root)?.join("server"),
    };
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
    let server = match HistoryServer::open(config) {
        Ok(server) => server,
        // An active listener holds the server root lock. Reuse its saved
        // authenticated client for operations supported both online and offline.
        Err(ctx_history_server::Error::Unavailable)
            if matches!(
                args.command,
                ServerCommand::Grant { .. } | ServerCommand::Status
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
            if matches!(args.command, ServerCommand::Revoke { .. }) =>
        {
            bail!("server root is in use; stop its server before revoking access across this root. For collection-only membership revocation, use ctx server --remote NAME revoke --user ID");
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
                server.list_publications(&owner.credential.secret, &owner.collection,
                    ctx_history_server::PublicationListRequest { after: None, limit: 1 })
                    .context("existing operator credential does not authorize this server; use its original credential file, or restore a checkpoint into a new root")?;
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
                "principal": owner.principal, "collection": owner.collection,
                "credentials_file": credentials_out, "root": root, "admin_endpoint": url
            }), format!(
                "{} server. Operator credentials: {}.\nLocal administration configured for {url}.\nNext: ctx server run, then ctx server invite NAME in another terminal. Use this same --root override for each command if selected.",
                if initialized { "Initialized" } else { "Reused existing" }, credentials_out.display()
            ), ui)
        }
        ServerCommand::Run { .. } => {
            let file = operator_file(root, None)?;
            ctx_history_server::serve_blocking_with_ready(std::sync::Arc::new(server), |bound| {
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
            })?;
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
            "Saved {kind} {} to {} ({})",
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

fn run_remote(
    args: &ServerArgs,
    client: &ctx_history_sharing::RemoteClient,
    invitations: &Path,
    ui: &mut Ui,
) -> Result<()> {
    let collection = &client.connection().collection;
    let result: Result<()> = (|| {
        match &args.command {
        ServerCommand::Publications { collection: selected, after, limit } => {
            require_collection(selected.as_deref(), collection)?;
            let page = client.list_publications(&ctx_history_server::PublicationListRequest {
                after: after.clone(), limit: *limit as usize,
            })?;
            let mut text = format!("Retained revisions in {collection}: {} on this page.", page.publications.len());
            for entry in &page.publications {
                text.push_str(&format!("\n{}  owner={}  withdrawn={}", entry.state.publication, entry.state.owner, entry.state.withdrawn));
                text.push_str(&format!("\n  revision={}{}", entry.retained_revision,
                    if entry.retained_revision == entry.state.revision { " (current)" } else { " (older retained revision)" }));
                for citation in &entry.session_citations {
                    text.push_str(&format!("\n  {citation}"));
                }
            }
            if let Some(cursor) = &page.next_cursor {
                text.push_str(&format!("\nMore publications: repeat with --after {cursor}"));
            }
            text.push_str("\nA new read grant exposes all nonwithdrawn retained history in this collection, including older revisions.");
            super::print_result(args.format, json!({
                "schema_version": 1, "operation": "server_publications", "collection": collection,
                "publications": page.publications, "next_cursor": page.next_cursor
            }), text, ui)
        }
        ServerCommand::Invite { name, read_only, manage, ttl_seconds, credential_ttl_seconds, output } => {
            let output = match output {
                Some(output) => output.clone(),
                None => {
                    let directory = invitations;
                    ctx_history_platform::platform_security::create_private_directory_all(directory)?;
                    directory.join(format!("{}.json", uuid::Uuid::new_v4()))
                }
            };
            let mut file = super::credentials::create(&output)?;
            let invitation = match client.invite(&ctx_history_server::InviteRequest {
                name: name.clone(), grants: Grants { read: true, publish: !read_only, manage: *manage },
                enrollment_ttl_seconds: *ttl_seconds, credential_ttl_seconds: *credential_ttl_seconds,
            }) {
                Ok(invitation) => invitation,
                Err(error) => {
                    drop(file);
                    let _ = std::fs::remove_file(&output);
                    return Err(error.into());
                }
            };
            super::credentials::write(&mut file, &invitation)?;
            super::print_result(args.format, json!({
                "schema_version": 1, "operation": "user_invite", "user": invitation.principal,
                "collection": invitation.collection, "id": invitation.enrollment.id,
                "expires_at": invitation.enrollment.expires_at, "grants": invitation.enrollment.grants,
                "file": output
            }), format!("Invited {name} to {collection}; one-time enrollment saved to {}.\nSend that protected file to the member. They run ctx remote connect SERVER_URL and paste its compact JSON, or use --enrollment-file PATH.", output.display()), ui)
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
        _ => bail!("this operation requires local --root administration; remote administration supports invite, grant, revoke --user, withdraw, and status"),
    }
    })();
    result.map_err(|error| {
        if matches!(
            error.downcast_ref::<ctx_history_sharing::Error>(),
            Some(ctx_history_sharing::Error::Unavailable)
        ) {
            error.context(format!(
                "history server at {} is unavailable; check or start the listener, then retry",
                client.connection().endpoint.as_str()
            ))
        } else {
            error
        }
    })
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
