use std::collections::{HashMap, HashSet};

use ctx_core::ids::SessionId;
use ctx_core::models::{
    WorkspaceActiveSnapshotClientMessage, WorkspaceActiveSnapshotSessionIntent,
};
use ctx_workspace_active_snapshot::{
    ResolvedWorkspaceActiveSessionReplay, ResolvedWorkspaceActiveSessionSubscription,
    ResolvedWorkspaceActiveSubscriptions, SessionReplayCursor, WorkspaceActiveSubscriptionState,
};

#[derive(Clone, Debug)]
pub struct WorkspaceStreamSubscriptionPlan {
    pub include_initial_snapshot: bool,
    pub fingerprint: String,
    pub sessions: Vec<WorkspaceStreamResolvedSession>,
    pub state: WorkspaceActiveSubscriptionState,
    pub provisional_subscriptions: HashMap<SessionId, SessionReplayCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceStreamSessionPinChanges {
    pub attach: Vec<SessionId>,
    pub detach: Vec<SessionId>,
}

#[derive(Clone, Debug)]
pub enum WorkspaceStreamSubscriptionTransactionPlan {
    NoChange,
    Apply(Box<WorkspaceStreamSubscriptionApplyPlan>),
}

#[derive(Clone, Debug)]
pub struct WorkspaceStreamSubscriptionApplyPlan {
    pub include_initial_snapshot: bool,
    pub fingerprint: String,
    pub sessions: Vec<WorkspaceStreamResolvedSession>,
    pub state: WorkspaceActiveSubscriptionState,
    pub provisional_subscriptions: HashMap<SessionId, SessionReplayCursor>,
    pub pin_changes: WorkspaceStreamSessionPinChanges,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceStreamSubscriptionReplayFinalization {
    pub subscriptions: HashMap<SessionId, SessionReplayCursor>,
    pub pin_changes: WorkspaceStreamSessionPinChanges,
}

#[derive(Clone, Debug)]
pub struct WorkspaceStreamResolvedSession {
    pub session_id: SessionId,
    pub intent: WorkspaceActiveSnapshotSessionIntent,
    pub replay: WorkspaceStreamSessionReplay,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceStreamSessionReplay {
    Reset,
    Resume {
        after_seq: i64,
        after_projection_rev: i64,
    },
}

pub fn plan_workspace_stream_subscription(
    message: &WorkspaceActiveSnapshotClientMessage,
    resolved: ResolvedWorkspaceActiveSubscriptions,
    existing: &HashMap<SessionId, SessionReplayCursor>,
) -> WorkspaceStreamSubscriptionPlan {
    let include_initial_snapshot = matches!(
        message,
        WorkspaceActiveSnapshotClientMessage::Subscribe {
            include_active_heads: true,
            ..
        }
    );
    let ResolvedWorkspaceActiveSubscriptions { sessions, state } = resolved;
    let sessions = sessions
        .into_iter()
        .map(WorkspaceStreamResolvedSession::from)
        .collect::<Vec<_>>();
    let fingerprint =
        workspace_subscription_fingerprint(include_initial_snapshot, &sessions, &state);
    let provisional_subscriptions =
        provisional_subscriptions_for_resolved_sessions(&sessions, existing);
    WorkspaceStreamSubscriptionPlan {
        include_initial_snapshot,
        fingerprint,
        sessions,
        state,
        provisional_subscriptions,
    }
}

pub fn plan_workspace_stream_subscription_transaction(
    message: &WorkspaceActiveSnapshotClientMessage,
    resolved: ResolvedWorkspaceActiveSubscriptions,
    current_subscriptions: &HashMap<SessionId, SessionReplayCursor>,
    current_fingerprint: Option<&str>,
) -> WorkspaceStreamSubscriptionTransactionPlan {
    let plan = plan_workspace_stream_subscription(message, resolved, current_subscriptions);
    if current_fingerprint == Some(plan.fingerprint.as_str()) {
        return WorkspaceStreamSubscriptionTransactionPlan::NoChange;
    }
    let pin_changes = workspace_stream_session_pin_changes(
        current_subscriptions.keys().copied(),
        plan.provisional_subscriptions.keys().copied(),
    );
    WorkspaceStreamSubscriptionTransactionPlan::Apply(Box::new(
        WorkspaceStreamSubscriptionApplyPlan {
            include_initial_snapshot: plan.include_initial_snapshot,
            fingerprint: plan.fingerprint,
            sessions: plan.sessions,
            state: plan.state,
            provisional_subscriptions: plan.provisional_subscriptions,
            pin_changes,
        },
    ))
}

pub fn finalize_workspace_stream_subscription_replay(
    current_state: &WorkspaceActiveSubscriptionState,
    current_subscriptions: &HashMap<SessionId, SessionReplayCursor>,
    replayed_subscriptions: HashMap<SessionId, SessionReplayCursor>,
    transaction_sessions: &[WorkspaceStreamResolvedSession],
) -> WorkspaceStreamSubscriptionReplayFinalization {
    let head_targets = transaction_sessions
        .iter()
        .filter_map(|subscription| {
            (subscription.intent == WorkspaceActiveSnapshotSessionIntent::Head)
                .then_some(subscription.session_id)
        })
        .collect::<HashSet<_>>();
    let mut subscriptions = current_subscriptions
        .iter()
        .map(|(session_id, live_cursor)| {
            let last_sent = replayed_subscriptions
                .get(session_id)
                .map(|replayed_cursor| replayed_cursor.cover(*live_cursor))
                .unwrap_or(*live_cursor);
            (*session_id, last_sent)
        })
        .collect::<HashMap<_, _>>();
    for (session_id, replayed_cursor) in replayed_subscriptions {
        if subscriptions.contains_key(&session_id) {
            continue;
        }
        if head_targets.contains(&session_id)
            && subscription_state_contains_session(current_state, session_id)
        {
            subscriptions.insert(session_id, replayed_cursor);
        }
    }
    let pin_changes = workspace_stream_session_pin_changes(
        current_subscriptions.keys().copied(),
        subscriptions.keys().copied(),
    );
    WorkspaceStreamSubscriptionReplayFinalization {
        subscriptions,
        pin_changes,
    }
}

pub fn workspace_stream_session_pin_changes<I, J>(
    current: I,
    next: J,
) -> WorkspaceStreamSessionPinChanges
where
    I: IntoIterator<Item = SessionId>,
    J: IntoIterator<Item = SessionId>,
{
    let current = current.into_iter().collect::<HashSet<_>>();
    let next = next.into_iter().collect::<HashSet<_>>();
    let mut attach = next.difference(&current).copied().collect::<Vec<_>>();
    let mut detach = current.difference(&next).copied().collect::<Vec<_>>();
    attach.sort_by_key(|session_id| session_id.0);
    detach.sort_by_key(|session_id| session_id.0);
    WorkspaceStreamSessionPinChanges { attach, detach }
}

fn subscription_state_contains_session(
    state: &WorkspaceActiveSubscriptionState,
    session_id: SessionId,
) -> bool {
    state.explicit_sessions.contains(&session_id)
        || state.replay_sessions.contains(&session_id)
        || state
            .active_task_sessions
            .values()
            .any(|active_session_id| *active_session_id == session_id)
        || state
            .foreground_session_ids
            .as_ref()
            .is_some_and(|foreground| foreground.contains(&session_id))
}

fn workspace_subscription_fingerprint(
    include_initial_snapshot: bool,
    sessions: &[WorkspaceStreamResolvedSession],
    next_state: &WorkspaceActiveSubscriptionState,
) -> String {
    let mut sessions = sessions
        .iter()
        .map(|subscription| {
            let replay = match subscription.replay {
                WorkspaceStreamSessionReplay::Reset => "reset".to_string(),
                WorkspaceStreamSessionReplay::Resume {
                    after_seq,
                    after_projection_rev,
                } => format!("resume:{after_seq}:{after_projection_rev}"),
            };
            format!(
                "{}:{:?}:{}",
                subscription.session_id.0, subscription.intent, replay
            )
        })
        .collect::<Vec<_>>();
    sessions.sort();
    let mut foreground = next_state
        .foreground_session_ids
        .as_ref()
        .map(|ids| ids.iter().map(|id| id.0.to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    foreground.sort();
    format!(
        "heads={};active={};foreground={};sessions={}",
        include_initial_snapshot,
        next_state.active_scope,
        foreground.join(","),
        sessions.join("|")
    )
}

fn provisional_subscriptions_for_resolved_sessions(
    sessions: &[WorkspaceStreamResolvedSession],
    existing: &HashMap<SessionId, SessionReplayCursor>,
) -> HashMap<SessionId, SessionReplayCursor> {
    let mut provisional_subscriptions = HashMap::new();
    for subscription in sessions {
        let WorkspaceStreamSessionReplay::Resume {
            after_seq,
            after_projection_rev,
        } = subscription.replay
        else {
            continue;
        };
        let requested = SessionReplayCursor {
            last_event_seq: after_seq.max(0),
            projection_rev: after_projection_rev.max(0),
        };
        let last_sent = existing
            .get(&subscription.session_id)
            .copied()
            .map(|existing| existing.cover(requested))
            .unwrap_or(requested);
        provisional_subscriptions.insert(subscription.session_id, last_sent);
    }
    provisional_subscriptions
}

impl From<ResolvedWorkspaceActiveSessionSubscription> for WorkspaceStreamResolvedSession {
    fn from(value: ResolvedWorkspaceActiveSessionSubscription) -> Self {
        Self {
            session_id: value.session_id,
            intent: value.intent,
            replay: value.replay.into(),
        }
    }
}

impl From<ResolvedWorkspaceActiveSessionReplay> for WorkspaceStreamSessionReplay {
    fn from(value: ResolvedWorkspaceActiveSessionReplay) -> Self {
        match value {
            ResolvedWorkspaceActiveSessionReplay::Reset => Self::Reset,
            ResolvedWorkspaceActiveSessionReplay::Resume {
                after_seq,
                after_projection_rev,
            } => Self::Resume {
                after_seq,
                after_projection_rev,
            },
        }
    }
}
