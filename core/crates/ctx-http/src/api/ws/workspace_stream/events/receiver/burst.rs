use super::metrics::record_workspace_stream_receiver_drain;
use super::*;

const WORKSPACE_STREAM_RECEIVER_DRAIN_LIMIT: usize = 2048;

#[derive(Debug)]
pub(crate) struct WorkspaceStreamReceiverBurst {
    pub(super) events: Vec<WorkspaceActiveSnapshotEvent>,
    pub(super) lagged: Option<u64>,
    pub(super) closed: bool,
    pub(super) hit_limit: bool,
}

pub(crate) fn take_workspace_stream_receiver_burst(
    rx: &mut tokio::sync::broadcast::Receiver<WorkspaceActiveSnapshotEvent>,
    first_event: WorkspaceActiveSnapshotEvent,
) -> WorkspaceStreamReceiverBurst {
    let mut events = vec![first_event];
    let mut hit_limit = false;
    for _ in 1..WORKSPACE_STREAM_RECEIVER_DRAIN_LIMIT {
        match rx.try_recv() {
            Ok(event) => events.push(event),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                return WorkspaceStreamReceiverBurst {
                    events,
                    lagged: None,
                    closed: false,
                    hit_limit,
                };
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(lagged)) => {
                return WorkspaceStreamReceiverBurst {
                    events,
                    lagged: Some(lagged),
                    closed: false,
                    hit_limit,
                };
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                return WorkspaceStreamReceiverBurst {
                    events,
                    lagged: None,
                    closed: true,
                    hit_limit,
                };
            }
        }
    }
    if !events.is_empty() {
        hit_limit = true;
    }
    WorkspaceStreamReceiverBurst {
        events,
        lagged: None,
        closed: false,
        hit_limit,
    }
}

pub(crate) async fn handle_workspace_stream_receiver_burst(
    state: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    burst: WorkspaceStreamReceiverBurst,
    runtime: &mut WorkspaceStreamRuntime,
    labels: &WorkspaceStreamLabels,
) -> Result<(), ()> {
    let event_count = burst.events.len();
    let hit_limit = burst.hit_limit;
    for event in burst.events {
        handle_workspace_stream_event(state, workspace_id, event, runtime, labels).await?;
        if runtime.reset_queued || runtime.send_control.should_disconnect_after_flush() {
            record_workspace_stream_receiver_drain(state, labels, event_count, hit_limit).await;
            return Ok(());
        }
    }
    record_workspace_stream_receiver_drain(state, labels, event_count, hit_limit).await;
    if let Some(lagged) = burst.lagged {
        handle_workspace_stream_lagged(state, workspace_id, lagged, runtime, labels).await?;
    }
    if burst.closed {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_event(workspace_id: WorkspaceId, snapshot_rev: i64) -> WorkspaceActiveSnapshotEvent {
        WorkspaceActiveSnapshotEvent::Ready {
            workspace_id,
            snapshot_rev,
            archived_rev: 0,
        }
    }

    #[test]
    fn receiver_burst_drains_ready_events_up_to_fairness_limit() {
        let workspace_id = WorkspaceId::new();
        let (tx, mut rx) =
            tokio::sync::broadcast::channel(WORKSPACE_STREAM_RECEIVER_DRAIN_LIMIT + 16);
        for rev in 1..=(WORKSPACE_STREAM_RECEIVER_DRAIN_LIMIT + 4) {
            tx.send(ready_event(
                workspace_id,
                i64::try_from(rev).expect("rev fits in i64"),
            ))
            .expect("receiver is open");
        }

        let first_event = rx.try_recv().expect("first event is ready");
        let burst = take_workspace_stream_receiver_burst(&mut rx, first_event);

        assert_eq!(burst.events.len(), WORKSPACE_STREAM_RECEIVER_DRAIN_LIMIT);
        assert!(burst.hit_limit);
        assert_eq!(burst.lagged, None);
        assert!(!burst.closed);
        assert!(matches!(
            rx.try_recv(),
            Ok(WorkspaceActiveSnapshotEvent::Ready { .. })
        ));
    }

    #[test]
    fn receiver_burst_reports_lag_after_collected_events() {
        let workspace_id = WorkspaceId::new();
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        tx.send(ready_event(workspace_id, 1))
            .expect("receiver is open");
        let first_event = rx.try_recv().expect("first event is ready");
        for rev in 2..=8 {
            tx.send(ready_event(workspace_id, rev))
                .expect("receiver is open");
        }

        let burst = take_workspace_stream_receiver_burst(&mut rx, first_event);

        assert_eq!(burst.events.len(), 1);
        assert_eq!(burst.lagged, Some(3));
        assert!(!burst.closed);
        assert!(!burst.hit_limit);
    }
}
