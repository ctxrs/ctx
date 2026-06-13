use super::*;
use workspace_stream_matchers::{
    decode_workspace_stream_message, workspace_stream_message_has_done_event,
    workspace_stream_message_has_terminal_failure, workspace_stream_subscription_seed_received,
};

type WorkspaceStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

mod workspace_stream_matchers;

pub(super) async fn wait_for_subscription_seed(
    ws_stream: &mut WorkspaceStream,
    session_id: ctx_core::ids::SessionId,
) {
    let subscribed = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(frame)) = ws_stream.next().await {
            if let tokio_tungstenite::tungstenite::Message::Text(txt) = frame {
                let message = decode_workspace_stream_message(&txt);
                if workspace_stream_subscription_seed_received(&message, session_id) {
                    return true;
                }
            }
        }
        false
    })
    .await
    .expect("timed out waiting for workspace stream subscription seed");
    assert!(
        subscribed,
        "workspace stream ended before subscription seed"
    );
}

pub(super) async fn wait_for_done_event(
    ws_stream: &mut WorkspaceStream,
    session_id: ctx_core::ids::SessionId,
) {
    let seen_done = tokio::time::timeout(Duration::from_secs(60), async {
        let mut frames = Vec::new();
        while let Some(Ok(frame)) = ws_stream.next().await {
            if let tokio_tungstenite::tungstenite::Message::Text(txt) = frame {
                let message = decode_workspace_stream_message(&txt);
                if workspace_stream_message_has_terminal_failure(&message, session_id) {
                    panic!(
                        "turn failed before Done event; frames={frames:#?}; message={message:#?}"
                    );
                }
                if workspace_stream_message_has_done_event(&message, session_id) {
                    return true;
                }
                frames.push(message);
            }
        }
        false
    })
    .await
    .expect("timed out waiting for Done event");
    assert!(seen_done);
}

pub(super) async fn assert_user_message_persisted(
    daemon: &TestDaemon,
    session_id: ctx_core::ids::SessionId,
) {
    assert!(daemon
        .session_has_user_message_event_for_test(session_id)
        .await
        .unwrap());
}

pub(super) async fn assert_task_read_unread_round_trip(
    client: &reqwest::Client,
    base: &str,
    task_id: ctx_core::ids::TaskId,
) {
    let task_after_read: ctx_core::models::Task = client
        .post(format!("{base}/api/tasks/{}/mark_read", task_id.0))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(task_after_read.assistant_seen_at.is_some());

    let task_after_unread: ctx_core::models::Task = client
        .post(format!("{base}/api/tasks/{}/mark_unread", task_id.0))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(task_after_unread.assistant_seen_at.is_none());
}
