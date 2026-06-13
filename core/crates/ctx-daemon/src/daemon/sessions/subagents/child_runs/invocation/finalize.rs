use super::*;

pub(in crate::daemon::sessions::subagents) async fn finalize_subagent_invocation(
    host: &SubagentChildRunHost,
    invocation_id: &str,
    tool_call_id: &str,
    parent_session_id: SessionId,
    parent_turn_id: Option<TurnId>,
) -> Result<(), String> {
    let store = match host
        .store_for_session_allow_archived(parent_session_id)
        .await
    {
        Ok(Some(store)) => store,
        Ok(None) => return Ok(()),
        Err(SessionStoreAccessError::NotFound) => return Ok(()),
        Err(error) => {
            return Err(logs::redact_sensitive(
                &session_store_access_anyhow(error).to_string(),
            ));
        }
    };
    let Some(invocation) = store
        .get_subagent_invocation(invocation_id)
        .await
        .map_err(|error| logs::redact_sensitive(&error.to_string()))?
    else {
        return Ok(());
    };

    if invocation.children.is_empty() {
        return Ok(());
    }
    if invocation
        .children
        .iter()
        .any(|child| child.status == "running")
    {
        return Ok(());
    }

    let final_status = if invocation
        .children
        .iter()
        .all(|child| child.status == "completed")
    {
        "completed"
    } else {
        "failed"
    };
    if invocation.status == final_status {
        return Ok(());
    }

    let updated_at = chrono::Utc::now();
    store
        .update_subagent_invocation_status(invocation_id, final_status, updated_at)
        .await
        .map_err(|error| logs::redact_sensitive(&error.to_string()))?;
    let child_session_ids = invocation
        .children
        .iter()
        .map(|child| child.child_session_id.0.to_string())
        .collect::<Vec<_>>();
    let child_statuses = invocation
        .children
        .iter()
        .map(|child| {
            serde_json::json!({
                "session_id": child.child_session_id.0.to_string(),
                "status": child.status,
            })
        })
        .collect::<Vec<_>>();
    emit_subagent_invocation_notice(
        host,
        parent_session_id,
        parent_turn_id,
        serde_json::json!({
            "kind": "subagent_invocation_updated",
            "invocation_id": invocation_id,
            "tool_call_id": tool_call_id,
            "status": final_status,
            "child_session_ids": child_session_ids,
            "child_statuses": child_statuses,
        }),
    )
    .await
    .map_err(|error| error.message().to_string())?;

    Ok(())
}
