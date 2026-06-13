use std::time::Duration;

use ctx_core::ids::{MessageId, TurnId};
use ctx_core::models::{MessageDelivery, Session, SessionEventType};
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::PostUserMessageInput;
use crate::daemon::scheduler::SchedulerCommand;
use crate::daemon::SessionMessageCommandHandle;
use crate::test_support::TestDaemon;

async fn seed_session_for_message_command_test(
) -> anyhow::Result<(tempfile::TempDir, TestDaemon, Session)> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().join("data");
    let repo_root = temp.path().join("repo");
    std::fs::create_dir_all(&repo_root)?;
    let daemon = TestDaemon::new_for_test(data_root, "http://127.0.0.1:0".to_string()).await?;
    let session = daemon
        .seed_title_generation_session_for_test(&repo_root)
        .await?;
    Ok((temp, daemon, session))
}

fn post_input(content: &str) -> PostUserMessageInput {
    PostUserMessageInput {
        message_id: MessageId::new(),
        turn_id: TurnId::new(),
        client_supplied_ids: false,
        content: content.to_string(),
        requested_delivery: Some(MessageDelivery::Immediate),
        attachments: Vec::new(),
        queued_messages_enabled: false,
        run_id_header: None,
    }
}

async fn capture_scheduler_commands(
    handle: &SessionMessageCommandHandle,
    session: Session,
) -> mpsc::Receiver<SchedulerCommand> {
    let (observed_tx, observed_rx) = mpsc::channel(8);
    handle
        .ensure_scheduler_for_test(session, move |_session, mut rx| async move {
            while let Some(command) = rx.recv().await {
                let _ = observed_tx.send(command).await;
            }
        })
        .await;
    observed_rx
}

#[tokio::test]
async fn delete_queued_message_removes_state_and_does_not_spawn_scheduler() -> anyhow::Result<()> {
    let (_temp, daemon, session) = seed_session_for_message_command_test().await?;
    let message_id = daemon
        .seed_global_id_routing_queued_message_for_test(session.id, "queued")
        .await?;
    let handle = daemon.session_message_command_handle_for_test();
    assert!(handle.scheduler_sender(session.id).await.is_none());

    handle
        .delete_queued_session_message(session.id, message_id)
        .await
        .expect("delete queued message");

    let store = daemon.store_for_session(session.id).await?;
    assert!(store.get_message(message_id).await?.is_none());
    assert!(handle.scheduler_sender(session.id).await.is_none());
    let events = store.list_session_events(session.id).await?;
    assert!(events.iter().any(|event| {
        matches!(&event.event_type, SessionEventType::MessageQueueRemoved)
            && event.payload_json["message_id"].as_str() == Some(&message_id.0.to_string())
    }));
    Ok(())
}

#[tokio::test]
async fn delete_queued_message_notifies_existing_scheduler() -> anyhow::Result<()> {
    let (_temp, daemon, session) = seed_session_for_message_command_test().await?;
    let message_id = daemon
        .seed_global_id_routing_queued_message_for_test(session.id, "queued")
        .await?;
    let handle = daemon.session_message_command_handle_for_test();
    let mut commands = capture_scheduler_commands(&handle, session.clone()).await;

    handle
        .delete_queued_session_message(session.id, message_id)
        .await
        .expect("delete queued message");

    let command = timeout(Duration::from_secs(2), commands.recv())
        .await?
        .expect("scheduler command");
    match command {
        SchedulerCommand::RemoveQueued(removed) => assert_eq!(removed, message_id),
        other => panic!("expected RemoveQueued, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn post_user_message_publishes_event_and_titles_only_first_message() -> anyhow::Result<()> {
    let (_temp, daemon, session) = seed_session_for_message_command_test().await?;
    daemon
        .ensure_workspace_active_snapshot_hydrated(session.workspace_id)
        .await
        .expect("hydrate workspace active snapshot");
    let handle = daemon.session_message_command_handle_for_test();
    let mut commands = capture_scheduler_commands(&handle, session.clone()).await;
    let mut event_head_rx = handle
        .subscribe_session_event_head_for_test(session.id)
        .await;
    let initial_event_head = *event_head_rx.borrow();

    let first = handle
        .post_user_message_for_request(session.id, post_input("first title prompt"))
        .await
        .expect("post first message");
    let first_command = timeout(Duration::from_secs(2), commands.recv())
        .await?
        .expect("first enqueue command");
    match first_command {
        SchedulerCommand::Enqueue(queued) => assert_eq!(queued.message.id, first.id),
        other => panic!("expected Enqueue, got {other:?}"),
    }
    assert_eq!(
        daemon.session_title_for_test(session.id).await?.as_deref(),
        Some("first title prompt")
    );
    timeout(Duration::from_secs(2), event_head_rx.changed()).await??;
    assert!(*event_head_rx.borrow() > initial_event_head);
    let store = daemon.store_for_session(session.id).await?;
    let events = store.list_session_events(session.id).await?;
    assert!(events.iter().any(|event| {
        matches!(&event.event_type, SessionEventType::UserMessage)
            && event.payload_json["message_id"].as_str() == Some(&first.id.0.to_string())
    }));

    let _second = handle
        .post_user_message_for_request(session.id, post_input("second title prompt"))
        .await
        .expect("post second message");
    let _ = timeout(Duration::from_secs(2), commands.recv())
        .await?
        .expect("second enqueue command");
    assert_eq!(
        daemon.session_title_for_test(session.id).await?.as_deref(),
        Some("first title prompt")
    );
    Ok(())
}
