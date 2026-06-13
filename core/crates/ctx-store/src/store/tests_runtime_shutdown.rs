use super::tests::{create_session_with_turn, setup_store};
use super::*;

use anyhow::Result;
use serde_json::json;

#[tokio::test]
async fn store_close_stops_background_runtimes() -> Result<()> {
    let (_dir, store) = setup_store().await;
    let (session, turn_id) = create_session_with_turn(&store, None).await;
    let _ = store
        .append_session_event(
            session.id,
            None,
            Some(turn_id),
            SessionEventType::Notice,
            json!({"msg":"before close"}),
        )
        .await?;

    store.close().await;

    let enqueue_after_close = store
        .event_log
        .enqueue(SessionEvent {
            seq: 999,
            id: SessionEventId::new(),
            session_id: session.id,
            run_id: None,
            turn_id: Some(turn_id),
            event_type: SessionEventType::Notice,
            payload_json: json!({"msg":"after close"}),
            transient: false,
            created_at: Utc::now(),
        })
        .await;
    assert!(enqueue_after_close.is_err());

    let projection_after_close = store
        .active_head_projection
        .enqueue(session.id, Some(999))
        .await;
    assert!(projection_after_close.is_err());
    Ok(())
}
