use super::*;

async fn write_codex_rollout_log(codex_home: &Path, session_ref: &str) {
    let sessions_dir = codex_home
        .join("sessions")
        .join("2026")
        .join("04")
        .join("05");
    tokio::fs::create_dir_all(&sessions_dir)
        .await
        .expect("create sessions dir");
    let path = sessions_dir.join(format!("rollout-2026-04-05T00-00-00-{session_ref}.jsonl"));
    let payload = json!({
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": {
                "model_context_window": 258400,
                "last_token_usage": {
                    "input_tokens": 12,
                    "output_tokens": 3,
                    "reasoning_output_tokens": 2,
                    "total_tokens": 17
                }
            }
        }
    });
    tokio::fs::write(&path, format!("{payload}\n"))
        .await
        .expect("write rollout log");
}

#[tokio::test]
async fn codex_done_metrics_use_runtime_codex_home_instead_of_home_dir_guess() {
    let session_ref = "019d5ac4-e8b0-7c93-9b0f-e4b22203d391";
    let codex_home = tempdir().expect("temp codex home");
    let data_dir = tempdir().expect("temp data dir");

    write_codex_rollout_log(codex_home.path(), session_ref).await;
    let fixture = build_loop_fixture(data_dir.path(), "codex", "gpt-5.4/medium").await;
    let (_fixture, turn) = run_done_event_loop(
        fixture,
        "codex",
        "gpt-5.4/medium",
        session_ref,
        Some(codex_home.path()),
    )
    .await;
    let metrics = turn.metrics_json.expect("expected codex rollout metrics");

    assert_eq!(turn.status, SessionTurnStatus::Completed);
    assert_eq!(
        metrics
            .get("context_window_tokens")
            .and_then(serde_json::Value::as_u64),
        Some(258400)
    );
    assert_eq!(
        metrics
            .get("context_tokens_estimate")
            .and_then(serde_json::Value::as_u64),
        Some(17)
    );
    assert_eq!(
        metrics
            .get("remaining_tokens_estimate")
            .and_then(serde_json::Value::as_u64),
        Some(258383)
    );
    assert_eq!(
        metrics
            .get("total_input_tokens")
            .and_then(serde_json::Value::as_u64),
        Some(12)
    );
    assert_eq!(
        metrics
            .get("total_output_tokens")
            .and_then(serde_json::Value::as_u64),
        Some(5)
    );
}

#[tokio::test]
async fn done_events_update_active_task_summary_activity_to_match_head() {
    let data_dir = tempdir().expect("temp data dir");
    let fixture = build_loop_fixture(data_dir.path(), "fake", "fake-model").await;
    let (fixture, turn) =
        run_done_event_loop(fixture, "fake", "fake-model", "session-ref", None).await;

    assert_eq!(turn.status, SessionTurnStatus::Completed);

    let active_task = fixture
        .state
        .workspaces
        .workspace_active_snapshot
        .active_task_summary(fixture.workspace_id, fixture.task_id)
        .await
        .expect("active task summary");
    assert_eq!(
        active_task.primary_session.activity.last_turn_status,
        Some(SessionTurnStatus::Completed)
    );
    assert!(!active_task.primary_session.activity.is_working);

    let active_heads = fixture
        .state
        .workspaces
        .workspace_active_snapshot
        .active_heads(fixture.workspace_id)
        .await;
    let head = active_heads
        .heads
        .into_iter()
        .find(|head| head.session.id == fixture.session_id)
        .expect("active head");
    assert_eq!(head.activity, active_task.primary_session.activity);
}
