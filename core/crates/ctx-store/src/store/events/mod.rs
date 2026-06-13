use super::*;

// Keep stream-only seq values within JS safe integer range.
const STREAM_ONLY_EVENT_SEQ_START: i64 = -(1_i64 << 52);
static STREAM_ONLY_EVENT_SEQ: AtomicI64 = AtomicI64::new(STREAM_ONLY_EVENT_SEQ_START);

fn next_stream_only_event_seq() -> i64 {
    STREAM_ONLY_EVENT_SEQ.fetch_add(1, Ordering::Relaxed)
}

fn is_terminal_session_event(event_type: &SessionEventType) -> bool {
    matches!(
        event_type,
        SessionEventType::TurnFinished | SessionEventType::TurnInterrupted
    )
}

fn ensure_supported_session_event_type(event_type: &SessionEventType) -> Result<()> {
    if matches!(event_type, SessionEventType::Error) {
        anyhow::bail!("session error events are not durable; use failed turn_finished");
    }
    Ok(())
}

include!("writes.rs");
include!("reads.rs");
