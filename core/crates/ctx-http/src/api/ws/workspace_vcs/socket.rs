use super::*;

use super::buffer::VcsPendingBuffer;
use super::metrics::{record_workspace_vcs_stream_metrics, VcsStreamMetrics};
use super::send_loop::spawn_workspace_vcs_send_loop;
use super::subscription::{
    handle_workspace_vcs_client_message, release_workspace_vcs_demand, WorkspaceVcsRuntime,
};
use ctx_workspace_stream_service::vcs::WorkspaceVcsSnapshotRoute;

fn spawn_workspace_vcs_metrics_loop(
    state: WorkspaceVcsStreamHandle,
    metrics: Arc<VcsStreamMetrics>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            record_workspace_vcs_stream_metrics(&state, &metrics).await;
        }
    })
}

pub(super) async fn handle_workspace_vcs_ws(
    socket: WebSocket,
    state: WorkspaceVcsStreamHandle,
    workspace_id: WorkspaceId,
) {
    let (sender, mut receiver) = socket.split();
    let pending = Arc::new(VcsPendingBuffer::new());
    let metrics = Arc::new(VcsStreamMetrics::default());
    pending
        .push_control(WorktreeVcsStreamMessage::Ready {
            workspace_id,
            vcs_generation: 0,
        })
        .await;

    let send_task =
        spawn_workspace_vcs_send_loop(sender, Arc::clone(&pending), Arc::clone(&metrics));
    let metrics_task = spawn_workspace_vcs_metrics_loop(state.clone(), Arc::clone(&metrics));

    let mut runtime = WorkspaceVcsRuntime::default();
    let mut rx = state.subscribe_worktree_vcs_events();
    let recv_loop = async {
        loop {
            tokio::select! {
                msg = receiver.next() => {
                    match msg {
                        Some(Ok(WsMessage::Text(text))) => {
                            let Ok(message) = serde_json::from_str::<WorktreeVcsStreamClientMessage>(&text) else {
                                continue;
                            };
                            handle_workspace_vcs_client_message(
                                &state,
                                workspace_id,
                                &pending,
                                &metrics,
                                &mut runtime,
                                message,
                            )
                            .await;
                        }
                        Some(Ok(WsMessage::Binary(bytes))) => {
                            let Ok(text) = String::from_utf8(bytes.to_vec()) else {
                                continue;
                            };
                            let Ok(message) = serde_json::from_str::<WorktreeVcsStreamClientMessage>(&text) else {
                                continue;
                            };
                            handle_workspace_vcs_client_message(
                                &state,
                                workspace_id,
                                &pending,
                                &metrics,
                                &mut runtime,
                                message,
                            )
                            .await;
                        }
                        Some(Ok(WsMessage::Close(_))) => break,
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => break,
                    }
                }
                event = rx.recv() => {
                    match event {
                        Ok(snapshot) => match state.route_workspace_vcs_snapshot(&runtime, snapshot.worktree_id) {
                            WorkspaceVcsSnapshotRoute::Details => {
                                super::subscription::queue_vcs_snapshot(
                                    &pending,
                                    &metrics,
                                    workspace_id,
                                    runtime.demand_generation,
                                    WorktreeVcsStreamTier::Details,
                                    snapshot,
                                )
                                .await;
                            }
                            WorkspaceVcsSnapshotRoute::Summary => {
                                super::subscription::queue_vcs_snapshot(
                                    &pending,
                                    &metrics,
                                    workspace_id,
                                    runtime.demand_generation,
                                    WorktreeVcsStreamTier::Summary,
                                    snapshot,
                                )
                                .await;
                            }
                            WorkspaceVcsSnapshotRoute::Drop => {}
                        },
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                target: "ctx_http.ws_vcs",
                                workspace_id = %workspace_id.0,
                                skipped,
                                "workspace vcs stream lagged; latest subscribed snapshots will be reseeded",
                            );
                            let plan = state.plan_workspace_vcs_lag_reseed(&runtime);
                            super::subscription::seed_current_vcs_snapshots(
                                &state,
                                workspace_id,
                                &pending,
                                &metrics,
                                runtime.demand_generation,
                                &plan,
                            )
                            .await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    recv_loop.await;
    send_task.abort();
    metrics_task.abort();
    record_workspace_vcs_stream_metrics(&state, &metrics).await;
    release_workspace_vcs_demand(&state, &runtime).await;
}
