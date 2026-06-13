use std::collections::HashMap;

use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::WorkspaceActiveSnapshotEvent;
use ctx_workspace_active_snapshot::{SessionReplayCursor, WorkspaceActiveSubscriptionState};
use ctx_workspace_stream_service::subscriptions::application as stream_subscription_application;
pub use ctx_workspace_stream_service::subscriptions::application::{
    WorkspaceStreamLiveEventApplication, WorkspaceStreamSubscriptionEventApplication,
};

use crate::daemon::WorkspaceStreamHandle;

pub async fn apply_workspace_stream_subscription_event(
    handle: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    subscription_state: WorkspaceActiveSubscriptionState,
    subscriptions: HashMap<SessionId, SessionReplayCursor>,
    event: &WorkspaceActiveSnapshotEvent,
) -> WorkspaceStreamSubscriptionEventApplication {
    let seed = active_task_cursor_seed(
        handle,
        workspace_id,
        &subscription_state,
        &subscriptions,
        event,
    )
    .await;
    match stream_subscription_application::apply_workspace_stream_subscription_event(
        subscription_state,
        subscriptions,
        event,
        seed,
    ) {
        Ok(application) => application,
        Err(missing) => {
            let session_id = missing.session_id;
            let cursor =
                super::super::active_task_subscription_cursor(handle, workspace_id, session_id)
                    .await;
            stream_subscription_application::apply_missing_active_task_cursor(missing, cursor)
        }
    }
}

pub async fn apply_workspace_stream_live_event(
    handle: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    subscription_state: WorkspaceActiveSubscriptionState,
    subscriptions: HashMap<SessionId, SessionReplayCursor>,
    event: WorkspaceActiveSnapshotEvent,
) -> WorkspaceStreamLiveEventApplication {
    let application = apply_workspace_stream_subscription_event(
        handle,
        workspace_id,
        subscription_state,
        subscriptions,
        &event,
    )
    .await;
    stream_subscription_application::route_workspace_stream_live_event(application, event)
}

async fn active_task_cursor_seed(
    handle: &WorkspaceStreamHandle,
    workspace_id: WorkspaceId,
    subscription_state: &WorkspaceActiveSubscriptionState,
    subscriptions: &HashMap<SessionId, SessionReplayCursor>,
    event: &WorkspaceActiveSnapshotEvent,
) -> Option<stream_subscription_application::WorkspaceStreamActiveTaskCursorSeed> {
    let session_id = stream_subscription_application::active_task_cursor_seed_session(
        subscription_state,
        subscriptions,
        event,
    )?;
    let cursor =
        super::super::active_task_subscription_cursor(handle, workspace_id, session_id).await;
    Some(
        stream_subscription_application::WorkspaceStreamActiveTaskCursorSeed { session_id, cursor },
    )
}
