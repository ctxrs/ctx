pub(super) fn decode_workspace_stream_message(
    txt: &str,
) -> ctx_core::models::WorkspaceActiveSnapshotStreamMessage {
    serde_json::from_str(txt)
        .unwrap_or_else(|err| panic!("failed to decode workspace stream message: {err}; raw={txt}"))
}

pub(super) fn workspace_stream_subscription_seed_received(
    message: &ctx_core::models::WorkspaceActiveSnapshotStreamMessage,
    session_id: ctx_core::ids::SessionId,
) -> bool {
    match message {
        ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Snapshot { .. } => true,
        ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event { event, .. } => {
            matches!(
                event.as_ref(),
                ctx_core::models::WorkspaceActiveSnapshotEvent::SessionHeadSeed { head, .. }
                    if head.session.id == session_id
            )
        }
        ctx_core::models::WorkspaceActiveSnapshotStreamMessage::HeadsBatch { deltas, .. } => {
            deltas.iter().any(|delta| delta.session_id == session_id)
        }
        _ => false,
    }
}

pub(super) fn workspace_stream_message_has_done_event(
    message: &ctx_core::models::WorkspaceActiveSnapshotStreamMessage,
    session_id: ctx_core::ids::SessionId,
) -> bool {
    match message {
        ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event { event, .. } => {
            workspace_active_event_has_done_event(event.as_ref(), session_id)
        }
        ctx_core::models::WorkspaceActiveSnapshotStreamMessage::HeadsBatch { deltas, .. } => {
            deltas.iter().any(|delta| {
                delta.session_id == session_id && session_delta_has_done_event(delta.event.as_ref())
            })
        }
        _ => false,
    }
}

pub(super) fn workspace_stream_message_has_terminal_failure(
    message: &ctx_core::models::WorkspaceActiveSnapshotStreamMessage,
    session_id: ctx_core::ids::SessionId,
) -> bool {
    match message {
        ctx_core::models::WorkspaceActiveSnapshotStreamMessage::Event { event, .. } => {
            workspace_active_event_has_terminal_failure(event.as_ref(), session_id)
        }
        ctx_core::models::WorkspaceActiveSnapshotStreamMessage::HeadsBatch { deltas, .. } => {
            deltas.iter().any(|delta| {
                delta.session_id == session_id
                    && session_delta_has_terminal_failure(delta.event.as_ref())
            })
        }
        _ => false,
    }
}

fn workspace_active_event_has_done_event(
    event: &ctx_core::models::WorkspaceActiveSnapshotEvent,
    session_id: ctx_core::ids::SessionId,
) -> bool {
    match event {
        ctx_core::models::WorkspaceActiveSnapshotEvent::SessionHeadDelta { delta, .. } => {
            delta.session_id == session_id && session_delta_has_done_event(delta.event.as_ref())
        }
        ctx_core::models::WorkspaceActiveSnapshotEvent::SessionHeadSeed { head, .. } => {
            head.session.id == session_id
                && head.events.iter().any(|event| {
                    matches!(event.event_type, ctx_core::models::SessionEventType::Done)
                })
        }
        _ => false,
    }
}

fn workspace_active_event_has_terminal_failure(
    event: &ctx_core::models::WorkspaceActiveSnapshotEvent,
    session_id: ctx_core::ids::SessionId,
) -> bool {
    match event {
        ctx_core::models::WorkspaceActiveSnapshotEvent::SessionHeadDelta { delta, .. } => {
            delta.session_id == session_id
                && session_delta_has_terminal_failure(delta.event.as_ref())
        }
        ctx_core::models::WorkspaceActiveSnapshotEvent::SessionHeadSeed { head, .. } => {
            head.session.id == session_id
                && head.events.iter().any(session_event_is_terminal_failure)
        }
        _ => false,
    }
}

fn session_delta_has_done_event(event: Option<&ctx_core::models::SessionEvent>) -> bool {
    event
        .map(|event| matches!(event.event_type, ctx_core::models::SessionEventType::Done))
        .unwrap_or(false)
}

fn session_delta_has_terminal_failure(event: Option<&ctx_core::models::SessionEvent>) -> bool {
    event
        .map(session_event_is_terminal_failure)
        .unwrap_or(false)
}

fn session_event_is_terminal_failure(event: &ctx_core::models::SessionEvent) -> bool {
    if matches!(
        event.event_type,
        ctx_core::models::SessionEventType::TurnInterrupted
    ) {
        return true;
    }

    matches!(
        event.event_type,
        ctx_core::models::SessionEventType::TurnFinished
    ) && matches!(
        ctx_core::session_projection::terminal_status_from_finished_payload(&event.payload_json),
        Some(
            ctx_core::models::SessionTurnStatus::Failed
                | ctx_core::models::SessionTurnStatus::Interrupted
        )
    )
}
