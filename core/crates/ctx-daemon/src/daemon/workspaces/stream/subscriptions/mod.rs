mod application;
mod planning;
mod resolution;

use std::collections::HashMap;

use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::{WorkspaceActiveSnapshotClientMessage, WorkspaceActiveSnapshotEvent};
use ctx_workspace_active_snapshot::{SessionReplayCursor, WorkspaceActiveSubscriptionState};
pub use ctx_workspace_stream_service::subscriptions::WorkspaceStreamSubscriptionResolutionError;

use crate::daemon::WorkspaceStreamHandle;

pub use application::{
    apply_workspace_stream_live_event, apply_workspace_stream_subscription_event,
    WorkspaceStreamLiveEventApplication, WorkspaceStreamSubscriptionEventApplication,
};
pub use planning::{
    finalize_workspace_stream_subscription_replay, plan_workspace_stream_subscription,
    plan_workspace_stream_subscription_transaction, WorkspaceStreamResolvedSession,
    WorkspaceStreamSessionPinChanges, WorkspaceStreamSessionReplay,
    WorkspaceStreamSubscriptionApplyPlan, WorkspaceStreamSubscriptionPlan,
    WorkspaceStreamSubscriptionReplayFinalization, WorkspaceStreamSubscriptionTransactionPlan,
};
pub use resolution::resolve_workspace_active_snapshot_subscriptions;

impl WorkspaceStreamHandle {
    pub fn finalize_workspace_stream_subscription_replay(
        &self,
        current_state: &WorkspaceActiveSubscriptionState,
        current_subscriptions: &HashMap<SessionId, SessionReplayCursor>,
        replayed_subscriptions: HashMap<SessionId, SessionReplayCursor>,
        transaction_sessions: &[WorkspaceStreamResolvedSession],
    ) -> WorkspaceStreamSubscriptionReplayFinalization {
        finalize_workspace_stream_subscription_replay(
            current_state,
            current_subscriptions,
            replayed_subscriptions,
            transaction_sessions,
        )
    }

    pub async fn apply_workspace_stream_subscription_event(
        &self,
        workspace_id: WorkspaceId,
        subscription_state: WorkspaceActiveSubscriptionState,
        subscriptions: HashMap<SessionId, SessionReplayCursor>,
        event: &WorkspaceActiveSnapshotEvent,
    ) -> WorkspaceStreamSubscriptionEventApplication {
        apply_workspace_stream_subscription_event(
            self,
            workspace_id,
            subscription_state,
            subscriptions,
            event,
        )
        .await
    }

    pub async fn apply_workspace_stream_live_event(
        &self,
        workspace_id: WorkspaceId,
        subscription_state: WorkspaceActiveSubscriptionState,
        subscriptions: HashMap<SessionId, SessionReplayCursor>,
        event: WorkspaceActiveSnapshotEvent,
    ) -> WorkspaceStreamLiveEventApplication {
        apply_workspace_stream_live_event(
            self,
            workspace_id,
            subscription_state,
            subscriptions,
            event,
        )
        .await
    }

    pub async fn resolve_workspace_active_snapshot_subscriptions(
        &self,
        workspace_id: WorkspaceId,
        message: WorkspaceActiveSnapshotClientMessage,
        existing: &HashMap<SessionId, SessionReplayCursor>,
    ) -> Result<WorkspaceStreamSubscriptionPlan, WorkspaceStreamSubscriptionResolutionError> {
        super::prepare_subscription_read_model(self, workspace_id)
            .await
            .map_err(|error| {
                tracing::error!(
                    target: "ctx_daemon.workspace_stream",
                    workspace_id = %workspace_id.0,
                    "workspace stream hydration failed while resolving subscriptions: {error:?}"
                );
                WorkspaceStreamSubscriptionResolutionError::Hydration
            })?;
        let resolved = resolution::resolve_workspace_active_snapshot_subscriptions(
            self,
            workspace_id,
            message.clone(),
            existing,
        )
        .await
        .map_err(|_| WorkspaceStreamSubscriptionResolutionError::Resolution)?;
        Ok(plan_workspace_stream_subscription(
            &message, resolved, existing,
        ))
    }

    pub async fn plan_workspace_stream_subscription_transaction(
        &self,
        workspace_id: WorkspaceId,
        message: WorkspaceActiveSnapshotClientMessage,
        current_subscriptions: &HashMap<SessionId, SessionReplayCursor>,
        current_fingerprint: Option<&str>,
    ) -> Result<
        WorkspaceStreamSubscriptionTransactionPlan,
        WorkspaceStreamSubscriptionResolutionError,
    > {
        super::prepare_subscription_read_model(self, workspace_id)
            .await
            .map_err(|error| {
                tracing::error!(
                    target: "ctx_daemon.workspace_stream",
                    workspace_id = %workspace_id.0,
                    "workspace stream hydration failed while planning subscription transaction: {error:?}"
                );
                WorkspaceStreamSubscriptionResolutionError::Hydration
            })?;
        let resolved = resolution::resolve_workspace_active_snapshot_subscriptions(
            self,
            workspace_id,
            message.clone(),
            current_subscriptions,
        )
        .await
        .map_err(|_| WorkspaceStreamSubscriptionResolutionError::Resolution)?;
        Ok(plan_workspace_stream_subscription_transaction(
            &message,
            resolved,
            current_subscriptions,
            current_fingerprint,
        ))
    }
}
