use super::*;

mod fixtures;

use fixtures::{
    assert_control_session_gap, expect_heads_batch, push_control_gap, PriorityOrderingFixture,
};

#[test]
fn background_head_batch_chunk_limit_keeps_foreground_preemption_fine_grained() {
    const {
        assert!(
            BACKGROUND_HEAD_BATCH_CHUNK_LIMIT <= 16,
            "background head batches must remain small enough for foreground websocket preemption",
        );
    }
}

#[tokio::test]
async fn next_workspace_stream_item_prioritizes_foreground_lane() {
    let fixture = PriorityOrderingFixture::new();
    let workspace_id = WorkspaceId::new();
    let foreground_session_id = SessionId::new();
    let background_session_id = SessionId::new();

    push_control_gap(
        &fixture.control,
        workspace_id,
        background_session_id,
        "test_control",
        1,
        10,
        "background",
    )
    .await
    .expect("control event should enqueue");
    fixture
        .foreground_head_buffer
        .push(2, partial_delta(foreground_session_id))
        .await
        .expect("foreground delta should enqueue");
    fixture
        .background_head_buffer
        .push(3, partial_delta(background_session_id))
        .await
        .expect("background delta should enqueue");
    push_control_gap(
        &fixture.priority_control,
        workspace_id,
        foreground_session_id,
        "test_priority",
        4,
        11,
        "foreground",
    )
    .await
    .expect("priority control event should enqueue");

    assert_control_session_gap(
        fixture.next().await,
        foreground_session_id,
        "expected priority control event first",
    );

    let deltas = expect_heads_batch(
        fixture.next().await,
        HeadBatchLane::Foreground,
        "expected foreground heads batch second",
    );
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].session_id, foreground_session_id);

    assert_control_session_gap(
        fixture.next().await,
        background_session_id,
        "expected background control event third",
    );

    let deltas = expect_heads_batch(
        fixture.next().await,
        HeadBatchLane::Background,
        "expected background heads batch last",
    );
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].session_id, background_session_id);
}

#[tokio::test]
async fn background_heads_are_chunked_and_recheck_foreground_priority() {
    let fixture = PriorityOrderingFixture::new();
    let foreground_session_id = SessionId::new();
    let background_session_id = SessionId::new();

    for seq in 0..(BACKGROUND_HEAD_BATCH_CHUNK_LIMIT + 5) {
        fixture
            .background_head_buffer
            .push(
                seq as i64,
                cursor_delta(background_session_id, seq as i64 + 1),
            )
            .await
            .expect("background delta should enqueue");
    }

    let deltas = expect_heads_batch(
        fixture.next().await,
        HeadBatchLane::Background,
        "expected first background chunk",
    );
    assert_eq!(deltas.len(), BACKGROUND_HEAD_BATCH_CHUNK_LIMIT);

    fixture
        .foreground_head_buffer
        .push(200, cursor_delta(foreground_session_id, 1))
        .await
        .expect("foreground delta should enqueue");

    let deltas = expect_heads_batch(
        fixture.next().await,
        HeadBatchLane::Foreground,
        "expected foreground chunk before background remainder",
    );
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].session_id, foreground_session_id);

    let deltas = expect_heads_batch(
        fixture.next().await,
        HeadBatchLane::Background,
        "expected remaining background chunk",
    );
    assert_eq!(deltas.len(), 5);
}
