use std::path::Path;

use anyhow::{bail, Result};
use ctx_history_server::Grants;
use serde_json::json;

use crate::{output::JsonOutputFormat, ui::Ui};

use super::super::{credentials, print_result};
use super::{removal, ServerArgs, ServerCommand, UserCommand};

pub(super) fn run_remote(
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
            print_result(args.format, json!({
                "schema_version": 1, "operation": "server_publications", "collection": collection,
                "publications": page.publications, "next_cursor": page.next_cursor
            }), text, ui)
        }
        ServerCommand::Invite { name, user, read_only, publish_only, manage, ttl_seconds, credential_ttl_seconds, output } => {
            let output = match output {
                Some(output) => output.clone(),
                None => {
                    let directory = invitations;
                    ctx_history_platform::platform_security::create_private_directory_all(directory)?;
                    directory.join(format!("{}.json", uuid::Uuid::new_v4()))
                }
            };
            let mut file = credentials::create(&output)?;
            let invitation = match client.invite(&ctx_history_server::InviteRequest {
                name: name.clone(), principal: user.clone(), grants: Grants { read: !publish_only, publish: !read_only, manage: *manage },
                enrollment_ttl_seconds: *ttl_seconds, credential_ttl_seconds: *credential_ttl_seconds,
            }) {
                Ok(invitation) => invitation,
                Err(error) => {
                    drop(file);
                    let _ = std::fs::remove_file(&output);
                    return Err(error.into());
                }
            };
            credentials::write(&mut file, &invitation)?;
            let deadline = i64::try_from(invitation.enrollment.expires_at).ok()
                .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                .map(|expires| expires.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                .unwrap_or_else(|| format!("Unix timestamp {}", invitation.enrollment.expires_at));
            print_result(args.format, json!({
                "schema_version": 1, "operation": "user_invite", "user": invitation.principal,
                "collection": invitation.collection, "id": invitation.enrollment.id,
                "expires_at": invitation.enrollment.expires_at, "grants": invitation.enrollment.grants,
                "file": output
            }), format!("Invited user {}{} to {collection}; one-time enrollment saved to {}.\nEnrollment expires at {deadline}.\nSend that protected file to the member. They run ctx remote connect SERVER_URL and paste its compact JSON, or use --enrollment-file PATH.", invitation.principal, name.as_ref().map(|name| format!(" ({name:?})")).unwrap_or_default(), output.display()), ui)
        }
        ServerCommand::User { command: UserCommand::List { page } } => {
            print_users(args.format, client.list_principals(&page.request())?, ui)
        }
        ServerCommand::User { command: UserCommand::Credentials { user, page } } => {
            print_credentials(args.format, user, client.list_credentials(user, &page.request())?, ui)
        }
        ServerCommand::Grant { user, collection: selected, read, publish, manage } => {
            require_collection(selected.as_deref(), collection)?;
            let grants = Grants { read: *read, publish: *publish, manage: *manage };
            client.grant(&ctx_history_server::GrantRequest { principal: user.clone(), grants })?;
            print_result(args.format, json!({
                "schema_version": 1, "operation": "grant", "user": user, "collection": collection,
                "grants": grants
            }), format!("Set {user}'s rights in {collection}: read={read}, publish={publish}, manage={manage}"), ui)
        }
        ServerCommand::Revoke { user: Some(user), server_wide, .. }
            if *server_wide || args.remote.is_none() => {
            client.admin_revoke_principal(user)?;
            print_revoked(args.format, "user", user, ui)
        }
        ServerCommand::Revoke { credential: Some(credential), .. } => {
            client.admin_revoke_credential(credential)?;
            print_revoked(args.format, "credential", credential, ui)
        }
        ServerCommand::Revoke { user: Some(user), .. } => {
            client.revoke_member(user)?;
            print_result(args.format, json!({
                "schema_version": 1, "operation": "revoke", "kind": "membership", "id": user,
                "scope": "collection", "collection": collection, "retained_history_unchanged": true
            }), format!("Revoked {user}'s access to {collection}. Previously shared history is retained."), ui)
        }
        ServerCommand::Withdraw { collection: selected, publication, expected_revision, token_file } => {
            require_collection(selected.as_deref(), collection)?;
            if token_file.is_some() { bail!("remote administration uses the saved connection credential"); }
            let state = client.publication(publication)?;
            let receipt = client.remove(&removal(collection, &state, expected_revision.as_deref())?)?;
            print_result(args.format, json!({
                "schema_version": 1, "operation": "withdraw", "receipt": receipt
            }), format!("Withdrew publication {publication} from {collection}"), ui)
        }
        ServerCommand::Status => {
            let status = client.status()?;
            print_result(args.format, json!({
                "schema_version": 1, "operation": "server_status", "status": status
            }), format!("Collection {collection}: stored {}, searchable {}, reads available: {}",
                status.stored_sequence, status.searchable_sequence, status.reads_available), ui)
        }
        _ => bail!("this operation requires local --root administration; remote administration supports invite, user list, user credentials, publications, grant, revoke, withdraw, and status"),
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

pub(super) fn print_users(
    format: JsonOutputFormat,
    page: ctx_history_server::PrincipalPage,
    ui: &mut Ui,
) -> Result<()> {
    let mut text = format!(
        "Users across this server: {} on this page.",
        page.principals.len()
    );
    for user in &page.principals {
        text.push_str(&format!(
            "\n{}  label={}  server_owner={}  revoked={}",
            user.principal,
            user.name
                .as_ref()
                .map(|name| format!("{name:?}"))
                .unwrap_or_else(|| "(unnamed)".into()),
            user.server_owner,
            user.revoked,
        ));
    }
    if let Some(cursor) = &page.next_cursor {
        text.push_str(&format!("\nMore users: repeat with --after {cursor}"));
    }
    print_result(
        format,
        json!({
            "schema_version": 1, "operation": "user_list", "scope": "server",
            "users": page.principals, "next_cursor": page.next_cursor,
        }),
        text,
        ui,
    )
}

pub(super) fn print_credentials(
    format: JsonOutputFormat,
    user: &str,
    page: ctx_history_server::CredentialPage,
    ui: &mut Ui,
) -> Result<()> {
    let mut text = format!("Credentials for {user} across this server: {} on this page.\nRights are credential limits; ordinary credentials also require current user grants for their collection. Server-owner credentials authorize all collections.", page.credentials.len());
    for credential in &page.credentials {
        text.push_str(&format!(
            "\n{}  collection={}  read={} publish={} manage={}  server_owner={}  expires={}  revoked={}",
            credential.credential_id,
            credential.collection.as_deref().unwrap_or("all collections"),
            credential.grants.read, credential.grants.publish, credential.grants.manage,
            credential.server_owner,
            if credential.expires_at == 0 { "until revoked".into() } else { credential.expires_at.to_string() },
            credential.revoked,
        ));
        if let Some(enrollment_id) = &credential.enrollment_id {
            text.push_str(&format!("  enrollment_id={enrollment_id}"));
        }
    }
    if let Some(cursor) = &page.next_cursor {
        text.push_str(&format!("\nMore credentials: repeat with --after {cursor}"));
    }
    print_result(
        format,
        json!({
            "schema_version": 1, "operation": "user_credentials", "scope": "server",
            "user": user, "credentials": page.credentials, "next_cursor": page.next_cursor,
        }),
        text,
        ui,
    )
}

pub(super) fn print_revoked(
    format: JsonOutputFormat,
    kind: &str,
    id: &str,
    ui: &mut Ui,
) -> Result<()> {
    let affected = if kind == "user" {
        "All of this user's devices and pending invitations are revoked across all collections."
    } else {
        "Only this credential is revoked; other credentials and user grants are unchanged."
    };
    print_result(
        format,
        json!({
            "schema_version": 1, "operation": "revoke", "kind": kind, "id": id,
            "scope": "server", "retained_history_unchanged": true,
        }),
        format!("Revoked {kind} {id}. {affected}\nPreviously shared history is retained."),
        ui,
    )
}

fn require_collection(selected: Option<&str>, connected: &str) -> Result<()> {
    if selected.is_some_and(|collection| collection != connected) {
        bail!("selected collection differs from the saved connection");
    }
    Ok(())
}
