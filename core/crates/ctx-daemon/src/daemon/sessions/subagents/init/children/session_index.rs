use ctx_core::models::Session;

use super::SubagentChildInit;

pub(super) async fn index_child_session(
    init: &SubagentChildInit,
    store: &ctx_store::Store,
    session: &Session,
    label: &str,
) {
    if let Err(error) = init
        .host
        .upsert_workspace_session_index(session.id, init.parent.workspace_id)
        .await
    {
        tracing::warn!(
            session_id = %session.id.0,
            "failed to update subagent session index: {error:?}"
        );
    }
    if store
        .update_session_title(session.id, label.to_string())
        .await
        .is_err()
    {
        tracing::warn!(session_id = %session.id.0, "failed to set subagent label");
    }
}
