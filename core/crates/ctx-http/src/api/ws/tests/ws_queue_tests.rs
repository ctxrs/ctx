use super::*;
use chrono::Utc;
use serde_json::json;
use std::collections::HashMap;

#[test]
fn terminal_ws_queue_enqueues_while_capacity_is_available() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    assert_eq!(
        queue_terminal_ws_message(&tx, WsMessage::Text("one".to_string())),
        TerminalWsQueueOutcome::Enqueued
    );
    assert!(matches!(rx.try_recv(), Ok(WsMessage::Text(text)) if text == "one"));
}

#[test]
fn terminal_ws_queue_drops_new_message_when_full() {
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    assert_eq!(
        queue_terminal_ws_message(&tx, WsMessage::Text("one".to_string())),
        TerminalWsQueueOutcome::Enqueued
    );
    assert_eq!(
        queue_terminal_ws_message(&tx, WsMessage::Text("two".to_string())),
        TerminalWsQueueOutcome::Dropped
    );
}

#[test]
fn terminal_ws_queue_reports_closed_when_writer_is_gone() {
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);
    assert_eq!(
        queue_terminal_ws_message(&tx, WsMessage::Text("one".to_string())),
        TerminalWsQueueOutcome::Closed
    );
}

#[test]
fn terminal_ws_tail_resync_waits_until_capacity_returns() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let needs_tail_resync = std::sync::atomic::AtomicBool::new(true);
    assert_eq!(
        queue_terminal_ws_message(&tx, WsMessage::Text("occupied".to_string())),
        TerminalWsQueueOutcome::Enqueued
    );

    assert!(matches!(
        queue_terminal_ws_tail_resync_if_requested(
            &tx,
            || b"tail-marker".to_vec(),
            &needs_tail_resync
        ),
        Some(TerminalWsQueueOutcome::Dropped)
    ));
    assert!(needs_tail_resync.load(std::sync::atomic::Ordering::Acquire));

    assert!(matches!(rx.try_recv(), Ok(WsMessage::Text(text)) if text == "occupied"));

    assert!(matches!(
        queue_terminal_ws_tail_resync_if_requested(
            &tx,
            || b"tail-marker".to_vec(),
            &needs_tail_resync
        ),
        Some(TerminalWsQueueOutcome::Enqueued)
    ));
    assert!(!needs_tail_resync.load(std::sync::atomic::Ordering::Acquire));
    assert!(matches!(rx.try_recv(), Ok(WsMessage::Binary(bytes)) if bytes == b"tail-marker"));
}

fn make_partial_delta(session_id: SessionId, turn_id: TurnId, fragment: &str) -> SessionHeadDelta {
    let event = SessionEvent {
        seq: -1,
        id: SessionEventId::new(),
        session_id,
        run_id: None,
        turn_id: Some(turn_id),
        event_type: SessionEventType::AssistantChunk,
        payload_json: json!({ "content_fragment": fragment }),
        transient: true,
        created_at: Utc::now(),
    };
    SessionHeadDelta {
        session_id,
        last_event_seq: 0,
        projection_rev: 0,
        state_rev: 0,
        emitted_at_ms: None,
        session: None,
        activity: None,
        event: Some(event),
        turn: None,
        message: None,
        tool_summaries: Vec::new(),
    }
}

fn fragment_from_delta(delta: &SessionHeadDelta) -> String {
    delta
        .event
        .as_ref()
        .and_then(|event| event.payload_json.get("content_fragment"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn head_batch_coalesces_partials_per_session() {
    let buffer = HeadBatchBuffer::new();
    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let turn_a = TurnId::new();
    let turn_b = TurnId::new();
    let limit = HEAD_BATCH_SESSION_LIMIT + 25;

    for i in 0..limit {
        let fragment = format!("a-{i}-");
        buffer
            .push(1, make_partial_delta(session_a, turn_a, &fragment))
            .await
            .expect("partial burst should coalesce");
        if i % 5 == 0 {
            let fragment_b = format!("b-{i}-");
            buffer
                .push(1, make_partial_delta(session_b, turn_b, &fragment_b))
                .await
                .expect("partial burst should coalesce");
        }
    }

    let (_, deltas) = buffer.take().await;
    let mut by_session = HashMap::new();
    for delta in deltas {
        by_session.insert(delta.session_id, delta);
    }
    assert_eq!(by_session.len(), 2);

    let merged_a = fragment_from_delta(by_session.get(&session_a).unwrap());
    assert!(merged_a.contains("a-0-"));
    assert!(merged_a.contains(&format!("a-{}-", limit - 1)));
}
