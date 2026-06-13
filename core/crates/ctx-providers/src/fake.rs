use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, Duration};
use uuid::Uuid;

use ctx_core::models::SessionEventType;

use crate::adapters::{ProviderAdapter, ProviderStatus, ProviderTurnOutcome, RunHandle, TurnInput};
use crate::events::NormalizedEvent;

#[derive(Debug, Clone)]
struct FixtureToolCall {
    kind: String,
    title: Option<String>,
    input: Option<Value>,
    output_text: Option<String>,
}

const LIVE_CONTEXT_WINDOW_MARKER: &str = "emit-live-context-window";
const STREAM_ASSISTANT_PARTIALS_MARKER: &str = "stream-assistant-partials";

fn live_context_window_metrics(content: &str) -> Option<Value> {
    if !content.contains(LIVE_CONTEXT_WINDOW_MARKER) {
        return None;
    }
    Some(json!({
        "context_tokens_estimate": 25,
        "context_window_tokens": 100,
        "remaining_tokens_estimate": 75,
        "remaining_fraction": 0.75,
    }))
}

fn split_assistant_fragments(content: &str, chunk_count: usize) -> Vec<String> {
    if content.is_empty() {
        return vec![String::new()];
    }
    let chars: Vec<char> = content.chars().collect();
    let total = chars.len();
    let target_chunks = chunk_count.max(1).min(total);
    let mut fragments = Vec::with_capacity(target_chunks);
    let mut start = 0usize;
    for index in 0..target_chunks {
        let remaining = total.saturating_sub(start);
        let remaining_chunks = target_chunks - index;
        let take = remaining.div_ceil(remaining_chunks);
        let end = (start + take).min(total);
        fragments.push(chars[start..end].iter().collect());
        start = end;
    }
    fragments
}

fn parse_fixture_tools(content: &str) -> Option<Vec<FixtureToolCall>> {
    let start = content.find("[[tool_calls]]")?;
    let end = content.find("[[/tool_calls]]")?;
    if end <= start {
        return None;
    }
    let raw = &content[start + "[[tool_calls]]".len()..end];
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let tools_value = if parsed.is_array() {
        parsed
    } else {
        parsed.get("tool_calls")?.clone()
    };
    let list = tools_value.as_array()?.to_vec();
    let mut out = Vec::new();
    for tool in list {
        let kind = tool
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("execute");
        let title = tool
            .get("title")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string());
        let input = tool.get("input").cloned();
        let output_text = tool
            .get("output_text")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string());
        out.push(FixtureToolCall {
            kind: kind.to_string(),
            title,
            input,
            output_text,
        });
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[derive(Default)]
pub struct FakeProviderAdapter;

impl FakeProviderAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ProviderAdapter for FakeProviderAdapter {
    async fn inspect(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "fake".into(),
            installed: true,
            detected_path: None,
            version: Some("0.1.0".into()),
            capabilities: Some(crate::adapters::ProviderCapabilities {
                stream_events: true,
                stream_format: "jsonl".into(),
                has_turn_boundaries: true,
                has_tool_call_ids: true,
                has_file_change_events: false,
                has_command_events: false,
                supports_resume: false,
                supports_stable_session_id: false,
                supports_fork_or_rewind: false,
                supports_headless: true,
                supports_server_mode: false,
                supports_interactive_tui: false,
                supports_private_state_dir: false,
                supports_sandbox_flags: false,
                supports_approval_flags: false,
                notes: vec!["Deterministic fake provider for CI".into()],
            }),
            health: crate::adapters::ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: crate::adapters::ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        event_sink: mpsc::Sender<NormalizedEvent>,
        _hooks: crate::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let (outcome_tx, outcome_rx) = oneshot::channel::<ProviderTurnOutcome>();
        let join = tokio::spawn(async move {
            let sink = event_sink;
            let fixture_tools = parse_fixture_tools(&input.content);

            let send = |event_type, payload| async {
                let _ = sink
                    .send(NormalizedEvent {
                        event_type,
                        payload_json: payload,
                    })
                    .await;
            };

            let slow = input.content.contains("slow-diff-test");
            let stream_assistant_partials =
                input.content.contains(STREAM_ASSISTANT_PARTIALS_MARKER);
            let live_context_window = live_context_window_metrics(&input.content);
            let delay = if slow {
                Duration::from_millis(1200)
            } else {
                Duration::from_millis(10)
            };
            let assistant_message_id = Uuid::new_v4().to_string();
            let assistant_output = format!("done: {}", input.content);
            let assistant_fragments = if stream_assistant_partials {
                split_assistant_fragments(&assistant_output, 3)
            } else {
                vec![assistant_output.clone()]
            };
            let assistant_order_seq = 2_i64;
            let omit_terminal_event = input.content.contains("omit-terminal-event");

            let done_payload = if let Some(context_window) = live_context_window.clone() {
                json!({"context_window": context_window})
            } else {
                json!({})
            };

            let outcome = tokio::select! {
                outcome = async {
                    for fragment in assistant_fragments {
                        send(
                            SessionEventType::AssistantChunk,
                            json!({
                                "content": fragment,
                                "content_fragment": fragment,
                                "message_id": assistant_message_id.clone(),
                                "order_seq": assistant_order_seq,
                            }),
                        )
                        .await;
                        sleep(delay).await;
                    }
                    if let Some(context_window) = live_context_window.clone() {
                        send(
                            SessionEventType::ContextWindowUpdate,
                            json!({"context_window": context_window}),
                        )
                        .await;
                        sleep(delay).await;
                    }
                    if input.content.contains("emit-thought") {
                        // Exercise thought streaming paths. This is intentionally stream-only
                        // and should be safe for tests that opt in via the marker.
                        send(SessionEventType::ThoughtChunk, json!({"content_fragment": "thinking...", "is_final": false})).await;
                        sleep(delay).await;
                        send(SessionEventType::ThoughtChunk, json!({"content_fragment": "done thinking", "is_final": true})).await;
                        sleep(delay).await;
                    }
                    if let Some(tools) = fixture_tools {
                        for tool in tools {
                            let tool_call_id = Uuid::new_v4().to_string();
                            send(SessionEventType::ToolCall, json!({
                                "tool_call_id": tool_call_id,
                                "kind": tool.kind,
                                "title": tool.title,
                                "rawInput": tool.input,
                            })).await;
                            sleep(delay).await;
                            send(SessionEventType::ToolResult, json!({
                                "tool_call_id": tool_call_id,
                                "kind": tool.kind,
                                "title": tool.title,
                                "outputText": tool.output_text.unwrap_or_else(|| "ok".to_string()),
                            })).await;
                            sleep(delay).await;
                        }
                    } else {
                        let tool_call_id = Uuid::new_v4().to_string();
                        send(SessionEventType::ToolCall, json!({"tool_call_id": tool_call_id, "name": "fake_tool", "args": {}})).await;
                        sleep(delay).await;
                        send(SessionEventType::ToolResult, json!({"tool_call_id": tool_call_id, "result": "ok"})).await;
                        sleep(delay).await;
                    }
                    send(
                        SessionEventType::AssistantComplete,
                        json!({
                            "content": assistant_output,
                            "full_content": assistant_output,
                            "message_id": assistant_message_id.clone(),
                            "order_seq": assistant_order_seq,
                        }),
                    )
                    .await;
                    sleep(delay).await;
                    if omit_terminal_event {
                        ProviderTurnOutcome::protocol_violation(
                            "provider_protocol_violation_no_terminal_outcome",
                            "fake provider ended without emitting a terminal event",
                        )
                    } else {
                        send(SessionEventType::Done, done_payload.clone()).await;
                        ProviderTurnOutcome::completed()
                    }
                } => outcome,
                _ = &mut cancel_rx => {
                    if input.content.contains("complete-on-cancel") {
                        send(SessionEventType::Done, done_payload).await;
                        ProviderTurnOutcome::completed()
                    } else {
                        send(
                            SessionEventType::TurnInterrupted,
                            json!({
                                "reason": "cancelled",
                                "provider_cancelled": true,
                                "status": "interrupted",
                            }),
                        )
                        .await;
                        ProviderTurnOutcome::interrupted("cancelled", true)
                    }
                }
            };
            let _ = outcome_tx.send(outcome);
        });
        let abort = join.abort_handle();
        drop(tokio::spawn(async move {
            let _ = join.await;
            let _ = done_tx.send(());
        }));

        Ok(RunHandle {
            done: done_rx,
            outcome: outcome_rx,
            cancel: Some(cancel_tx),
            abort: Some(abort),
        })
    }

    async fn cancel(&self, handle: &mut RunHandle) -> Result<()> {
        if let Some(cancel) = handle.cancel.take() {
            let _ = cancel.send(());
        }
        let done = tokio::time::timeout(std::time::Duration::from_secs(2), &mut handle.done).await;
        if done.is_err() {
            if let Some(abort) = handle.abort.take() {
                abort.abort();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_provider_reports_protocol_violation_when_terminal_event_is_missing() {
        let adapter = FakeProviderAdapter::new();
        let (event_tx, _event_rx) = mpsc::channel(8);
        let handle = adapter
            .run(
                TurnInput {
                    content: "omit-terminal-event".to_string(),
                    attachments: Vec::new(),
                    context_blocks: Vec::new(),
                    model_id: None,
                },
                PathBuf::from("."),
                HashMap::new(),
                event_tx,
                crate::adapters::ProviderRunHooks::default(),
            )
            .await
            .expect("run handle");

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), handle.outcome)
            .await
            .expect("outcome timeout")
            .expect("outcome");
        assert_eq!(outcome.status, crate::adapters::ProviderTurnStatus::Failed);
        assert_eq!(
            outcome.reason.as_deref(),
            Some("provider_protocol_violation_no_terminal_outcome")
        );
        assert!(!outcome.terminal_event_emitted);
    }

    #[tokio::test]
    async fn fake_provider_cancel_reports_interrupted_outcome() {
        let adapter = FakeProviderAdapter::new();
        let (event_tx, _event_rx) = mpsc::channel(8);
        let mut handle = adapter
            .run(
                TurnInput {
                    content: "slow-diff-test".to_string(),
                    attachments: Vec::new(),
                    context_blocks: Vec::new(),
                    model_id: None,
                },
                PathBuf::from("."),
                HashMap::new(),
                event_tx,
                crate::adapters::ProviderRunHooks::default(),
            )
            .await
            .expect("run handle");

        handle
            .cancel
            .take()
            .expect("cancel sender")
            .send(())
            .expect("cancel turn");

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), handle.outcome)
            .await
            .expect("outcome timeout")
            .expect("outcome");
        assert_eq!(
            outcome.status,
            crate::adapters::ProviderTurnStatus::Interrupted
        );
        assert_eq!(outcome.reason.as_deref(), Some("cancelled"));
        assert_eq!(outcome.provider_cancelled, Some(true));
    }

    #[tokio::test]
    async fn fake_provider_can_complete_after_cancel_when_marker_is_set() {
        let adapter = FakeProviderAdapter::new();
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let mut handle = adapter
            .run(
                TurnInput {
                    content: "slow-diff-test complete-on-cancel".to_string(),
                    attachments: Vec::new(),
                    context_blocks: Vec::new(),
                    model_id: None,
                },
                PathBuf::from("."),
                HashMap::new(),
                event_tx,
                crate::adapters::ProviderRunHooks::default(),
            )
            .await
            .expect("run handle");

        handle
            .cancel
            .take()
            .expect("cancel sender")
            .send(())
            .expect("cancel turn");

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), handle.outcome)
            .await
            .expect("outcome timeout")
            .expect("outcome");
        assert_eq!(
            outcome.status,
            crate::adapters::ProviderTurnStatus::Completed
        );

        let done = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while let Some(event) = event_rx.recv().await {
                if matches!(event.event_type, SessionEventType::Done) {
                    return true;
                }
            }
            false
        })
        .await
        .expect("event timeout");
        assert!(done);
    }
}
