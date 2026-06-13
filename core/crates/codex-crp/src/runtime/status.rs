use super::AppServerSessionState;
use crate::app_server::{ThreadLoadedListResponse, ThreadReadResponse, ThreadStatus};
use anyhow::Result;
use serde_json::{json, Value};

pub(super) fn build_session_status_details(
    root_thread_id: &str,
    active_turn_id: Option<String>,
    resumed_from_provider_session: bool,
    command_execution_seen: bool,
    thread_statuses: Vec<ThreadStatusSnapshot>,
) -> Value {
    let mut loaded_thread_ids = Vec::new();
    let mut active_thread_ids = Vec::new();
    let mut system_error_thread_ids = Vec::new();
    let mut busy_reasons = Vec::new();

    for snapshot in thread_statuses {
        let ThreadStatusSnapshot { thread_id, status } = snapshot;
        loaded_thread_ids.push(thread_id.clone());
        match status {
            ThreadStatus::Active { .. } => active_thread_ids.push(thread_id),
            ThreadStatus::SystemError => system_error_thread_ids.push(thread_id),
            ThreadStatus::NotLoaded | ThreadStatus::Idle => {}
        }
    }

    loaded_thread_ids.sort();
    loaded_thread_ids.dedup();
    active_thread_ids.sort();
    active_thread_ids.dedup();
    system_error_thread_ids.sort();
    system_error_thread_ids.dedup();

    if active_turn_id.is_some() {
        busy_reasons.push("active_turn".to_string());
    }
    if !active_thread_ids.is_empty() {
        busy_reasons.push("loaded_thread_active".to_string());
    }
    json!({
        "quiescent": active_turn_id.is_none()
            && active_thread_ids.is_empty(),
        "root_thread_id": root_thread_id,
        "active_turn_id": active_turn_id,
        "loaded_thread_ids": loaded_thread_ids,
        "active_thread_ids": active_thread_ids,
        "system_error_thread_ids": system_error_thread_ids,
        "resumed_from_provider_session": resumed_from_provider_session,
        "command_execution_observed": command_execution_seen,
        "busy_reasons": busy_reasons,
    })
}

#[derive(Debug, Clone)]
pub(super) struct ThreadStatusSnapshot {
    pub(super) thread_id: String,
    pub(super) status: ThreadStatus,
}

pub(super) async fn query_session_status(session: &mut AppServerSessionState) -> Result<Value> {
    let loaded = session
        .client
        .request::<ThreadLoadedListResponse>("thread/loaded/list", json!({}))
        .await?;
    let mut loaded_thread_ids = loaded.data;
    if !loaded_thread_ids
        .iter()
        .any(|thread_id| thread_id == &session.thread_id)
    {
        loaded_thread_ids.push(session.thread_id.clone());
    }
    loaded_thread_ids.sort();
    loaded_thread_ids.dedup();

    let mut thread_statuses = Vec::new();
    let active_turn_id = session.turn_aliases.active_crp_turn_id.clone();

    for thread_id in &loaded_thread_ids {
        let response = session
            .client
            .request::<ThreadReadResponse>(
                "thread/read",
                json!({
                    "threadId": thread_id,
                    "includeTurns": false,
                }),
            )
            .await?;
        thread_statuses.push(ThreadStatusSnapshot {
            thread_id: response.thread.id,
            status: response.thread.status,
        });
    }
    Ok(build_session_status_details(
        &session.thread_id,
        active_turn_id,
        session.resumed_from_provider_session,
        session.command_execution_seen,
        thread_statuses,
    ))
}
