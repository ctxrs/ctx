use std::time::Duration;

use ctx_core::ids::{MessageId, RunId, SessionId, TurnId};
use ctx_core::models::{
    MessageRole, SessionEvent, SessionEventType, SessionTurn, SessionTurnStatus,
};
use ctx_core::session_projection::terminal_status_from_finished_payload;

use super::{
    AssistantChunkStreamSnapshot, NoisyOutputPersistenceSnapshot, TerminalTurnPersistenceSnapshot,
    TestDaemon, TurnReconciliationSnapshot,
};

impl TestDaemon {
    pub async fn wait_for_assistant_message_for_test(
        &self,
        session_id: SessionId,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let store = self.state.store_for_session(session_id).await?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let messages = store.list_messages_for_session(session_id).await?;
            if messages
                .iter()
                .any(|message| matches!(message.role, MessageRole::Assistant))
            {
                return Ok(());
            }
            let turns = store
                .list_session_turns_page_by_seq(session_id, None, Some(10))
                .await?;
            if turns.iter().any(|turn| {
                matches!(
                    turn.status,
                    SessionTurnStatus::Failed | SessionTurnStatus::Interrupted
                )
            }) {
                anyhow::bail!("turn failed before assistant message was produced: {turns:#?}");
            }
            if tokio::time::Instant::now() >= deadline {
                let events = store.list_session_events(session_id).await?;
                anyhow::bail!(
                    "assistant message not produced; messages={messages:#?}; events={events:#?}; turns={turns:#?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn wait_for_session_turn_failed_for_test(
        &self,
        session_id: SessionId,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let store = self.state.store_for_session(session_id).await?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let turns = store
                .list_session_turns_page_by_seq(session_id, None, Some(10))
                .await?;
            if turns
                .last()
                .is_some_and(|turn| turn.status == SessionTurnStatus::Failed)
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("turn did not fail before timeout; turns={turns:#?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn wait_for_scheduler_runtime_events_for_test<F>(
        &self,
        session_id: SessionId,
        timeout: Duration,
        label: &str,
        mut predicate: F,
    ) -> anyhow::Result<Vec<SessionEvent>>
    where
        F: FnMut(&[SessionEvent]) -> anyhow::Result<bool>,
    {
        let store = self.state.store_for_session(session_id).await?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let events = store.list_session_events(session_id).await?;
            if predicate(&events)? {
                return Ok(events);
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for {label}: {events:#?}");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub async fn wait_for_session_done_event_count_for_test(
        &self,
        session_id: SessionId,
        expected_done_events: usize,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        self.wait_for_scheduler_runtime_events_for_test(
            session_id,
            timeout,
            &format!("{expected_done_events} done events"),
            |events| {
                if events
                    .iter()
                    .any(|event| matches!(event.event_type, SessionEventType::Error))
                {
                    anyhow::bail!(
                        "unexpected session error while waiting for done events: {events:#?}"
                    );
                }
                let done_count = events
                    .iter()
                    .filter(|event| matches!(event.event_type, SessionEventType::Done))
                    .count();
                Ok(done_count >= expected_done_events)
            },
        )
        .await
        .map(|_| ())
    }

    pub async fn provider_target_session_events_after_done_for_test(
        &self,
        session_id: SessionId,
        expected_assistant_message: &str,
        timeout: Duration,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        self.wait_for_scheduler_runtime_events_for_test(
            session_id,
            timeout,
            "provider target scoped install session done event",
            |events| {
                if events
                    .iter()
                    .any(|event| matches!(event.event_type, SessionEventType::Error))
                {
                    anyhow::bail!("unexpected session error while waiting for done: {events:#?}");
                }
                let has_done = events
                    .iter()
                    .any(|event| matches!(event.event_type, SessionEventType::Done));
                let has_expected_message = events.iter().any(|event| {
                    matches!(event.event_type, SessionEventType::AssistantMessageInserted)
                        && event
                            .payload_json
                            .get("content")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|content| content.contains(expected_assistant_message))
                });
                Ok(has_done && has_expected_message)
            },
        )
        .await
    }

    pub async fn wait_for_session_completed_turn_count_for_test(
        &self,
        session_id: SessionId,
        expected_completed_turns: usize,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        self.wait_for_scheduler_runtime_events_for_test(
            session_id,
            timeout,
            &format!("{expected_completed_turns} completed turns"),
            |events| {
                let terminal_failure = events.iter().find(|event| {
                    matches!(
                        event.event_type,
                        SessionEventType::Error | SessionEventType::TurnInterrupted
                    ) || (matches!(event.event_type, SessionEventType::TurnFinished)
                        && matches!(
                            terminal_status_from_finished_payload(&event.payload_json),
                            Some(SessionTurnStatus::Failed | SessionTurnStatus::Interrupted)
                        ))
                });
                if let Some(event) = terminal_failure {
                    anyhow::bail!(
                        "unexpected terminal session failure while waiting for completed turns: {event:#?}"
                    );
                }

                let mut completed_turns = std::collections::HashSet::new();
                for event in events {
                    let completed = matches!(event.event_type, SessionEventType::Done)
                        || (matches!(event.event_type, SessionEventType::TurnFinished)
                            && terminal_status_from_finished_payload(&event.payload_json)
                                == Some(SessionTurnStatus::Completed));
                    if completed {
                        if let Some(turn_id) = event.turn_id {
                            completed_turns.insert(turn_id);
                        }
                    }
                }

                Ok(completed_turns.len() >= expected_completed_turns)
            },
        )
        .await
        .map(|_| ())
    }

    pub async fn wait_for_provider_session_ref_for_test(
        &self,
        session_id: SessionId,
        timeout: Duration,
    ) -> anyhow::Result<String> {
        let store = self.state.store_for_session(session_id).await?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let session = store
                .get_session(session_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("session {session_id:?} not found"))?;
            if let Some(provider_session_ref) = session.provider_session_ref {
                return Ok(provider_session_ref);
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for provider_session_ref on {session_id:?}");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub async fn wait_for_noisy_output_persistence_snapshot_for_test(
        &self,
        session_id: SessionId,
        timeout: Duration,
    ) -> anyhow::Result<NoisyOutputPersistenceSnapshot> {
        let events = self
            .wait_for_scheduler_runtime_events_for_test(
                session_id,
                timeout,
                "noisy output Done event",
                |events| {
                    if events
                        .iter()
                        .any(|event| matches!(event.event_type, SessionEventType::Error))
                    {
                        anyhow::bail!(
                            "unexpected session error while waiting for noisy output persistence: {events:#?}"
                        );
                    }
                    Ok(events
                        .iter()
                        .any(|event| matches!(event.event_type, SessionEventType::Done)))
                },
            )
            .await?;
        let messages = self
            .state
            .store_for_session(session_id)
            .await?
            .list_messages_for_session(session_id)
            .await?;
        Ok(NoisyOutputPersistenceSnapshot { events, messages })
    }

    pub async fn assistant_chunk_stream_snapshot_for_test(
        &self,
        session_id: SessionId,
        timeout: Duration,
    ) -> anyhow::Result<AssistantChunkStreamSnapshot> {
        let events = self
            .wait_for_scheduler_runtime_events_for_test(
                session_id,
                timeout,
                "Done event",
                |events| {
                    Ok(events
                        .iter()
                        .any(|event| matches!(event.event_type, SessionEventType::Done)))
                },
            )
            .await?;
        let turns = self
            .state
            .store_for_session(session_id)
            .await?
            .list_session_turns_page_by_seq(session_id, None, Some(1))
            .await?;
        Ok(AssistantChunkStreamSnapshot { events, turns })
    }

    pub async fn wait_for_terminal_turn_persistence_snapshot_for_test(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        timeout: Duration,
    ) -> anyhow::Result<TerminalTurnPersistenceSnapshot> {
        let store = self.state.store_for_session(session_id).await?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let turn = store
                .get_session_turn(session_id, turn_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("turn {turn_id:?} not found"))?;
            let events = store
                .list_session_events_for_turn(session_id, turn_id, false)
                .await?;
            let terminal = matches!(
                turn.status,
                SessionTurnStatus::Completed
                    | SessionTurnStatus::Failed
                    | SessionTurnStatus::Interrupted
            );
            let finished = events
                .iter()
                .any(|event| matches!(event.event_type, SessionEventType::TurnFinished));
            if terminal && finished {
                let assistant_messages = store
                    .list_messages_for_session(session_id)
                    .await?
                    .into_iter()
                    .filter(|message| {
                        message.turn_id == Some(turn_id)
                            && matches!(message.role, MessageRole::Assistant)
                    })
                    .collect();
                return Ok(TerminalTurnPersistenceSnapshot {
                    turn,
                    events,
                    assistant_messages,
                });
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for terminal turn: {events:#?}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn seed_running_turn_for_reconciliation_test(
        &self,
        session_id: SessionId,
        run_id: RunId,
        turn_id: TurnId,
    ) -> anyhow::Result<()> {
        self.state
            .store_for_session(session_id)
            .await?
            .insert_session_turn(SessionTurn {
                turn_id,
                session_id,
                run_id: Some(run_id),
                user_message_id: Some(MessageId::new()),
                status: SessionTurnStatus::Running,
                start_seq: Some(1),
                end_seq: None,
                started_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                assistant_partial: None,
                thought_partial: None,
                metrics_json: None,
                failure: None,
                tool_total: 0,
                tool_pending: 0,
                tool_running: 0,
                tool_completed: 0,
                tool_failed: 0,
            })
            .await?;
        Ok(())
    }

    pub async fn append_turn_finished_event_for_test(
        &self,
        session_id: SessionId,
        run_id: Option<RunId>,
        turn_id: TurnId,
        status: SessionTurnStatus,
    ) -> anyhow::Result<SessionEvent> {
        let status = match status {
            SessionTurnStatus::Completed => "completed",
            SessionTurnStatus::Failed => "failed",
            SessionTurnStatus::Interrupted => "interrupted",
            other => anyhow::bail!("unsupported terminal status for fixture: {other:?}"),
        };
        self.state
            .store_for_session(session_id)
            .await?
            .append_session_event(
                session_id,
                run_id,
                Some(turn_id),
                SessionEventType::TurnFinished,
                serde_json::json!({ "status": status }),
            )
            .await
            .map_err(Into::into)
    }

    pub async fn turn_reconciliation_snapshot_for_test(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> anyhow::Result<TurnReconciliationSnapshot> {
        let store = self.state.store_for_session(session_id).await?;
        let turn = store
            .get_session_turn(session_id, turn_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("turn {turn_id:?} not found"))?;
        let events = store
            .list_session_events_for_turn(session_id, turn_id, false)
            .await?;
        let summary = store
            .get_session_snapshot(session_id, 50, false)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session snapshot {session_id:?} not found"))?
            .summary;
        Ok(TurnReconciliationSnapshot {
            turn,
            events,
            last_turn_status: summary.activity.last_turn_status,
            is_working: summary.activity.is_working,
        })
    }

    pub async fn seed_invalid_workspace_runtime_settings_for_test(
        &self,
        session_id: SessionId,
        contents: &str,
    ) -> anyhow::Result<()> {
        let _ = self
            .state
            .store_for_session(session_id)
            .await?
            .upsert_runtime_settings_document(1, contents)
            .await?;
        Ok(())
    }

    pub async fn session_has_user_message_event_for_test(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<bool> {
        let events = self
            .state
            .store_for_session(session_id)
            .await?
            .list_session_events(session_id)
            .await?;
        Ok(events
            .iter()
            .any(|event| matches!(event.event_type, SessionEventType::UserMessage)))
    }
}
