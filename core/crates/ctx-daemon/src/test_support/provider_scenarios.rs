use std::time::Duration;

use ctx_core::ids::SessionId;
use ctx_core::models::{MessageRole, SessionEvent, SessionEventType, SessionTurnStatus};
use serde_json::Value;

use super::TestDaemon;

#[derive(Debug)]
pub struct ProviderScenarioAssistantMessageSnapshot {
    pub content: String,
    pub order_seq: Option<i64>,
}

#[derive(Debug)]
pub struct ProviderScenarioTurnSnapshot {
    pub thought_partial: Option<String>,
    pub metrics_json: Option<Value>,
}

#[derive(Debug)]
pub struct ProviderScenarioSessionSnapshot {
    pub events: Vec<SessionEvent>,
    pub turns: Vec<ProviderScenarioTurnSnapshot>,
    pub assistant_messages: Vec<ProviderScenarioAssistantMessageSnapshot>,
}

impl TestDaemon {
    pub async fn wait_for_provider_scenario_done_for_test(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<ProviderScenarioSessionSnapshot> {
        self.wait_for_provider_scenario_done_inner_for_test(session_id, false)
            .await
    }

    pub async fn wait_for_provider_scenario_completed_turn_for_test(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<ProviderScenarioSessionSnapshot> {
        self.wait_for_provider_scenario_done_inner_for_test(session_id, true)
            .await
    }

    async fn wait_for_provider_scenario_done_inner_for_test(
        &self,
        session_id: SessionId,
        require_completed_turn: bool,
    ) -> anyhow::Result<ProviderScenarioSessionSnapshot> {
        let store = self.state.store_for_session(session_id).await?;
        let timeout_secs = std::env::var("CTX_TEST_WAIT_FOR_DONE_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(120);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            let events = store.list_session_events(session_id).await?;
            if events
                .iter()
                .any(|event| matches!(event.event_type, SessionEventType::Error))
            {
                let provider_logs = provider_log_snapshot_for_test(self.data_root());
                anyhow::bail!("saw Error event(s): {events:#?}\nprovider logs:\n{provider_logs}");
            }

            let saw_done = events
                .iter()
                .any(|event| matches!(event.event_type, SessionEventType::Done));
            let turns =
                if saw_done || require_completed_turn || tokio::time::Instant::now() >= deadline {
                    store
                        .list_session_turns_page_by_seq(session_id, None, Some(10))
                        .await?
                } else {
                    Vec::new()
                };

            if saw_done {
                if !require_completed_turn {
                    return self
                        .provider_scenario_session_snapshot_for_test(session_id, events, turns)
                        .await;
                }
                if let Some(turn) = turns.last() {
                    match turn.status {
                        SessionTurnStatus::Completed => {
                            return self
                                .provider_scenario_session_snapshot_for_test(
                                    session_id, events, turns,
                                )
                                .await;
                        }
                        SessionTurnStatus::Failed | SessionTurnStatus::Interrupted => {
                            anyhow::bail!("saw terminal non-completed turn after Done: {turn:#?}");
                        }
                        SessionTurnStatus::Queued
                        | SessionTurnStatus::Starting
                        | SessionTurnStatus::Running => {}
                    }
                }
            }

            if tokio::time::Instant::now() >= deadline {
                let provider_logs = provider_log_snapshot_for_test(self.data_root());
                anyhow::bail!(
                    "timed out waiting for Done event after {timeout_secs}s: {events:#?}\nturns:\n{turns:#?}\nprovider logs:\n{provider_logs}"
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn provider_scenario_session_snapshot_for_test(
        &self,
        session_id: SessionId,
        events: Vec<SessionEvent>,
        turns: Vec<ctx_core::models::SessionTurn>,
    ) -> anyhow::Result<ProviderScenarioSessionSnapshot> {
        let store = self.state.store_for_session(session_id).await?;
        let assistant_messages = store
            .list_messages_for_session(session_id)
            .await?
            .into_iter()
            .filter(|message| matches!(message.role, MessageRole::Assistant))
            .map(|message| ProviderScenarioAssistantMessageSnapshot {
                content: message.content,
                order_seq: message.order_seq,
            })
            .collect();
        let turns = turns
            .into_iter()
            .map(|turn| ProviderScenarioTurnSnapshot {
                thought_partial: turn.thought_partial,
                metrics_json: turn.metrics_json,
            })
            .collect();
        Ok(ProviderScenarioSessionSnapshot {
            events,
            turns,
            assistant_messages,
        })
    }
}

fn provider_log_snapshot_for_test(data_root: &std::path::Path) -> String {
    let dir = data_root.join("logs").join("providers");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return format!("missing provider log dir: {}", dir.display());
    };
    let mut files = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    files.sort();

    if files.is_empty() {
        return format!("empty provider log dir: {}", dir.display());
    }

    files
        .into_iter()
        .map(|path| {
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| format!("<failed to read: {err}>"));
            format!("== {} ==\n{}", path.display(), contents)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
