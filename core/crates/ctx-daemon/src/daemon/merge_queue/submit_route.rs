use ctx_core::ids::{SessionId, WorktreeId};
use ctx_mcp_auth::McpAuthContext;
use ctx_merge_queue::MergeQueueSubmitParams;
use ctx_observability::logs;
use ctx_route_contracts::merge_queue::{
    MergeQueueEntryRouteResponse, MergeQueueSubmitRouteError, SubmitMergeQueueEntryRouteRequest,
};

use crate::daemon::{MergeQueueApiHandle, ScopedMcpSessionAccessError};

impl MergeQueueApiHandle {
    pub async fn submit_merge_queue_entry_for_route(
        &self,
        req: SubmitMergeQueueEntryRouteRequest,
        mcp_auth: Option<McpAuthContext>,
    ) -> Result<MergeQueueEntryRouteResponse, MergeQueueSubmitRouteError> {
        let (raw_session_id, raw_worktree_id, raw_worktree_root, target_branch, message) =
            req.into_parts();
        let mut session_id = parse_optional_session_id(raw_session_id.as_deref())?;
        let mut worktree_id = parse_optional_worktree_id(raw_worktree_id.as_deref())?;
        let worktree_root = normalized_worktree_root(raw_worktree_root);

        if let Some(mcp_auth) = mcp_auth {
            if worktree_root.is_some() {
                return Err(MergeQueueSubmitRouteError::unauthorized(
                    "scoped ctx-mcp merge queue submit cannot override worktree_root",
                ));
            }
            let scoped_session_id = session_id.unwrap_or(mcp_auth.session_id);
            let scoped_worktree_id = worktree_id.unwrap_or(mcp_auth.worktree_id);
            if !mcp_auth.allows_merge_queue_submit(scoped_session_id, scoped_worktree_id) {
                return Err(MergeQueueSubmitRouteError::unauthorized(
                    "scoped ctx-mcp merge queue submit is limited to the current session and worktree",
                ));
            }
            self.require_scoped_mcp_session_context(mcp_auth, scoped_session_id)
                .await
                .map_err(scoped_mcp_session_route_error)?;
            session_id = Some(scoped_session_id);
            worktree_id = Some(scoped_worktree_id);
        }

        self.submit_merge_queue_entry(MergeQueueSubmitParams {
            session_id,
            worktree_id,
            worktree_root,
            target_branch,
            message,
        })
        .await
        .map(Into::into)
        .map_err(|error| MergeQueueSubmitRouteError::bad_request(error.to_string()))
    }
}

fn parse_optional_session_id(
    raw: Option<&str>,
) -> Result<Option<SessionId>, MergeQueueSubmitRouteError> {
    raw.map(|id| {
        uuid::Uuid::parse_str(id)
            .map(SessionId)
            .map_err(|_| MergeQueueSubmitRouteError::bad_request("invalid session_id"))
    })
    .transpose()
}

fn parse_optional_worktree_id(
    raw: Option<&str>,
) -> Result<Option<WorktreeId>, MergeQueueSubmitRouteError> {
    raw.map(|id| {
        uuid::Uuid::parse_str(id)
            .map(WorktreeId)
            .map_err(|_| MergeQueueSubmitRouteError::bad_request("invalid worktree_id"))
    })
    .transpose()
}

fn normalized_worktree_root(raw: Option<String>) -> Option<String> {
    raw.and_then(|root| {
        let trimmed = root.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn scoped_mcp_session_route_error(
    error: ScopedMcpSessionAccessError,
) -> MergeQueueSubmitRouteError {
    match error {
        ScopedMcpSessionAccessError::Unauthorized(message) => {
            MergeQueueSubmitRouteError::unauthorized(message)
        }
        ScopedMcpSessionAccessError::SessionNotFound => {
            MergeQueueSubmitRouteError::not_found("session not found")
        }
        ScopedMcpSessionAccessError::StoreUnavailable(error) => {
            MergeQueueSubmitRouteError::internal(logs::redact_sensitive(&error.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_route_contracts::merge_queue::MergeQueueSubmitRouteErrorKind;

    #[test]
    fn route_id_parsing_preserves_public_bad_request_messages() {
        let error = parse_optional_session_id(Some("not-a-uuid")).unwrap_err();
        assert_eq!(error.kind(), MergeQueueSubmitRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid session_id");

        let error = parse_optional_worktree_id(Some("not-a-uuid")).unwrap_err();
        assert_eq!(error.kind(), MergeQueueSubmitRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid worktree_id");
    }

    #[test]
    fn route_worktree_root_normalization_trims_and_drops_empty_values() {
        assert_eq!(normalized_worktree_root(None), None);
        assert_eq!(normalized_worktree_root(Some("   ".to_string())), None);
        assert_eq!(
            normalized_worktree_root(Some("  /tmp/worktree  ".to_string())),
            Some("/tmp/worktree".to_string())
        );
    }
}
