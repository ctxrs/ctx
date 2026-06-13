use std::time::Instant;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::SinkExt;

use super::super::common::{WorkspaceStreamSendRuntime, WorkspaceStreamSequencer};
use super::super::queue::{HeadBatchLane, NextWorkspaceStreamItem};
use super::super::workspace_stream;
use ctx_core::ids::WorkspaceId;

#[path = "send_loop/telemetry.rs"]
mod telemetry;

pub(super) fn spawn_workspace_active_send_loop(
    sender: futures::stream::SplitSink<WebSocket, WsMessage>,
    workspace_id: WorkspaceId,
    runtime: &workspace_stream::WorkspaceStreamRuntime,
) -> tokio::task::JoinHandle<()> {
    let runtime = WorkspaceStreamSendRuntime::new(runtime);

    tokio::spawn(async move {
        let mut sender = sender;
        let mut sequencer = WorkspaceStreamSequencer::default();
        let mut tick = WorkspaceStreamSendRuntime::flush_tick();
        loop {
            if let Some(next) = runtime.take_next().await {
                match next {
                    NextWorkspaceStreamItem::Control(entry) => {
                        let (enqueued_at, message) = entry.into_parts();
                        let queued_ms = enqueued_at.elapsed().as_millis();
                        let sequenced = sequencer.sequence_control_message(&runtime, message);
                        let message = sequenced.message;
                        let serialize_start = Instant::now();
                        let Ok(text) = serde_json::to_string(&message) else {
                            break;
                        };
                        let encode_ms = serialize_start.elapsed().as_millis();
                        let payload_bytes = text.len();
                        let send_start = Instant::now();
                        if sender.send(WsMessage::Text(text)).await.is_err() {
                            break;
                        }
                        if sequenced.is_snapshot {
                            telemetry::log_workspace_snapshot_sent(
                                workspace_id,
                                payload_bytes,
                                queued_ms,
                                encode_ms,
                                send_start.elapsed().as_millis(),
                                &message,
                            );
                            runtime.clear_hydrating();
                        }
                    }
                    NextWorkspaceStreamItem::HeadsBatch {
                        lane,
                        snapshot_rev,
                        deltas,
                        oldest_queued_ms,
                        stream_source,
                    } => {
                        let delta_count = deltas.len();
                        let message = sequencer.sequence_heads_batch(
                            &runtime,
                            snapshot_rev,
                            deltas,
                            stream_source,
                        );
                        let serialize_start = Instant::now();
                        let Ok(text) = serde_json::to_string(&message) else {
                            break;
                        };
                        let encode_ms = serialize_start.elapsed().as_millis();
                        let payload_bytes = text.len();
                        let send_start = Instant::now();
                        if sender.send(WsMessage::Text(text)).await.is_err() {
                            break;
                        }
                        telemetry::log_workspace_heads_batch_sent(
                            workspace_id,
                            lane.as_str(),
                            delta_count,
                            payload_bytes,
                            oldest_queued_ms,
                            encode_ms,
                            send_start.elapsed().as_millis(),
                        );
                        if lane == HeadBatchLane::Background {
                            runtime.wait_after_background_batch().await;
                        }
                    }
                    NextWorkspaceStreamItem::SummaryBatch { events } => {
                        let mut send_failed = false;
                        for queued in events {
                            let message = sequencer.sequence_summary_event(
                                &runtime,
                                queued.event,
                                queued.stream_source,
                            );
                            let Ok(text) = serde_json::to_string(&message) else {
                                send_failed = true;
                                break;
                            };
                            if sender.send(WsMessage::Text(text)).await.is_err() {
                                send_failed = true;
                                break;
                            }
                        }
                        if send_failed {
                            break;
                        }
                    }
                }
                if runtime.should_disconnect_after_flush().await {
                    break;
                }
                continue;
            }
            if runtime.disconnect_requested() {
                break;
            }

            runtime.wait_for_next_signal(&mut tick).await;
        }
    })
}
