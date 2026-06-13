use super::fixtures::{
    create_workspace_session, create_workspace_worktree, create_worktree_for_workspace, session_id,
    test_state, workspace_stream_handle, workspace_vcs_stream_handle,
};
use super::*;
use std::collections::{HashMap, HashSet};

use crate::daemon::{
    route_handles_from_state, workspace_vcs_stream_with_refresh_effect_from_state,
};
use chrono::Utc;
use ctx_core::ids::{
    MergeQueueEntryId, SessionEventId, SessionId, TaskId, TurnId, WorkspaceId, WorktreeId,
};
use ctx_core::models::{
    ExecutionEnvironment, MergeQueueEntry, MergeQueueEntryStatus, MergeQueuePatchSource,
    SessionActivityState, SessionEvent, SessionEventType, SessionHeadDelta, SessionHeadSnapshot,
    SessionHeadWindow, SessionMetadata, SessionSnapshotSummary, SessionStatus, SessionSummaryDelta,
    Task, TaskDelta, TaskDeltaKind, TaskStatus, WorkspaceActiveHeadBatch, WorkspaceActivePage,
    WorkspaceActiveSnapshot, WorkspaceActiveSnapshotClientMessage, WorkspaceActiveSnapshotEvent,
    WorkspaceActiveSnapshotSessionIntent, WorkspaceActiveSnapshotSessionReplay,
    WorkspaceActiveSnapshotSessionSubscription, WorkspaceActiveTaskSummary, WorkspaceTaskSummary,
    Worktree, WorktreeBootstrapNotice, WorktreeBootstrapStatus, WorktreeVcsBaseResolution,
    WorktreeVcsComputeState, WorktreeVcsFreshness, WorktreeVcsGitStatusSummary,
    WorktreeVcsSnapshot, WorktreeVcsStreamTier, WorktreeVcsSummary, WorktreeVcsTouchedFiles,
    WorktreeVcsTouchedFilesState,
};
use ctx_workspace_active_snapshot::{
    ResolvedWorkspaceActiveSessionReplay, ResolvedWorkspaceActiveSessionSubscription,
    ResolvedWorkspaceActiveSubscriptions, SessionReplayCursor, WorkspaceActiveSubscriptionState,
};
use ctx_workspace_config::{update_merge_queue_config, MergeQueueConfigUpdate};
use std::sync::Arc;

fn test_vcs_snapshot(
    worktree_id: WorktreeId,
    freshness: WorktreeVcsFreshness,
    touched_files_state: WorktreeVcsTouchedFilesState,
) -> WorktreeVcsSnapshot {
    WorktreeVcsSnapshot {
        worktree_id,
        rev: 1,
        emitted_at_ms: 1,
        base_commit_sha: "base".to_string(),
        head_commit_sha: "head".to_string(),
        target_branch: Some("origin/main".to_string()),
        target_branch_commit_sha: Some("target".to_string()),
        base_resolution: WorktreeVcsBaseResolution::default(),
        compute_state: WorktreeVcsComputeState::Ready,
        summary: WorktreeVcsSummary {
            file_count: Some(1),
            line_additions: Some(1),
            line_deletions: Some(0),
            line_count: Some(1),
        },
        git_status: WorktreeVcsGitStatusSummary::default(),
        touched_files: WorktreeVcsTouchedFiles::default(),
        touched_files_state,
        freshness,
        available: true,
        unavailable_reason: None,
        schema_version: 1,
    }
}

fn task(workspace_id: WorkspaceId, primary_session_id: Option<SessionId>) -> Task {
    Task {
        id: TaskId::new(),
        workspace_id,
        title: "task".to_string(),
        description: None,
        status: TaskStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        exec_plan_id: None,
        primary_session_id,
        primary_worktree_id: None,
        archived_at: None,
        assistant_seen_at: None,
        last_activity_at: None,
        last_assistant_message_at: None,
        has_active_session: primary_session_id.is_some(),
    }
}

fn queued_merge_queue_entry(workspace_id: WorkspaceId) -> MergeQueueEntry {
    let now = Utc::now();
    MergeQueueEntry {
        id: MergeQueueEntryId::new(),
        workspace_id,
        worktree_id: None,
        session_id: None,
        target_branch: "main".to_string(),
        message: Some("workspace stream activation".to_string()),
        patch_source: MergeQueuePatchSource::Generated,
        base_commit_sha: Some("base".to_string()),
        head_commit_sha: Some("head".to_string()),
        patch_path: "/tmp/workspace-stream-activation.patch".to_string(),
        patch_size: 1,
        status: MergeQueueEntryStatus::Queued,
        result_commit_sha: None,
        error_message: None,
        created_at: now,
        updated_at: now,
    }
}

fn cursor(last_event_seq: i64, projection_rev: i64) -> SessionReplayCursor {
    SessionReplayCursor {
        last_event_seq,
        projection_rev,
    }
}

fn resolved_stream_session(
    session_id: SessionId,
    intent: WorkspaceActiveSnapshotSessionIntent,
    replay: WorkspaceStreamSessionReplay,
) -> WorkspaceStreamResolvedSession {
    WorkspaceStreamResolvedSession {
        session_id,
        intent,
        replay,
    }
}

fn test_session_metadata(workspace_id: WorkspaceId, session_id: SessionId) -> SessionMetadata {
    SessionMetadata {
        id: session_id,
        task_id: TaskId::new(),
        workspace_id,
        worktree_id: WorktreeId::new(),
        execution_environment: ExecutionEnvironment::Host,
        parent_session_id: None,
        relationship: None,
        provider_id: "fake".to_string(),
        model_id: "fake-model".to_string(),
        reasoning_effort: None,
        title: "test".to_string(),
        agent_role: "assistant".to_string(),
        status: SessionStatus::Active,
        provider_session_ref: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn test_head(
    workspace_id: WorkspaceId,
    session_id: SessionId,
    last_event_seq: i64,
    projection_rev: i64,
) -> SessionHeadSnapshot {
    SessionHeadSnapshot {
        session: test_session_metadata(workspace_id, session_id),
        turns: Vec::new(),
        tool_summaries: Vec::new(),
        events: Vec::new(),
        messages: Vec::new(),
        last_event_seq,
        projection_rev,
        state_rev: projection_rev,
        activity: SessionActivityState::default(),
        has_more_turns: false,
        history_cursor: None,
        has_more_history: false,
        summary_checkpoint: None,
        head_window: SessionHeadWindow::default(),
    }
}

fn partial_delta(session_id: SessionId) -> SessionHeadDelta {
    SessionHeadDelta {
        session_id,
        last_event_seq: 5,
        projection_rev: 7,
        state_rev: 0,
        emitted_at_ms: None,
        session: None,
        activity: None,
        event: Some(SessionEvent {
            seq: -1,
            id: SessionEventId::new(),
            session_id,
            run_id: None,
            turn_id: Some(TurnId::new()),
            event_type: SessionEventType::AssistantChunk,
            payload_json: serde_json::json!({ "content_fragment": "partial" }),
            transient: true,
            created_at: Utc::now(),
        }),
        turn: None,
        message: None,
        tool_summaries: Vec::new(),
    }
}

fn durable_delta(
    session_id: SessionId,
    last_event_seq: i64,
    projection_rev: i64,
) -> SessionHeadDelta {
    SessionHeadDelta {
        session_id,
        last_event_seq,
        projection_rev,
        state_rev: projection_rev,
        emitted_at_ms: None,
        session: None,
        activity: None,
        event: None,
        turn: None,
        message: None,
        tool_summaries: Vec::new(),
    }
}

fn summary_delta_event(
    workspace_id: WorkspaceId,
    session_id: SessionId,
) -> WorkspaceActiveSnapshotEvent {
    WorkspaceActiveSnapshotEvent::SessionSummaryDelta {
        workspace_id,
        snapshot_rev: 3,
        delta: Box::new(SessionSummaryDelta {
            session_id,
            task_id: TaskId::new(),
            activity: None,
            last_message_at: None,
            last_message_preview: Some("summary".to_string()),
            last_event_seq: Some(5),
            projection_rev: Some(7),
            state_rev: Some(7),
            emitted_at_ms: None,
        }),
    }
}

fn active_task_summary(
    workspace_id: WorkspaceId,
    task_id: TaskId,
    session_id: SessionId,
) -> WorkspaceActiveTaskSummary {
    let mut task = task(workspace_id, Some(session_id));
    task.id = task_id;
    WorkspaceActiveTaskSummary {
        task,
        primary_session: session_summary(workspace_id, session_id),
        primary_session_head: None,
        sessions: Vec::new(),
        sort_at: Utc::now(),
    }
}

fn session_summary(workspace_id: WorkspaceId, session_id: SessionId) -> SessionSnapshotSummary {
    SessionSnapshotSummary {
        session: test_session_metadata(workspace_id, session_id),
        last_message_at: None,
        last_message_preview: None,
        last_event_seq: None,
        projection_rev: 0,
        state_rev: 0,
        activity: SessionActivityState::default(),
        unread: None,
    }
}

fn active_task(workspace_id: WorkspaceId, session_id: SessionId) -> WorkspaceActiveTaskSummary {
    WorkspaceActiveTaskSummary {
        task: task(workspace_id, Some(session_id)),
        primary_session: session_summary(workspace_id, session_id),
        primary_session_head: None,
        sessions: Vec::new(),
        sort_at: Utc::now(),
    }
}

fn vcs_seed_pairs(plan: &WorkspaceVcsLagReseedPlan) -> Vec<(WorktreeId, WorktreeVcsStreamTier)> {
    plan.seeds
        .iter()
        .map(|seed| (seed.worktree_id, seed.tier))
        .collect()
}

#[test]
fn event_routing_head_delta_applicability_preserves_subscription_rules() {
    let session_id = SessionId::new();
    assert!(should_stream_head_delta(
        &HashMap::from([(TaskId::new(), session_id)]),
        &HashSet::new(),
        None,
        session_id,
    ));
    assert!(should_stream_head_delta(
        &HashMap::new(),
        &HashSet::from([session_id]),
        None,
        session_id,
    ));
    assert!(should_stream_head_delta(
        &HashMap::new(),
        &HashSet::new(),
        Some(&HashSet::from([session_id])),
        session_id,
    ));
    assert!(!should_stream_head_delta(
        &HashMap::new(),
        &HashSet::new(),
        None,
        session_id,
    ));
}

#[test]
fn event_routing_partial_delta_filtering_preserves_foreground_rules() {
    let session_id = SessionId::new();

    assert!(
        filter_partial_delta_for_active_tasks(partial_delta(session_id), None).is_none(),
        "partial-only background deltas must be dropped",
    );

    let filtered = filter_partial_delta_for_active_tasks(
        partial_delta(session_id),
        Some(&HashSet::from([session_id])),
    )
    .expect("foreground partial delta should be preserved");
    assert!(filtered.event.is_some());
}

#[test]
fn event_routing_priority_control_only_matches_foreground_session_events() {
    let workspace_id = WorkspaceId::new();
    let foreground_session_id = SessionId::new();
    let background_session_id = SessionId::new();
    let foreground = HashSet::from([foreground_session_id]);
    let foreground_gap = WorkspaceActiveSnapshotEvent::SessionGap {
        workspace_id,
        snapshot_rev: 1,
        session_id: foreground_session_id,
        after_seq: 1,
        reason: Some("foreground".to_string()),
        seed_follows: false,
    };
    let background_seed = WorkspaceActiveSnapshotEvent::SessionHeadSeed {
        workspace_id,
        snapshot_rev: 1,
        head: Box::new(test_head(workspace_id, background_session_id, 1, 1)),
    };

    assert!(is_priority_control_event(
        &foreground_gap,
        Some(&foreground),
    ));
    assert!(!is_priority_control_event(
        &background_seed,
        Some(&foreground),
    ));
}

#[test]
fn event_routing_pending_replay_blockers_cover_full_event_surface() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let task_id = TaskId::new();
    let pending = HashSet::from([session_id]);
    let active_task_sessions = HashMap::from([(task_id, session_id)]);
    let blocking = vec![
        WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
            workspace_id,
            snapshot_rev: 1,
            task: Box::new(active_task(workspace_id, session_id)),
        },
        WorkspaceActiveSnapshotEvent::ActiveTaskDelete {
            workspace_id,
            snapshot_rev: 1,
            task_id,
        },
        WorkspaceActiveSnapshotEvent::TaskDelta {
            workspace_id,
            snapshot_rev: 1,
            delta: Box::new(TaskDelta {
                task: task(workspace_id, Some(session_id)),
                kind: TaskDeltaKind::Updated,
            }),
        },
        WorkspaceActiveSnapshotEvent::SessionSummary {
            workspace_id,
            snapshot_rev: 1,
            summary: Box::new(session_summary(workspace_id, session_id)),
        },
        WorkspaceActiveSnapshotEvent::SessionSummaryDelta {
            workspace_id,
            snapshot_rev: 1,
            delta: Box::new(SessionSummaryDelta {
                session_id,
                task_id,
                activity: None,
                last_message_at: None,
                last_message_preview: None,
                last_event_seq: None,
                projection_rev: None,
                state_rev: None,
                emitted_at_ms: None,
            }),
        },
        WorkspaceActiveSnapshotEvent::SessionRemoved {
            workspace_id,
            snapshot_rev: 1,
            session_id,
        },
        WorkspaceActiveSnapshotEvent::SessionGap {
            workspace_id,
            snapshot_rev: 1,
            session_id,
            after_seq: 1,
            reason: None,
            seed_follows: false,
        },
        WorkspaceActiveSnapshotEvent::SessionHeadDelta {
            workspace_id,
            snapshot_rev: 1,
            delta: Box::new(partial_delta(session_id)),
        },
        WorkspaceActiveSnapshotEvent::SessionHeadSeed {
            workspace_id,
            snapshot_rev: 1,
            head: Box::new(test_head(workspace_id, session_id, 1, 1)),
        },
    ];
    for event in blocking {
        assert!(
            event_blocks_pending_replay_with_active_task_sessions(
                &event,
                &pending,
                &active_task_sessions,
            ),
            "event should block pending replay: {event:?}",
        );
    }

    let nonblocking = vec![
        WorkspaceActiveSnapshotEvent::Ready {
            workspace_id,
            snapshot_rev: 1,
            archived_rev: 0,
        },
        WorkspaceActiveSnapshotEvent::WorktreeBootstrap {
            workspace_id,
            snapshot_rev: 1,
            notice: WorktreeBootstrapNotice {
                worktree_id: WorktreeId::new(),
                worktree_root: "/tmp/worktree".to_string(),
                status: WorktreeBootstrapStatus::Success,
                started_at: Utc::now(),
                finished_at: Utc::now(),
                exit_code: Some(0),
                timeout_sec: None,
                command: None,
                script_path: None,
                log_path: None,
                log_truncated: None,
                error: None,
            },
        },
        WorkspaceActiveSnapshotEvent::ArchivedTaskUpsert {
            workspace_id,
            archived_rev: 2,
            task: Box::new(WorkspaceTaskSummary {
                task: task(workspace_id, Some(session_id)),
                provider_ids: Vec::new(),
                sessions: Vec::new(),
                sort_at: Utc::now(),
            }),
        },
        WorkspaceActiveSnapshotEvent::ArchivedTaskDelete {
            workspace_id,
            archived_rev: 2,
            task_id,
        },
    ];
    for event in nonblocking {
        assert!(
            !event_blocks_pending_replay_with_active_task_sessions(
                &event,
                &pending,
                &active_task_sessions,
            ),
            "event should not block pending replay: {event:?}",
        );
    }
}

#[test]
fn event_routing_pending_replay_uses_subscription_state_for_active_task_delete() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let task_id = TaskId::new();
    let pending = HashSet::from([session_id]);
    let event = WorkspaceActiveSnapshotEvent::ActiveTaskDelete {
        workspace_id,
        snapshot_rev: 1,
        task_id,
    };

    assert!(
        event_blocks_pending_replay(
            &event,
            &pending,
            &WorkspaceActiveSubscriptionState {
                active_task_sessions: HashMap::from([(task_id, session_id)]),
                ..WorkspaceActiveSubscriptionState::default()
            },
        ),
        "delete for an active task whose primary session is pending must block replay",
    );
    assert!(
        !event_blocks_pending_replay(
            &event,
            &pending,
            &WorkspaceActiveSubscriptionState {
                active_task_sessions: HashMap::from([(task_id, SessionId::new())]),
                ..WorkspaceActiveSubscriptionState::default()
            },
        ),
        "delete for an unrelated active task session must not block pending replay",
    );
}

#[test]
fn event_routing_snapshot_rev_extraction_preserves_active_snapshot_events() {
    let workspace_id = WorkspaceId::new();
    assert_eq!(
        event_snapshot_rev(&WorkspaceActiveSnapshotEvent::Ready {
            workspace_id,
            snapshot_rev: 7,
            archived_rev: 0,
        }),
        Some(7),
    );
    assert_eq!(
        event_snapshot_rev(&WorkspaceActiveSnapshotEvent::ArchivedTaskDelete {
            workspace_id,
            archived_rev: 9,
            task_id: TaskId::new(),
        }),
        None,
    );
}

#[tokio::test]
async fn subscription_resolution_filters_cross_workspace_session_references() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let (workspace_a, session_a) = create_workspace_session(&state, root.path()).await;
    let (_workspace_b, session_b) = create_workspace_session(&state, root.path()).await;

    let resolved = resolve_workspace_active_snapshot_subscriptions(
        &workspace_stream_handle(&state),
        workspace_a,
        WorkspaceActiveSnapshotClientMessage::Subscribe {
            session_ids: vec![session_a, session_b],
            sessions: vec![
                ctx_core::models::WorkspaceActiveSnapshotSessionSubscription {
                    session_id: session_b,
                    intent: None,
                    replay: WorkspaceActiveSnapshotSessionReplay::Reset,
                },
            ],
            task_ids: Vec::new(),
            foreground_session_id: Some(session_b),
            scope: None,
            include_active_heads: false,
        },
        &HashMap::new(),
    )
    .await
    .unwrap();

    assert_eq!(resolved.sessions.len(), 1);
    assert_eq!(resolved.sessions[0].session_id, session_a);
    assert!(resolved.state.foreground_session_ids.is_none());
    assert_eq!(resolved.state.explicit_sessions, HashSet::from([session_a]));
}

#[test]
fn subscription_plan_derives_initial_snapshot_fingerprint_and_provisional_cursors() {
    let session_id = session_id("00000000-0000-0000-0000-000000000001");
    let task_id = TaskId::new();
    let message = WorkspaceActiveSnapshotClientMessage::Subscribe {
        session_ids: vec![session_id],
        sessions: vec![WorkspaceActiveSnapshotSessionSubscription {
            session_id,
            intent: Some(WorkspaceActiveSnapshotSessionIntent::Replay),
            replay: WorkspaceActiveSnapshotSessionReplay::Resume {
                after_seq: 10,
                after_projection_rev: 12,
            },
        }],
        task_ids: vec![task_id],
        foreground_session_id: Some(session_id),
        scope: None,
        include_active_heads: true,
    };
    let mut state = WorkspaceActiveSubscriptionState::default();
    state.active_scope = true;
    state.foreground_session_ids = Some(HashSet::from([session_id]));
    let resolved = ResolvedWorkspaceActiveSubscriptions {
        sessions: vec![ResolvedWorkspaceActiveSessionSubscription {
            session_id,
            intent: WorkspaceActiveSnapshotSessionIntent::Replay,
            replay: ResolvedWorkspaceActiveSessionReplay::Resume {
                after_seq: 10,
                after_projection_rev: 12,
            },
        }],
        state,
    };
    let existing = HashMap::from([(
        session_id,
        SessionReplayCursor {
            last_event_seq: 15,
            projection_rev: 16,
        },
    )]);

    let plan = plan_workspace_stream_subscription(&message, resolved, &existing);

    assert!(plan.include_initial_snapshot);
    assert_eq!(
        plan.provisional_subscriptions.get(&session_id).copied(),
        Some(SessionReplayCursor {
            last_event_seq: 15,
            projection_rev: 16,
        }),
        "existing live cursor must cover older requested replay cursor",
    );
    assert!(
        plan.fingerprint.starts_with("heads=true;active=true;"),
        "fingerprint must use the daemon-derived include_initial_snapshot flag",
    );
    assert_eq!(plan.sessions.len(), 1);
    assert!(matches!(
        plan.sessions[0].replay,
        WorkspaceStreamSessionReplay::Resume {
            after_seq: 10,
            after_projection_rev: 12,
        }
    ));
}

#[test]
fn subscription_transaction_returns_no_change_for_matching_fingerprint() {
    let session_id = session_id("00000000-0000-0000-0000-000000000011");
    let message = WorkspaceActiveSnapshotClientMessage::Subscribe {
        session_ids: vec![session_id],
        sessions: vec![WorkspaceActiveSnapshotSessionSubscription {
            session_id,
            intent: Some(WorkspaceActiveSnapshotSessionIntent::Replay),
            replay: WorkspaceActiveSnapshotSessionReplay::Auto,
        }],
        task_ids: Vec::new(),
        foreground_session_id: None,
        scope: None,
        include_active_heads: false,
    };
    let resolved = ResolvedWorkspaceActiveSubscriptions {
        sessions: vec![ResolvedWorkspaceActiveSessionSubscription {
            session_id,
            intent: WorkspaceActiveSnapshotSessionIntent::Replay,
            replay: ResolvedWorkspaceActiveSessionReplay::Resume {
                after_seq: 5,
                after_projection_rev: 6,
            },
        }],
        state: WorkspaceActiveSubscriptionState::default(),
    };
    let current = HashMap::from([(session_id, cursor(5, 6))]);
    let fingerprint = plan_workspace_stream_subscription(&message, resolved, &current).fingerprint;
    let resolved = ResolvedWorkspaceActiveSubscriptions {
        sessions: vec![ResolvedWorkspaceActiveSessionSubscription {
            session_id,
            intent: WorkspaceActiveSnapshotSessionIntent::Replay,
            replay: ResolvedWorkspaceActiveSessionReplay::Resume {
                after_seq: 5,
                after_projection_rev: 6,
            },
        }],
        state: WorkspaceActiveSubscriptionState::default(),
    };

    let transaction = plan_workspace_stream_subscription_transaction(
        &message,
        resolved,
        &current,
        Some(fingerprint.as_str()),
    );

    assert!(matches!(
        transaction,
        WorkspaceStreamSubscriptionTransactionPlan::NoChange
    ));
}

#[test]
fn subscription_transaction_plans_provisional_cursors_and_pin_deltas() {
    let old_session_id = session_id("00000000-0000-0000-0000-000000000012");
    let new_session_id = session_id("00000000-0000-0000-0000-000000000013");
    let message = WorkspaceActiveSnapshotClientMessage::Subscribe {
        session_ids: vec![new_session_id],
        sessions: vec![WorkspaceActiveSnapshotSessionSubscription {
            session_id: new_session_id,
            intent: Some(WorkspaceActiveSnapshotSessionIntent::Replay),
            replay: WorkspaceActiveSnapshotSessionReplay::Resume {
                after_seq: 10,
                after_projection_rev: 12,
            },
        }],
        task_ids: Vec::new(),
        foreground_session_id: None,
        scope: None,
        include_active_heads: false,
    };
    let resolved = ResolvedWorkspaceActiveSubscriptions {
        sessions: vec![ResolvedWorkspaceActiveSessionSubscription {
            session_id: new_session_id,
            intent: WorkspaceActiveSnapshotSessionIntent::Replay,
            replay: ResolvedWorkspaceActiveSessionReplay::Resume {
                after_seq: 10,
                after_projection_rev: 12,
            },
        }],
        state: WorkspaceActiveSubscriptionState::default(),
    };
    let current = HashMap::from([(old_session_id, cursor(30, 31))]);

    let WorkspaceStreamSubscriptionTransactionPlan::Apply(plan) =
        plan_workspace_stream_subscription_transaction(&message, resolved, &current, None)
    else {
        panic!("changed transaction should produce an apply plan");
    };

    assert_eq!(
        plan.provisional_subscriptions.get(&new_session_id).copied(),
        Some(cursor(10, 12))
    );
    assert_eq!(plan.pin_changes.attach, vec![new_session_id]);
    assert_eq!(plan.pin_changes.detach, vec![old_session_id]);
}

#[test]
fn snapshot_read_model_active_head_cursors_use_delivered_heads() {
    let workspace_id = ctx_core::ids::WorkspaceId::new();
    let session_id = ctx_core::ids::SessionId::new();
    let read_model = WorkspaceStreamSnapshotReadModel {
        active_snapshot: WorkspaceActiveSnapshot {
            workspace_id,
            snapshot_rev: 500,
            archived_rev: 0,
            active: WorkspaceActivePage {
                tasks: Vec::new(),
                total_count: 0,
            },
        },
        active_heads: WorkspaceActiveHeadBatch {
            workspace_id,
            snapshot_rev: 400,
            heads: vec![test_head(workspace_id, session_id, 17, 23)],
        },
    };

    let cursors = active_head_cursors_from_snapshot_read_model(&read_model);

    assert_eq!(
        cursors.get(&session_id).copied(),
        Some(cursor(17, 23)),
        "replay cursor seeding must use the delivered active-head batch, not a later read",
    );
}

#[test]
fn resume_replay_cursor_planning_preserves_live_coverage_semantics() {
    assert_eq!(
        plan_resume_replay_cursor(Some(cursor(15, 16)), 10, 12),
        WorkspaceStreamResumeReplayCursorPlan::Replay {
            cursor: cursor(15, 16),
        },
    );
    assert_eq!(
        plan_resume_replay_cursor(Some(cursor(15, 16)), 20, 0),
        WorkspaceStreamResumeReplayCursorPlan::Replay {
            cursor: cursor(20, i64::MAX),
        },
        "zero projection revision requests must preserve the prior i64::MAX fallback",
    );
    assert_eq!(
        plan_resume_replay_cursor(None, 10, 12),
        WorkspaceStreamResumeReplayCursorPlan::NoReplayRequired,
        "the existing no-live-cursor path skips replay work",
    );
}

#[test]
fn cursor_acceptance_preserves_transient_delta_without_advancing() {
    let current = cursor(5, 7);
    let accepted = accept_session_delta_cursor(current, &partial_delta(SessionId::new()));

    assert_eq!(
        accepted,
        WorkspaceStreamCursorAcceptance {
            accepted: true,
            next_cursor: current,
        },
    );
}

#[test]
fn cursor_acceptance_rejects_stale_durable_delta_and_advances_newer_delta() {
    let session_id = SessionId::new();
    let current = cursor(5, 7);

    assert_eq!(
        accept_session_delta_cursor(current, &durable_delta(session_id, 5, 7)),
        WorkspaceStreamCursorAcceptance {
            accepted: false,
            next_cursor: current,
        },
    );
    assert_eq!(
        accept_session_delta_cursor(current, &durable_delta(session_id, 5, 8)),
        WorkspaceStreamCursorAcceptance {
            accepted: true,
            next_cursor: cursor(5, 8),
        },
    );
}

#[test]
fn cursor_acceptance_rejects_stale_head_and_advances_newer_head() {
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let current = cursor(5, 7);

    assert_eq!(
        accept_session_head_cursor(current, &test_head(workspace_id, session_id, 5, 7)),
        WorkspaceStreamCursorAcceptance {
            accepted: false,
            next_cursor: current,
        },
    );
    assert_eq!(
        accept_session_head_cursor(current, &test_head(workspace_id, session_id, 6, 7)),
        WorkspaceStreamCursorAcceptance {
            accepted: true,
            next_cursor: cursor(6, 7),
        },
    );
}

#[test]
fn queue_stale_drop_predicates_match_replay_cursor_ordering() {
    let session_id = SessionId::new();
    let cursor = cursor(3, 7);
    assert!(!is_session_head_delta_after_cursor(
        &durable_delta(session_id, 3, 7),
        cursor,
    ));
    assert!(is_session_head_delta_after_cursor(
        &durable_delta(session_id, 3, 8),
        cursor,
    ));

    let stale_summary = SessionSummaryDelta {
        session_id,
        task_id: TaskId::new(),
        activity: None,
        last_message_at: None,
        last_message_preview: None,
        last_event_seq: Some(3),
        projection_rev: Some(7),
        state_rev: None,
        emitted_at_ms: None,
    };
    let newer_summary = SessionSummaryDelta {
        projection_rev: Some(8),
        ..stale_summary.clone()
    };
    let uncursored_summary = SessionSummaryDelta {
        last_event_seq: None,
        ..stale_summary.clone()
    };

    assert!(!is_session_summary_delta_after_cursor(
        &stale_summary,
        cursor,
    ));
    assert!(is_session_summary_delta_after_cursor(
        &newer_summary,
        cursor,
    ));
    assert!(is_session_summary_delta_after_cursor(
        &uncursored_summary,
        cursor,
    ));
}

#[test]
fn replay_live_cursor_merge_keeps_live_authoritative() {
    let replayed_session_id = SessionId::new();
    let live_only_session_id = SessionId::new();
    let removed_session_id = SessionId::new();
    let live_subscriptions = HashMap::from([
        (replayed_session_id, cursor(15, 15)),
        (live_only_session_id, cursor(7, 7)),
    ]);
    let replayed_subscriptions = HashMap::from([
        (replayed_session_id, cursor(12, 12)),
        (removed_session_id, cursor(20, 20)),
    ]);

    let merged =
        merge_replayed_and_live_subscription_cursors(&live_subscriptions, replayed_subscriptions);

    assert_eq!(merged.len(), 2);
    assert_eq!(
        merged.get(&replayed_session_id).copied(),
        Some(cursor(15, 15))
    );
    assert_eq!(
        merged.get(&live_only_session_id).copied(),
        Some(cursor(7, 7))
    );
    assert!(
        !merged.contains_key(&removed_session_id),
        "live subscription state must remain authoritative for removed sessions",
    );
}

#[test]
fn replay_finalization_adds_still_subscribed_head_only_sessions() {
    let live_session_id = SessionId::new();
    let head_session_id = SessionId::new();
    let mut state = WorkspaceActiveSubscriptionState::default();
    state.explicit_sessions.insert(head_session_id);
    let current = HashMap::from([(live_session_id, cursor(7, 7))]);
    let replayed = HashMap::from([(head_session_id, cursor(20, 21))]);

    let finalization = finalize_workspace_stream_subscription_replay(
        &state,
        &current,
        replayed,
        &[resolved_stream_session(
            head_session_id,
            WorkspaceActiveSnapshotSessionIntent::Head,
            WorkspaceStreamSessionReplay::Reset,
        )],
    );

    assert_eq!(
        finalization.subscriptions.get(&head_session_id).copied(),
        Some(cursor(20, 21))
    );
    assert_eq!(finalization.pin_changes.attach, vec![head_session_id]);
    assert!(finalization.pin_changes.detach.is_empty());
}

#[test]
fn replay_finalization_ignores_head_only_sessions_removed_during_replay() {
    let head_session_id = SessionId::new();
    let current = HashMap::new();
    let replayed = HashMap::from([(head_session_id, cursor(20, 21))]);

    let finalization = finalize_workspace_stream_subscription_replay(
        &WorkspaceActiveSubscriptionState::default(),
        &current,
        replayed,
        &[resolved_stream_session(
            head_session_id,
            WorkspaceActiveSnapshotSessionIntent::Head,
            WorkspaceStreamSessionReplay::Reset,
        )],
    );

    assert!(!finalization.subscriptions.contains_key(&head_session_id));
    assert!(finalization.pin_changes.attach.is_empty());
    assert!(finalization.pin_changes.detach.is_empty());
}

#[test]
fn replay_finalization_keeps_live_cursor_authoritative() {
    let session_id = SessionId::new();
    let live_only_session_id = SessionId::new();
    let removed_session_id = SessionId::new();
    let current = HashMap::from([
        (session_id, cursor(15, 16)),
        (live_only_session_id, cursor(7, 8)),
    ]);
    let replayed = HashMap::from([
        (session_id, cursor(10, 12)),
        (removed_session_id, cursor(30, 31)),
    ]);

    let finalization = finalize_workspace_stream_subscription_replay(
        &WorkspaceActiveSubscriptionState::default(),
        &current,
        replayed,
        &[resolved_stream_session(
            session_id,
            WorkspaceActiveSnapshotSessionIntent::Replay,
            WorkspaceStreamSessionReplay::Resume {
                after_seq: 10,
                after_projection_rev: 12,
            },
        )],
    );

    assert_eq!(
        finalization.subscriptions.get(&session_id).copied(),
        Some(cursor(15, 16))
    );
    assert_eq!(
        finalization
            .subscriptions
            .get(&live_only_session_id)
            .copied(),
        Some(cursor(7, 8))
    );
    assert!(!finalization.subscriptions.contains_key(&removed_session_id));
    assert!(finalization.pin_changes.attach.is_empty());
    assert!(finalization.pin_changes.detach.is_empty());
}

#[tokio::test]
async fn subscription_event_session_removed_updates_state_without_active_scope() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let mut subscription_state = WorkspaceActiveSubscriptionState::default();
    subscription_state.explicit_sessions.insert(session_id);
    subscription_state.replay_sessions.insert(session_id);
    subscription_state.foreground_session_ids = Some(HashSet::from([session_id]));
    let subscriptions = HashMap::from([(session_id, cursor(10, 11))]);

    let applied = apply_workspace_stream_subscription_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state,
        subscriptions,
        &WorkspaceActiveSnapshotEvent::SessionRemoved {
            workspace_id,
            snapshot_rev: 1,
            session_id,
        },
    )
    .await;

    assert!(applied.should_route);
    assert!(!applied.state.active_scope);
    assert!(applied.state.explicit_sessions.is_empty());
    assert!(applied.state.replay_sessions.is_empty());
    assert!(applied.state.foreground_session_ids.is_none());
    assert!(applied.subscriptions.is_empty());
    assert_eq!(applied.pin_changes.detach, vec![session_id]);
}

#[tokio::test]
async fn subscription_event_active_task_upsert_seeds_missing_and_preserves_existing_cursor() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = WorkspaceId::new();
    let seeded_task_id = TaskId::new();
    let seeded_session_id = SessionId::new();
    let existing_task_id = TaskId::new();
    let existing_session_id = SessionId::new();
    let mut subscription_state = WorkspaceActiveSubscriptionState::default();
    subscription_state.active_scope = true;

    let seeded = apply_workspace_stream_subscription_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state.clone(),
        HashMap::new(),
        &WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
            workspace_id,
            snapshot_rev: 1,
            task: Box::new(active_task_summary(
                workspace_id,
                seeded_task_id,
                seeded_session_id,
            )),
        },
    )
    .await;
    assert!(seeded.should_route);
    assert_eq!(
        seeded
            .state
            .active_task_sessions
            .get(&seeded_task_id)
            .copied(),
        Some(seeded_session_id),
    );
    assert_eq!(
        seeded.subscriptions.get(&seeded_session_id).copied(),
        Some(SessionReplayCursor::default()),
    );
    assert_eq!(seeded.pin_changes.attach, vec![seeded_session_id]);

    let existing_cursor = cursor(12, 13);
    let preserved = apply_workspace_stream_subscription_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state,
        HashMap::from([(existing_session_id, existing_cursor)]),
        &WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
            workspace_id,
            snapshot_rev: 2,
            task: Box::new(active_task_summary(
                workspace_id,
                existing_task_id,
                existing_session_id,
            )),
        },
    )
    .await;
    assert_eq!(
        preserved.subscriptions.get(&existing_session_id).copied(),
        Some(existing_cursor),
        "active-task upsert must not overwrite an existing live cursor",
    );
    assert!(preserved.pin_changes.attach.is_empty());
}

#[tokio::test]
async fn subscription_event_active_task_delete_retains_shared_and_explicit_sessions() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let removed_task_id = TaskId::new();
    let shared_task_id = TaskId::new();
    let explicit_task_id = TaskId::new();
    let explicit_session_id = SessionId::new();
    let mut subscription_state = WorkspaceActiveSubscriptionState::default();
    subscription_state.active_scope = true;
    subscription_state
        .active_task_sessions
        .insert(removed_task_id, session_id);
    subscription_state
        .active_task_sessions
        .insert(shared_task_id, session_id);
    subscription_state
        .active_task_sessions
        .insert(explicit_task_id, explicit_session_id);
    subscription_state
        .explicit_sessions
        .insert(explicit_session_id);
    let subscriptions = HashMap::from([
        (session_id, cursor(1, 1)),
        (explicit_session_id, cursor(2, 2)),
    ]);

    let shared_retained = apply_workspace_stream_subscription_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state.clone(),
        subscriptions.clone(),
        &WorkspaceActiveSnapshotEvent::ActiveTaskDelete {
            workspace_id,
            snapshot_rev: 1,
            task_id: removed_task_id,
        },
    )
    .await;
    assert!(shared_retained.subscriptions.contains_key(&session_id));
    assert!(shared_retained.pin_changes.detach.is_empty());

    let explicit_retained = apply_workspace_stream_subscription_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state,
        subscriptions,
        &WorkspaceActiveSnapshotEvent::ActiveTaskDelete {
            workspace_id,
            snapshot_rev: 2,
            task_id: explicit_task_id,
        },
    )
    .await;
    assert!(explicit_retained
        .subscriptions
        .contains_key(&explicit_session_id));
    assert!(explicit_retained.pin_changes.detach.is_empty());
}

#[tokio::test]
async fn subscription_event_archive_removes_unused_active_session() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = WorkspaceId::new();
    let task_id = TaskId::new();
    let session_id = SessionId::new();
    let mut archived_task = task(workspace_id, Some(session_id));
    archived_task.id = task_id;
    let mut subscription_state = WorkspaceActiveSubscriptionState::default();
    subscription_state.active_scope = true;
    subscription_state
        .active_task_sessions
        .insert(task_id, session_id);

    let applied = apply_workspace_stream_subscription_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state,
        HashMap::from([(session_id, cursor(4, 5))]),
        &WorkspaceActiveSnapshotEvent::TaskDelta {
            workspace_id,
            snapshot_rev: 1,
            delta: Box::new(TaskDelta {
                task: archived_task,
                kind: TaskDeltaKind::Archived,
            }),
        },
    )
    .await;

    assert!(applied.should_route);
    assert!(applied.state.active_task_sessions.is_empty());
    assert!(applied.subscriptions.is_empty());
    assert_eq!(applied.pin_changes.detach, vec![session_id]);
}

#[tokio::test]
async fn subscription_event_active_task_changes_noop_without_active_scope() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = WorkspaceId::new();
    let task_id = TaskId::new();
    let session_id = SessionId::new();
    let subscription_state = WorkspaceActiveSubscriptionState::default();

    let applied = apply_workspace_stream_subscription_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state,
        HashMap::new(),
        &WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
            workspace_id,
            snapshot_rev: 1,
            task: Box::new(active_task_summary(workspace_id, task_id, session_id)),
        },
    )
    .await;

    assert!(applied.should_route);
    assert!(applied.state.active_task_sessions.is_empty());
    assert!(applied.subscriptions.is_empty());
    assert!(applied.pin_changes.attach.is_empty());
}

#[tokio::test]
async fn live_event_accepts_head_delta_and_advances_cursor() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let mut subscription_state = WorkspaceActiveSubscriptionState::default();
    subscription_state.explicit_sessions.insert(session_id);

    let applied = apply_workspace_stream_live_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state,
        HashMap::from([(session_id, cursor(4, 6))]),
        WorkspaceActiveSnapshotEvent::SessionHeadDelta {
            workspace_id,
            snapshot_rev: 9,
            delta: Box::new(durable_delta(session_id, 5, 7)),
        },
    )
    .await;

    assert_eq!(
        applied.subscriptions.get(&session_id).copied(),
        Some(cursor(5, 7))
    );
    let WorkspaceStreamEventRoutePlan::HeadDelta {
        snapshot_rev,
        delta,
        lane,
    } = applied.route_plan
    else {
        panic!("expected accepted head delta route");
    };
    assert_eq!(snapshot_rev, 9);
    assert_eq!(delta.session_id, session_id);
    assert_eq!(lane, WorkspaceStreamHeadLane::Background);
}

#[tokio::test]
async fn live_event_routes_transient_head_delta_without_advancing_cursor() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let mut subscription_state = WorkspaceActiveSubscriptionState::default();
    subscription_state.foreground_session_ids = Some(HashSet::from([session_id]));
    let current = cursor(4, 6);

    let applied = apply_workspace_stream_live_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state,
        HashMap::from([(session_id, current)]),
        WorkspaceActiveSnapshotEvent::SessionHeadDelta {
            workspace_id,
            snapshot_rev: 10,
            delta: Box::new(partial_delta(session_id)),
        },
    )
    .await;

    assert_eq!(
        applied.subscriptions.get(&session_id).copied(),
        Some(current)
    );
    let WorkspaceStreamEventRoutePlan::HeadDelta { lane, delta, .. } = applied.route_plan else {
        panic!("expected transient head delta route");
    };
    assert_eq!(lane, WorkspaceStreamHeadLane::Foreground);
    assert!(delta.event.is_some());
}

#[tokio::test]
async fn live_event_drops_head_delta_without_cursor_even_when_state_allows_route() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let mut subscription_state = WorkspaceActiveSubscriptionState::default();
    subscription_state.explicit_sessions.insert(session_id);

    let applied = apply_workspace_stream_live_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state,
        HashMap::new(),
        WorkspaceActiveSnapshotEvent::SessionHeadDelta {
            workspace_id,
            snapshot_rev: 11,
            delta: Box::new(durable_delta(session_id, 6, 8)),
        },
    )
    .await;

    assert!(applied.subscriptions.is_empty());
    assert!(matches!(
        applied.route_plan,
        WorkspaceStreamEventRoutePlan::Drop
    ));
}

#[tokio::test]
async fn live_event_drops_stale_head_delta_and_head_seed_without_advancing_cursor() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let mut subscription_state = WorkspaceActiveSubscriptionState::default();
    subscription_state.foreground_session_ids = Some(HashSet::from([session_id]));
    let current = cursor(5, 7);

    let stale_delta = apply_workspace_stream_live_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state.clone(),
        HashMap::from([(session_id, current)]),
        WorkspaceActiveSnapshotEvent::SessionHeadDelta {
            workspace_id,
            snapshot_rev: 12,
            delta: Box::new(durable_delta(session_id, 5, 7)),
        },
    )
    .await;
    assert_eq!(
        stale_delta.subscriptions.get(&session_id).copied(),
        Some(current)
    );
    assert!(matches!(
        stale_delta.route_plan,
        WorkspaceStreamEventRoutePlan::Drop
    ));

    let stale_seed = apply_workspace_stream_live_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state,
        HashMap::from([(session_id, current)]),
        WorkspaceActiveSnapshotEvent::SessionHeadSeed {
            workspace_id,
            snapshot_rev: 13,
            head: Box::new(test_head(workspace_id, session_id, 5, 7)),
        },
    )
    .await;
    assert_eq!(
        stale_seed.subscriptions.get(&session_id).copied(),
        Some(current)
    );
    assert!(matches!(
        stale_seed.route_plan,
        WorkspaceStreamEventRoutePlan::Drop
    ));
}

#[tokio::test]
async fn live_event_active_task_upsert_seeds_subscription_and_pin_delta() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = WorkspaceId::new();
    let task_id = TaskId::new();
    let session_id = SessionId::new();
    let mut subscription_state = WorkspaceActiveSubscriptionState::default();
    subscription_state.active_scope = true;

    let applied = apply_workspace_stream_live_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state,
        HashMap::new(),
        WorkspaceActiveSnapshotEvent::ActiveTaskUpsert {
            workspace_id,
            snapshot_rev: 14,
            task: Box::new(active_task_summary(workspace_id, task_id, session_id)),
        },
    )
    .await;

    assert_eq!(
        applied.state.active_task_sessions.get(&task_id).copied(),
        Some(session_id)
    );
    assert!(applied.subscriptions.contains_key(&session_id));
    assert_eq!(applied.pin_changes.attach, vec![session_id]);
    assert!(matches!(
        applied.route_plan,
        WorkspaceStreamEventRoutePlan::Control { .. }
    ));
}

#[tokio::test]
async fn live_event_session_removed_detaches_and_later_head_event_drops() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let mut subscription_state = WorkspaceActiveSubscriptionState::default();
    subscription_state.explicit_sessions.insert(session_id);

    let removed = apply_workspace_stream_live_event(
        &workspace_stream_handle(&state),
        workspace_id,
        subscription_state,
        HashMap::from([(session_id, cursor(5, 7))]),
        WorkspaceActiveSnapshotEvent::SessionRemoved {
            workspace_id,
            snapshot_rev: 15,
            session_id,
        },
    )
    .await;

    assert!(removed.state.explicit_sessions.is_empty());
    assert!(removed.subscriptions.is_empty());
    assert_eq!(removed.pin_changes.detach, vec![session_id]);
    assert!(matches!(
        removed.route_plan,
        WorkspaceStreamEventRoutePlan::Control {
            session_id: Some(routed),
            ..
        } if routed == session_id
    ));

    let head_after_removal = apply_workspace_stream_live_event(
        &workspace_stream_handle(&state),
        workspace_id,
        removed.state,
        removed.subscriptions,
        WorkspaceActiveSnapshotEvent::SessionHeadDelta {
            workspace_id,
            snapshot_rev: 16,
            delta: Box::new(durable_delta(session_id, 6, 8)),
        },
    )
    .await;
    assert!(matches!(
        head_after_removal.route_plan,
        WorkspaceStreamEventRoutePlan::Drop
    ));
}

#[tokio::test]
async fn live_event_summary_delta_routes_without_cursor_mutation() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = WorkspaceId::new();
    let session_id = SessionId::new();
    let current = cursor(5, 7);

    let applied = apply_workspace_stream_live_event(
        &workspace_stream_handle(&state),
        workspace_id,
        WorkspaceActiveSubscriptionState::default(),
        HashMap::from([(session_id, current)]),
        summary_delta_event(workspace_id, session_id),
    )
    .await;

    assert_eq!(
        applied.subscriptions.get(&session_id).copied(),
        Some(current)
    );
    assert!(matches!(
        applied.route_plan,
        WorkspaceStreamEventRoutePlan::Summary { .. }
    ));
}

#[tokio::test]
async fn head_only_cursor_uses_snapshot_cursor_or_current_tail_fallback() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let (workspace_id, session_id) = create_workspace_session(&state, root.path()).await;

    let from_snapshot = head_only_snapshot_cursor(
        &workspace_stream_handle(&state),
        workspace_id,
        session_id,
        Some(cursor(4, 5)),
        Some(cursor(10, 3)),
        true,
    )
    .await;
    assert_eq!(from_snapshot, cursor(10, 5));

    let from_current_tail = head_only_snapshot_cursor(
        &workspace_stream_handle(&state),
        workspace_id,
        session_id,
        Some(cursor(4, 5)),
        Some(cursor(10, 3)),
        false,
    )
    .await;
    assert_eq!(
        from_current_tail,
        cursor(4, 5),
        "when no initial snapshot was queued, the daemon tail cursor is covered by the live cursor",
    );
}

#[tokio::test]
async fn active_task_subscription_cursor_reads_daemon_tail() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let (workspace_id, session_id) = create_workspace_session(&state, root.path()).await;

    let cursor =
        active_task_subscription_cursor(&workspace_stream_handle(&state), workspace_id, session_id)
            .await;

    assert_eq!(cursor, SessionReplayCursor::default());
}

#[tokio::test]
async fn handle_subscription_resolution_hydrates_active_snapshot_without_initial_heads() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let (workspace_id, session_id) = create_workspace_session(&state, root.path()).await;
    let handle = route_handles_from_state(&state).workspace_stream;

    handle
        .resolve_workspace_active_snapshot_subscriptions(
            workspace_id,
            WorkspaceActiveSnapshotClientMessage::Subscribe {
                session_ids: vec![session_id],
                sessions: Vec::new(),
                task_ids: Vec::new(),
                foreground_session_id: None,
                scope: None,
                include_active_heads: false,
            },
            &HashMap::new(),
        )
        .await
        .unwrap();

    let snapshot = state
        .workspaces
        .workspace_active_snapshot
        .active_snapshot(workspace_id, i64::MAX)
        .await;
    assert_eq!(
        snapshot.active.tasks.len(),
        1,
        "subscription resolution must prepare the daemon read model even without an initial snapshot",
    );
}

#[tokio::test]
async fn handle_subscription_resolution_activates_merge_queue() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let (workspace_id, session_id) = create_workspace_session(&state, root.path()).await;
    let store = state.store_for_workspace(workspace_id).await.unwrap();
    update_merge_queue_config(
        &store,
        MergeQueueConfigUpdate {
            enabled: true,
            target_branch: Some("main".to_string()),
            verify_commands: Vec::new(),
            push_on_success: None,
            push_remote: None,
            push_branch: None,
            canonical_sync: None,
        },
    )
    .await
    .unwrap();
    store
        .create_merge_queue_entry(&queued_merge_queue_entry(workspace_id))
        .await
        .unwrap();
    let mut schedule_rx = state
        .transport
        .merge_queue
        .take_schedule_rx()
        .await
        .expect("test runtime has not started the merge queue runner");
    let handle = workspace_stream_handle(&state);

    handle
        .resolve_workspace_active_snapshot_subscriptions(
            workspace_id,
            WorkspaceActiveSnapshotClientMessage::Subscribe {
                session_ids: vec![session_id],
                sessions: Vec::new(),
                task_ids: Vec::new(),
                foreground_session_id: None,
                scope: None,
                include_active_heads: false,
            },
            &HashMap::new(),
        )
        .await
        .unwrap();

    let scheduled = tokio::time::timeout(std::time::Duration::from_secs(1), schedule_rx.recv())
        .await
        .expect("workspace stream subscription should schedule active merge queue")
        .expect("merge queue schedule channel should stay open");
    assert_eq!(scheduled, workspace_id);
}

#[tokio::test]
async fn worktree_vcs_filter_removes_cross_workspace_duplicate_and_missing_ids() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let (workspace_a, worktree_a1) = create_workspace_worktree(&state, root.path()).await;
    let worktree_a2 = create_worktree_for_workspace(&state, root.path(), workspace_a).await;
    let (_workspace_b, worktree_b) = create_workspace_worktree(&state, root.path()).await;

    let handle = workspace_vcs_stream_handle(&state);
    let filtered = filter_workspace_worktree_ids(
        &handle,
        workspace_a,
        vec![
            worktree_b.id,
            worktree_a2.id,
            WorktreeId::new(),
            worktree_a1.id,
            worktree_a1.id,
        ],
    )
    .await;

    let mut expected = vec![worktree_a1.id, worktree_a2.id];
    expected.sort_by_key(|worktree_id| worktree_id.0);
    assert_eq!(filtered, expected);
}

#[tokio::test]
async fn workspace_vcs_subscription_plan_filters_dedupes_and_updates_demand() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let handle = workspace_vcs_stream_handle(&state);
    let (workspace_a, worktree_a1) = create_workspace_worktree(&state, root.path()).await;
    let worktree_a2 = create_worktree_for_workspace(&state, root.path(), workspace_a).await;
    let (_workspace_b, worktree_b) = create_workspace_worktree(&state, root.path()).await;

    let plan = plan_workspace_vcs_subscription_update(
        &handle,
        workspace_a,
        WorkspaceVcsDemandState::default(),
        vec![
            worktree_b.id,
            worktree_a2.id,
            worktree_a1.id,
            worktree_a1.id,
        ],
        vec![worktree_a2.id, worktree_b.id, worktree_a2.id],
    )
    .await;

    let mut expected_summary = vec![worktree_a1.id, worktree_a2.id];
    expected_summary.sort_by_key(|worktree_id| worktree_id.0);
    assert_eq!(plan.summary_subscribed_worktree_ids, expected_summary);
    assert_eq!(plan.detail_subscribed_worktree_ids, vec![worktree_a2.id]);
    assert_eq!(plan.state.demand_generation, 1);
    assert_eq!(
        plan.summary_seed_worktree_ids,
        HashSet::from([worktree_a1.id, worktree_a2.id])
    );
    assert_eq!(
        plan.detail_seed_worktree_ids,
        HashSet::from([worktree_a2.id])
    );
    let mut expected_seed_pairs = vec![
        (worktree_a1.id, WorktreeVcsStreamTier::Summary),
        (worktree_a2.id, WorktreeVcsStreamTier::Details),
    ];
    expected_seed_pairs.sort_by_key(|(worktree_id, _)| worktree_id.0);
    assert_eq!(vcs_seed_pairs(&plan.seed_plan), expected_seed_pairs);
    assert_eq!(plan.summary_refresh_worktree_ids, expected_summary);
    assert_eq!(plan.detail_refresh_worktree_ids, vec![worktree_a2.id]);
    assert!(
        handle.is_worktree_vcs_active_for_test(worktree_a1.id).await,
        "summary demand should mark worktree active",
    );
    assert!(
        handle.is_worktree_vcs_active_for_test(worktree_a2.id).await,
        "detail demand should mark worktree active",
    );
    assert!(
        handle
            .is_worktree_vcs_pane_open_for_test(worktree_a2.id)
            .await,
        "detail demand should mark pane open",
    );
    assert!(
        !handle.is_worktree_vcs_active_for_test(worktree_b.id).await,
        "foreign worktree must be filtered before activity mutation",
    );
}

#[tokio::test]
async fn workspace_vcs_subscription_plan_preserves_repeat_and_tier_transition_semantics() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let workspace_id = create_workspace_worktree(&state, root.path()).await.0;
    let worktree = create_worktree_for_workspace(&state, root.path(), workspace_id).await;

    let handle = workspace_vcs_stream_handle(&state);
    let initial = plan_workspace_vcs_subscription_update(
        &handle,
        workspace_id,
        WorkspaceVcsDemandState::default(),
        vec![worktree.id],
        Vec::new(),
    )
    .await;
    let repeat = plan_workspace_vcs_subscription_update(
        &handle,
        workspace_id,
        initial.state.clone(),
        vec![worktree.id],
        Vec::new(),
    )
    .await;
    assert_eq!(repeat.state.demand_generation, 2);
    assert!(repeat.summary_seed_worktree_ids.is_empty());
    assert!(repeat.detail_seed_worktree_ids.is_empty());
    assert!(repeat.seed_plan.seeds.is_empty());
    assert!(repeat.summary_refresh_worktree_ids.is_empty());
    assert!(repeat.detail_refresh_worktree_ids.is_empty());

    let upgrade = plan_workspace_vcs_subscription_update(
        &handle,
        workspace_id,
        repeat.state.clone(),
        vec![worktree.id],
        vec![worktree.id],
    )
    .await;
    assert_eq!(upgrade.state.demand_generation, 3);
    assert!(upgrade.summary_seed_worktree_ids.is_empty());
    assert_eq!(
        upgrade.detail_seed_worktree_ids,
        HashSet::from([worktree.id])
    );
    assert_eq!(
        vcs_seed_pairs(&upgrade.seed_plan),
        vec![(worktree.id, WorktreeVcsStreamTier::Details)]
    );
    assert!(upgrade.summary_refresh_worktree_ids.is_empty());
    assert_eq!(upgrade.detail_refresh_worktree_ids, vec![worktree.id]);

    let demotion = plan_workspace_vcs_subscription_update(
        &handle,
        workspace_id,
        upgrade.state.clone(),
        vec![worktree.id],
        Vec::new(),
    )
    .await;
    assert_eq!(demotion.state.demand_generation, 4);
    assert!(demotion.summary_seed_worktree_ids.is_empty());
    assert!(demotion.detail_seed_worktree_ids.is_empty());
    assert!(demotion.seed_plan.seeds.is_empty());
    assert!(demotion.summary_refresh_worktree_ids.is_empty());
    assert!(demotion.detail_refresh_worktree_ids.is_empty());
}

#[test]
fn workspace_vcs_snapshot_route_prefers_detail_over_summary_and_drops_unsubscribed() {
    let summary = WorktreeId::new();
    let detail = WorktreeId::new();
    let both = WorktreeId::new();
    let demand = WorkspaceVcsDemandState {
        demand_generation: 7,
        summary_worktree_ids: HashSet::from([summary, both]),
        detail_worktree_ids: HashSet::from([detail, both]),
    };

    assert_eq!(
        route_workspace_vcs_snapshot(&demand, summary),
        WorkspaceVcsSnapshotRoute::Summary,
    );
    assert_eq!(
        route_workspace_vcs_snapshot(&demand, detail),
        WorkspaceVcsSnapshotRoute::Details,
    );
    assert_eq!(
        route_workspace_vcs_snapshot(&demand, both),
        WorkspaceVcsSnapshotRoute::Details,
    );
    assert_eq!(
        route_workspace_vcs_snapshot(&demand, WorktreeId::new()),
        WorkspaceVcsSnapshotRoute::Drop,
    );
}

#[test]
fn workspace_vcs_lag_reseed_sorts_and_prefers_detail_tier() {
    let summary = WorktreeId::new();
    let detail = WorktreeId::new();
    let both = WorktreeId::new();
    let demand = WorkspaceVcsDemandState {
        demand_generation: 9,
        summary_worktree_ids: HashSet::from([summary, both]),
        detail_worktree_ids: HashSet::from([detail, both]),
    };
    let mut expected = vec![
        (summary, WorktreeVcsStreamTier::Summary),
        (detail, WorktreeVcsStreamTier::Details),
        (both, WorktreeVcsStreamTier::Details),
    ];
    expected.sort_by_key(|(worktree_id, _)| worktree_id.0);

    assert_eq!(
        vcs_seed_pairs(&plan_workspace_vcs_lag_reseed(&demand)),
        expected
    );
}

#[tokio::test]
async fn workspace_vcs_refresh_plan_filters_workspace_and_respects_tier() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let (workspace_a, worktree_a1) = create_workspace_worktree(&state, root.path()).await;
    let worktree_a2 = create_worktree_for_workspace(&state, root.path(), workspace_a).await;
    let (_workspace_b, worktree_b) = create_workspace_worktree(&state, root.path()).await;

    let handle = workspace_vcs_stream_handle(&state);
    let summary = plan_workspace_vcs_refresh(
        &handle,
        workspace_a,
        vec![
            worktree_b.id,
            worktree_a2.id,
            worktree_a1.id,
            worktree_a1.id,
        ],
        WorktreeVcsStreamTier::Summary,
    )
    .await;
    let mut expected = vec![worktree_a1.id, worktree_a2.id];
    expected.sort_by_key(|worktree_id| worktree_id.0);
    assert_eq!(summary.summary_refresh_worktree_ids, expected);
    assert!(summary.detail_refresh_worktree_ids.is_empty());

    let details = plan_workspace_vcs_refresh(
        &handle,
        workspace_a,
        vec![worktree_b.id, worktree_a2.id],
        WorktreeVcsStreamTier::Details,
    )
    .await;
    assert!(details.summary_refresh_worktree_ids.is_empty());
    assert_eq!(details.detail_refresh_worktree_ids, vec![worktree_a2.id]);
}

#[tokio::test]
async fn workspace_vcs_refresh_suppresses_fresh_snapshots_and_invokes_effect_for_stale_details() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let (workspace_id, fresh_summary) = create_workspace_worktree(&state, root.path()).await;
    let fresh_detail = create_worktree_for_workspace(&state, root.path(), workspace_id).await;
    let stale_detail = create_worktree_for_workspace(&state, root.path(), workspace_id).await;
    let (_foreign_workspace, foreign) = create_workspace_worktree(&state, root.path()).await;

    let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let refresh_effect = Arc::new({
        let calls = Arc::clone(&calls);
        move |worktree: Worktree, summary: bool, details: bool| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.lock().await.push((worktree.id, summary, details));
                Ok(())
            })
                as crate::daemon::workspace_stream_route_handles::WorkspaceVcsStreamRefreshFuture
        }
    })
        as crate::daemon::workspace_stream_route_handles::WorkspaceVcsStreamRefreshEffect;
    let handle = workspace_vcs_stream_with_refresh_effect_from_state(&state, refresh_effect);

    handle
        .runtime()
        .cache_worktree_vcs_snapshot_for_test(test_vcs_snapshot(
            fresh_summary.id,
            WorktreeVcsFreshness::Fresh,
            WorktreeVcsTouchedFilesState::NotLoaded,
        ))
        .await;
    handle
        .runtime()
        .cache_worktree_vcs_snapshot_for_test(test_vcs_snapshot(
            fresh_detail.id,
            WorktreeVcsFreshness::Fresh,
            WorktreeVcsTouchedFilesState::Ready,
        ))
        .await;
    handle
        .runtime()
        .cache_worktree_vcs_snapshot_for_test(test_vcs_snapshot(
            stale_detail.id,
            WorktreeVcsFreshness::Stale,
            WorktreeVcsTouchedFilesState::NotLoaded,
        ))
        .await;

    let summary_plan = plan_workspace_vcs_refresh(
        &handle,
        workspace_id,
        vec![foreign.id, fresh_summary.id, fresh_summary.id],
        WorktreeVcsStreamTier::Summary,
    )
    .await;
    refresh_worktree_vcs_for_worktrees(
        &handle,
        &summary_plan.summary_refresh_worktree_ids,
        &summary_plan.detail_refresh_worktree_ids,
    )
    .await;

    let detail_plan = plan_workspace_vcs_refresh(
        &handle,
        workspace_id,
        vec![fresh_detail.id, stale_detail.id],
        WorktreeVcsStreamTier::Details,
    )
    .await;
    refresh_worktree_vcs_for_worktrees(
        &handle,
        &detail_plan.summary_refresh_worktree_ids,
        &detail_plan.detail_refresh_worktree_ids,
    )
    .await;

    assert_eq!(
        *calls.lock().await,
        vec![(stale_detail.id, true, true)],
        "refresh effect should only receive loaded in-workspace worktrees that need refresh",
    );
}

#[tokio::test]
async fn workspace_vcs_release_clears_final_demand_state() {
    let root = tempfile::tempdir().unwrap();
    let state = test_state(root.path()).await;
    let handle = workspace_vcs_stream_handle(&state);
    let workspace_id = create_workspace_worktree(&state, root.path()).await.0;
    let summary = create_worktree_for_workspace(&state, root.path(), workspace_id).await;
    let detail = create_worktree_for_workspace(&state, root.path(), workspace_id).await;
    let plan = plan_workspace_vcs_subscription_update(
        &handle,
        workspace_id,
        WorkspaceVcsDemandState::default(),
        vec![summary.id],
        vec![detail.id],
    )
    .await;
    assert!(handle.is_worktree_vcs_active_for_test(summary.id).await);
    assert!(handle.is_worktree_vcs_active_for_test(detail.id).await);
    assert!(handle.is_worktree_vcs_pane_open_for_test(detail.id).await);

    release_workspace_vcs_demand(&handle, &plan.state).await;

    assert!(!handle.is_worktree_vcs_active_for_test(summary.id).await);
    assert!(!handle.is_worktree_vcs_active_for_test(detail.id).await);
    assert!(!handle.is_worktree_vcs_pane_open_for_test(detail.id).await);
}
