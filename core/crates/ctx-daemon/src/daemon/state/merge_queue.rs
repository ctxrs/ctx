use std::sync::Arc;

use crate::daemon::{
    merge_queue::MergeQueueRouteHost,
    merge_queue_route_handles::{
        MergeQueueNoticePublicationEffect, MergeQueueNoticePublicationFuture,
        MergeQueueNoticeSessionEvent,
    },
    DaemonState, ProtectedWorkspaceStoreLookup, SessionStoreLookup,
};

pub(in crate::daemon) fn merge_queue_route_host_from_state(
    state: &DaemonState,
) -> Arc<MergeQueueRouteHost> {
    let workspace_stores = ProtectedWorkspaceStoreLookup::new(
        state.core.stores.clone(),
        Arc::clone(&state.sessions),
        Arc::clone(&state.transport.merge_queue),
    );
    let session_stores =
        SessionStoreLookup::new(state.global_store().clone(), workspace_stores.clone());
    let publisher = state.session_publication.clone();
    let publish_merge_queue_notice: MergeQueueNoticePublicationEffect =
        Arc::new(move |notice_event: MergeQueueNoticeSessionEvent| {
            let publisher = publisher.clone();
            Box::pin(async move { publisher.publish_merge_queue_notice(notice_event).await })
                as MergeQueueNoticePublicationFuture
        });
    Arc::new(MergeQueueRouteHost::new(
        state.core.stores.clone(),
        state.global_store().clone(),
        workspace_stores,
        session_stores,
        Arc::clone(&state.transport.merge_queue),
        state.telemetry.ops_events.clone(),
        publish_merge_queue_notice,
    ))
}
