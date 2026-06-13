use super::*;

#[tokio::test]
async fn start_deadline_failure_finalizes_starting_turn_as_failed() {
    let data_dir = tempdir().expect("temp dir");
    let fixture = build_loop_fixture(data_dir.path(), "fake", "model").await;
    let adapter: Arc<dyn ProviderAdapter> = Arc::new(FakeProviderAdapter::new());
    let (event_tx, _event_rx) = mpsc::channel(8);
    let handle = adapter
        .run(
            TurnInput {
                content: "slow-diff-test".to_string(),
                attachments: Vec::new(),
                context_blocks: Vec::new(),
                model_id: None,
            },
            fixture.workspace_root.clone(),
            HashMap::new(),
            event_tx.clone(),
            ProviderRunHooks::default(),
        )
        .await
        .expect("run handle");
    let (_start_progress_tx, start_progress_rx) =
        tokio::sync::watch::channel(TurnStartProgress::Pending);

    fixture
        .state
        .session_scheduler_worker_host
        .worker_host()
        .fail_starting_turn(
            fixture.session_id,
            RunningTurn {
                adapter,
                handle,
                run_id: fixture.run_id,
                turn_id: fixture.turn_id,
                message_id: fixture.message_id,
                provider_id: "fake".to_string(),
                model_id: "model".to_string(),
                execution_environment_label: "host".to_string(),
                session_root_kind: "primary".to_string(),
                event_tx,
                events_done: None,
                start_progress: start_progress_rx,
                start_deadline: tokio::time::Instant::now(),
                mcp_token: None,
            },
            "provider did not report turn start before deadline",
        )
        .await;

    let turn = fixture
        .store
        .get_session_turn(fixture.session_id, fixture.turn_id)
        .await
        .expect("load turn")
        .expect("turn exists");
    assert_eq!(turn.status, SessionTurnStatus::Failed);

    let events = fixture
        .store
        .list_session_events_for_turn(fixture.session_id, fixture.turn_id, false)
        .await
        .expect("load turn events");
    assert!(events.iter().any(|event| {
        matches!(&event.event_type, SessionEventType::TurnFinished)
            && event
                .payload_json
                .get("reason")
                .and_then(|value| value.as_str())
                == Some("start_not_acknowledged")
    }));
    assert!(events.iter().any(|event| {
        matches!(&event.event_type, SessionEventType::TurnFinished)
            && event
                .payload_json
                .get("status")
                .and_then(|value| value.as_str())
                == Some("failed")
            && event.payload_json.get("kind") == Some(&json!("start_not_acknowledged"))
    }));
}
