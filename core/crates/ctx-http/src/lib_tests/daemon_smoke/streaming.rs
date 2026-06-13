use super::*;

mod assertions;
mod fixture;

use assertions::{
    assert_task_read_unread_round_trip, assert_user_message_persisted, wait_for_done_event,
    wait_for_subscription_seed,
};
use fixture::{create_default_task_session, start_streaming_server};

#[tokio::test]
async fn daemon_http_and_ws_streaming() {
    let _serial = home_env_test_lock().lock().await;
    let harness = start_streaming_server().await;
    let (workspace, task, session) = create_default_task_session(&harness).await;

    let ws_url = format!(
        "ws://{}/api/workspaces/{}/stream",
        harness.addr, workspace.id.0
    );
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
    let subscribe = serde_json::json!({
        "type": "subscribe",
        "include_active_heads": true,
        "sessions": [{
            "session_id": session.id.0,
            "replay": {
                "mode": "resume",
                "after_seq": 0,
            },
        }],
    })
    .to_string();
    ws_stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            subscribe.into(),
        ))
        .await
        .unwrap();
    wait_for_subscription_seed(&mut ws_stream, session.id).await;

    let _msg: ctx_core::models::Message = harness
        .client
        .post(format!(
            "{}/api/sessions/{}/messages",
            harness.base, session.id.0
        ))
        .json(&json!({"content":"hello"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    wait_for_done_event(&mut ws_stream, session.id).await;
    assert_user_message_persisted(harness.daemon(), session.id).await;
    assert_task_read_unread_round_trip(&harness.client, &harness.base, task.id).await;
}
