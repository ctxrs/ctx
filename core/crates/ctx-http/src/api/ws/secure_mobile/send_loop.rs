use std::time::Instant;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use ctx_core::ids::WorkspaceId;
use ctx_transport_runtime::mobile_e2ee;

use super::super::common::{send_secure_ws, WorkspaceStreamSendRuntime, WorkspaceStreamSequencer};
use super::super::queue::{HeadBatchLane, NextWorkspaceStreamItem};
use super::super::workspace_stream;

#[path = "send_loop/telemetry.rs"]
mod telemetry;

pub(super) fn spawn_mobile_secure_send_loop(
    sender: futures::stream::SplitSink<WebSocket, WsMessage>,
    workspace_id: WorkspaceId,
    device_id: String,
    key: mobile_e2ee::E2eeKey,
    runtime: &workspace_stream::WorkspaceStreamRuntime,
) -> tokio::task::JoinHandle<()> {
    let runtime = WorkspaceStreamSendRuntime::new(runtime);

    tokio::spawn(async move {
        let mut sender = sender;
        let mut envelope_seq: i64 = 0;
        let mut sequencer = WorkspaceStreamSequencer::default();
        let mut tick = WorkspaceStreamSendRuntime::flush_tick();
        loop {
            if let Some(next) = runtime.take_next().await {
                match next {
                    NextWorkspaceStreamItem::Control(entry) => {
                        let (enqueued_at, message) = entry.into_parts();
                        let queued_ms = enqueued_at.elapsed().as_millis();
                        let sequenced = sequencer.sequence_control_message(&runtime, message);
                        envelope_seq += 1;
                        let message = sequenced.message;
                        let send_start = Instant::now();
                        if send_secure_ws(&mut sender, &key, &device_id, envelope_seq, &message)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        if sequenced.is_snapshot {
                            telemetry::log_secure_snapshot_sent(
                                workspace_id,
                                queued_ms,
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
                        envelope_seq += 1;
                        let delta_count = deltas.len();
                        let message = sequencer.sequence_heads_batch(
                            &runtime,
                            snapshot_rev,
                            deltas,
                            stream_source,
                        );
                        let payload_bytes = serde_json::to_vec(&message)
                            .map(|data| data.len())
                            .unwrap_or(0);
                        let send_start = Instant::now();
                        if send_secure_ws(&mut sender, &key, &device_id, envelope_seq, &message)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        telemetry::log_secure_heads_batch_sent(
                            workspace_id,
                            lane.as_str(),
                            delta_count,
                            payload_bytes,
                            oldest_queued_ms,
                            send_start.elapsed().as_millis(),
                        );
                        if lane == HeadBatchLane::Background {
                            runtime.wait_after_background_batch().await;
                        }
                    }
                    NextWorkspaceStreamItem::SummaryBatch { events } => {
                        let mut send_failed = false;
                        for queued in events {
                            envelope_seq += 1;
                            let message = sequencer.sequence_summary_event(
                                &runtime,
                                queued.event,
                                queued.stream_source,
                            );
                            if send_secure_ws(&mut sender, &key, &device_id, envelope_seq, &message)
                                .await
                                .is_err()
                            {
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
