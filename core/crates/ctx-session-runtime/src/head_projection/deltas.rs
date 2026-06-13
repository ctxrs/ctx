use ctx_core::models::{Session, SessionActivityState, SessionEventType, SessionSummaryDelta};

pub fn should_include_session_metadata_in_head_delta(event_type: &SessionEventType) -> bool {
    matches!(
        event_type,
        SessionEventType::Init | SessionEventType::Notice
    )
}

pub fn build_session_summary_delta(
    session: &Session,
    activity: Option<SessionActivityState>,
    last_message_at: Option<chrono::DateTime<chrono::Utc>>,
    last_message_preview: Option<String>,
    last_event_seq: i64,
    projection_rev: i64,
    state_rev: i64,
) -> Option<SessionSummaryDelta> {
    if activity.is_none() && last_message_at.is_none() && last_message_preview.is_none() {
        return None;
    }

    Some(SessionSummaryDelta {
        session_id: session.id,
        task_id: session.task_id,
        activity,
        last_message_at,
        last_message_preview,
        last_event_seq: Some(last_event_seq),
        projection_rev: Some(projection_rev),
        state_rev: Some(state_rev),
        emitted_at_ms: Some(chrono::Utc::now().timestamp_millis()),
    })
}

pub async fn resolve_projection_rev_for_stream_delta<F, Fut>(
    stream_only: bool,
    last_event_seq: i64,
    cached_projection_rev: i64,
    load_projection_rev: F,
) -> i64
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<i64>>,
{
    if stream_only {
        return cached_projection_rev.max(0);
    }
    load_projection_rev().await.unwrap_or(last_event_seq.max(0))
}
