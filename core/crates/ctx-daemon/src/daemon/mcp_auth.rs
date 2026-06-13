use anyhow::Error;
use ctx_core::ids::{SessionId, WorkspaceId, WorktreeId};
use ctx_observability::ops_events::OpsEvents;

use crate::daemon::{DaemonState, SessionStoreAccessError};

mod events;

use ctx_mcp_auth::{McpAuthCapabilities, McpAuthContext};
pub use events::emit_mcp_token_denied;
use events::emit_mcp_token_event_with_ops;

pub async fn issue_provider_session_mcp_token(
    state: &DaemonState,
    session_id: SessionId,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
) -> String {
    issue_provider_session_mcp_token_with_capabilities(
        state,
        session_id,
        workspace_id,
        worktree_id,
        McpAuthCapabilities::provider_session(),
    )
    .await
}

pub async fn issue_provider_session_mcp_token_with_capabilities(
    state: &DaemonState,
    session_id: SessionId,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    capabilities: McpAuthCapabilities,
) -> String {
    issue_provider_session_mcp_token_with_capabilities_parts(
        state.core.mcp_auth.as_ref(),
        &state.telemetry.ops_events,
        session_id,
        workspace_id,
        worktree_id,
        capabilities,
    )
    .await
}

pub async fn revoke_provider_session_mcp_token(state: &DaemonState, token: &str) -> bool {
    revoke_provider_session_mcp_token_parts(
        state.core.mcp_auth.as_ref(),
        &state.telemetry.ops_events,
        token,
    )
    .await
}

pub async fn verify_mcp_auth_token(state: &DaemonState, token: &str) -> Option<McpAuthContext> {
    state.core.mcp_auth.verify_token(token).await
}

pub async fn issue_provider_session_mcp_token_with_capabilities_parts(
    mcp_auth: &ctx_mcp_auth::McpAuthRegistry,
    ops_events: &OpsEvents,
    session_id: SessionId,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    capabilities: McpAuthCapabilities,
) -> String {
    let issued = mcp_auth
        .issue_provider_session_token_with_capabilities(
            session_id,
            workspace_id,
            worktree_id,
            capabilities,
        )
        .await;
    if issued.replaced_count > 0 {
        emit_mcp_token_event_with_ops(
            ops_events,
            "info",
            "mcp_token_revoked",
            issued.context,
            serde_json::json!({ "reason": "replaced", "count": issued.replaced_count }),
        );
    }
    emit_mcp_token_event_with_ops(
        ops_events,
        "info",
        "mcp_token_issued",
        issued.context,
        serde_json::json!({ "reason": "provider_session" }),
    );
    issued.token
}

pub async fn revoke_provider_session_mcp_token_parts(
    mcp_auth: &ctx_mcp_auth::McpAuthRegistry,
    ops_events: &OpsEvents,
    token: &str,
) -> bool {
    if let Some(ctx) = mcp_auth.revoke_provider_session_token(token).await {
        emit_mcp_token_event_with_ops(
            ops_events,
            "info",
            "mcp_token_revoked",
            ctx,
            serde_json::json!({ "reason": "explicit" }),
        );
        return true;
    }
    false
}

#[derive(Debug)]
pub enum ScopedMcpSessionAccessError {
    Unauthorized(&'static str),
    SessionNotFound,
    StoreUnavailable(Error),
}

pub async fn require_scoped_mcp_session_context(
    state: &DaemonState,
    mcp_auth: McpAuthContext,
    session_id: SessionId,
) -> Result<(), ScopedMcpSessionAccessError> {
    if mcp_auth.session_id != session_id {
        return Err(ScopedMcpSessionAccessError::Unauthorized(
            "scoped ctx-mcp token is limited to the current session",
        ));
    }

    let store = state
        .existing_session_store(session_id)
        .await
        .map_err(scoped_mcp_session_store_error)?;
    let session = store
        .get_session(session_id)
        .await
        .map_err(ScopedMcpSessionAccessError::StoreUnavailable)?
        .ok_or(ScopedMcpSessionAccessError::SessionNotFound)?;

    if session.workspace_id != mcp_auth.workspace_id || session.worktree_id != mcp_auth.worktree_id
    {
        return Err(ScopedMcpSessionAccessError::Unauthorized(
            "scoped ctx-mcp token does not match the loaded session scope",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests;

fn scoped_mcp_session_store_error(error: SessionStoreAccessError) -> ScopedMcpSessionAccessError {
    match error {
        SessionStoreAccessError::NotFound => ScopedMcpSessionAccessError::SessionNotFound,
        SessionStoreAccessError::LookupUnavailable(error) => {
            ScopedMcpSessionAccessError::StoreUnavailable(error)
        }
        SessionStoreAccessError::StoreUnavailable => ScopedMcpSessionAccessError::StoreUnavailable(
            anyhow::anyhow!("workspace store unavailable"),
        ),
    }
}
