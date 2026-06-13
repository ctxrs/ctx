pub(super) fn parse_dt(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

pub(super) fn task_status_to_str(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

pub(super) fn parse_task_status(value: &str) -> TaskStatus {
    match value {
        "pending" => TaskStatus::Pending,
        "running" => TaskStatus::Running,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        _ => TaskStatus::Pending,
    }
}

pub(super) fn execution_environment_to_str(
    execution_environment: ExecutionEnvironment,
) -> &'static str {
    execution_environment.as_str()
}

pub(super) fn parse_execution_environment(value: &str) -> Result<ExecutionEnvironment> {
    match value {
        "host" => Ok(ExecutionEnvironment::Host),
        "sandbox" => Ok(ExecutionEnvironment::Sandbox),
        "container_host_mounted" | "container_disk_isolated" => Ok(ExecutionEnvironment::Sandbox),
        _ => anyhow::bail!("invalid persisted execution_environment: {value:?}"),
    }
}

pub(super) fn session_status_to_str(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Active => "active",
        SessionStatus::Completed => "completed",
        SessionStatus::Failed => "failed",
        SessionStatus::Cancelled => "cancelled",
    }
}

pub(super) fn parse_session_status(value: &str) -> SessionStatus {
    match value {
        "active" => SessionStatus::Active,
        "completed" => SessionStatus::Completed,
        "failed" => SessionStatus::Failed,
        "cancelled" => SessionStatus::Cancelled,
        _ => SessionStatus::Active,
    }
}

pub(super) fn merge_queue_entry_status_to_str(status: &MergeQueueEntryStatus) -> &'static str {
    match status {
        MergeQueueEntryStatus::Queued => "queued",
        MergeQueueEntryStatus::Running => "running",
        MergeQueueEntryStatus::Passed => "passed",
        MergeQueueEntryStatus::Failed => "failed",
        MergeQueueEntryStatus::Conflict => "conflict",
        MergeQueueEntryStatus::Cancelled => "cancelled",
    }
}

pub(super) fn parse_merge_queue_entry_status(value: &str) -> MergeQueueEntryStatus {
    match value {
        "queued" => MergeQueueEntryStatus::Queued,
        "running" => MergeQueueEntryStatus::Running,
        "passed" => MergeQueueEntryStatus::Passed,
        "failed" => MergeQueueEntryStatus::Failed,
        "conflict" => MergeQueueEntryStatus::Conflict,
        "cancelled" => MergeQueueEntryStatus::Cancelled,
        _ => MergeQueueEntryStatus::Queued,
    }
}

pub(super) fn merge_queue_run_status_to_str(status: &MergeQueueRunStatus) -> &'static str {
    match status {
        MergeQueueRunStatus::Running => "running",
        MergeQueueRunStatus::Passed => "passed",
        MergeQueueRunStatus::Failed => "failed",
        MergeQueueRunStatus::Conflict => "conflict",
        MergeQueueRunStatus::Cancelled => "cancelled",
    }
}

pub(super) fn parse_merge_queue_run_status(value: &str) -> MergeQueueRunStatus {
    match value {
        "running" => MergeQueueRunStatus::Running,
        "passed" => MergeQueueRunStatus::Passed,
        "failed" => MergeQueueRunStatus::Failed,
        "conflict" => MergeQueueRunStatus::Conflict,
        "cancelled" => MergeQueueRunStatus::Cancelled,
        _ => MergeQueueRunStatus::Running,
    }
}

pub(super) fn merge_queue_patch_source_to_str(source: &MergeQueuePatchSource) -> &'static str {
    match source {
        MergeQueuePatchSource::Generated => "generated",
        MergeQueuePatchSource::Provided => "provided",
    }
}

pub(super) fn parse_merge_queue_patch_source(value: &str) -> MergeQueuePatchSource {
    match value {
        "provided" => MergeQueuePatchSource::Provided,
        "generated" => MergeQueuePatchSource::Generated,
        _ => MergeQueuePatchSource::Generated,
    }
}

pub(super) fn message_role_to_str(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
    }
}

pub(super) fn parse_message_role(value: &str) -> MessageRole {
    match value {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "system" => MessageRole::System,
        _ => MessageRole::User,
    }
}

pub(super) fn attachment_kind_to_str(kind: &WorkspaceAttachmentKind) -> &'static str {
    match kind {
        WorkspaceAttachmentKind::ReferenceRepo => "reference_repo",
        WorkspaceAttachmentKind::DocMirror => "doc_mirror",
    }
}

pub(super) fn parse_attachment_kind(value: &str) -> WorkspaceAttachmentKind {
    match value {
        "reference_repo" => WorkspaceAttachmentKind::ReferenceRepo,
        "doc_mirror" => WorkspaceAttachmentKind::DocMirror,
        _ => WorkspaceAttachmentKind::ReferenceRepo,
    }
}

pub(super) fn attachment_mode_to_str(mode: &AttachmentMode) -> &'static str {
    match mode {
        AttachmentMode::Ro => "ro",
        AttachmentMode::Rw => "rw",
    }
}

pub(super) fn parse_attachment_mode(value: &str) -> AttachmentMode {
    match value {
        "rw" => AttachmentMode::Rw,
        "ro" => AttachmentMode::Ro,
        _ => AttachmentMode::Ro,
    }
}

pub(super) fn attachment_update_policy_to_str(policy: &AttachmentUpdatePolicy) -> &'static str {
    match policy {
        AttachmentUpdatePolicy::Manual => "manual",
        AttachmentUpdatePolicy::OnOpen => "on_open",
        AttachmentUpdatePolicy::Scheduled => "scheduled",
    }
}

pub(super) fn parse_attachment_update_policy(value: &str) -> AttachmentUpdatePolicy {
    match value {
        "on_open" => AttachmentUpdatePolicy::OnOpen,
        "scheduled" => AttachmentUpdatePolicy::Scheduled,
        "manual" => AttachmentUpdatePolicy::Manual,
        _ => AttachmentUpdatePolicy::Manual,
    }
}

pub(super) fn workspace_attachment_status_to_str(
    status: &WorkspaceAttachmentStatus,
) -> &'static str {
    match status {
        WorkspaceAttachmentStatus::Pending => "pending",
        WorkspaceAttachmentStatus::Syncing => "syncing",
        WorkspaceAttachmentStatus::Ready => "ready",
        WorkspaceAttachmentStatus::Error => "error",
    }
}

pub(super) fn parse_workspace_attachment_status(value: &str) -> WorkspaceAttachmentStatus {
    match value {
        "pending" => WorkspaceAttachmentStatus::Pending,
        "syncing" => WorkspaceAttachmentStatus::Syncing,
        "ready" => WorkspaceAttachmentStatus::Ready,
        "error" => WorkspaceAttachmentStatus::Error,
        _ => WorkspaceAttachmentStatus::Ready,
    }
}

pub(super) fn worktree_attachment_status_to_str(status: &WorktreeAttachmentStatus) -> &'static str {
    match status {
        WorktreeAttachmentStatus::Ready => "ready",
        WorktreeAttachmentStatus::Stale => "stale",
        WorktreeAttachmentStatus::Error => "error",
    }
}

pub(super) fn parse_worktree_attachment_status(value: &str) -> WorktreeAttachmentStatus {
    match value {
        "ready" => WorktreeAttachmentStatus::Ready,
        "stale" => WorktreeAttachmentStatus::Stale,
        "error" => WorktreeAttachmentStatus::Error,
        _ => WorktreeAttachmentStatus::Error,
    }
}

pub(super) fn message_delivery_to_str(delivery: &MessageDelivery) -> &'static str {
    match delivery {
        MessageDelivery::Immediate => "immediate",
        MessageDelivery::Queued => "queued",
    }
}

pub(super) fn parse_message_delivery(value: &str) -> MessageDelivery {
    match value {
        "immediate" => MessageDelivery::Immediate,
        "queued" => MessageDelivery::Queued,
        _ => MessageDelivery::Queued,
    }
}

pub(super) fn session_turn_status_to_str(status: &SessionTurnStatus) -> &'static str {
    match status {
        SessionTurnStatus::Queued => "queued",
        SessionTurnStatus::Starting => "starting",
        SessionTurnStatus::Running => "running",
        SessionTurnStatus::Completed => "completed",
        SessionTurnStatus::Interrupted => "interrupted",
        SessionTurnStatus::Failed => "failed",
    }
}

pub(super) fn parse_session_turn_status(value: &str) -> SessionTurnStatus {
    match value {
        "queued" => SessionTurnStatus::Queued,
        "starting" => SessionTurnStatus::Starting,
        "running" => SessionTurnStatus::Running,
        "completed" => SessionTurnStatus::Completed,
        "interrupted" => SessionTurnStatus::Interrupted,
        "failed" => SessionTurnStatus::Failed,
        _ => SessionTurnStatus::Running,
    }
}

pub(super) fn session_event_type_to_str(event_type: &SessionEventType) -> &'static str {
    match event_type {
        SessionEventType::Init => "init",
        SessionEventType::UserMessage => "user_message",
        SessionEventType::InputQueued => "input_queued",
        SessionEventType::TurnQueued => "turn_queued",
        SessionEventType::TurnStarted => "turn_started",
        SessionEventType::ContextWindowUpdate => "context_window_update",
        SessionEventType::TurnFinished => "turn_finished",
        SessionEventType::AuthRequired => "auth_required",
        SessionEventType::Notice => "notice",
        SessionEventType::AssistantChunk => "assistant_chunk",
        SessionEventType::ThoughtChunk => "thought_chunk",
        SessionEventType::AssistantComplete => "assistant_complete",
        SessionEventType::AssistantMessageInserted => "assistant_message_inserted",
        SessionEventType::ToolCall => "tool_call",
        SessionEventType::ToolCallUpdate => "tool_call_update",
        SessionEventType::ToolResult => "tool_result",
        SessionEventType::Plan => "plan",
        SessionEventType::ArtifactsSet => "artifacts_set",
        SessionEventType::Done => "done",
        SessionEventType::InterruptRequested => "interrupt_requested",
        SessionEventType::TurnInterrupted => "turn_interrupted",
        SessionEventType::MessageQueueAdded => "message_queue_added",
        SessionEventType::MessageQueueUpdated => "message_queue_updated",
        SessionEventType::MessageQueueRemoved => "message_queue_removed",
        SessionEventType::MessageQueuePromoted => "message_queue_promoted",
        SessionEventType::Error => "error",
    }
}

pub(super) fn parse_session_event_type(value: &str) -> SessionEventType {
    match value {
        "init" => SessionEventType::Init,
        "user_message" => SessionEventType::UserMessage,
        "input_queued" => SessionEventType::InputQueued,
        "turn_queued" => SessionEventType::TurnQueued,
        "turn_started" => SessionEventType::TurnStarted,
        "context_window_update" => SessionEventType::ContextWindowUpdate,
        "turn_finished" => SessionEventType::TurnFinished,
        "auth_required" => SessionEventType::AuthRequired,
        "notice" => SessionEventType::Notice,
        "assistant_chunk" => SessionEventType::AssistantChunk,
        "thought_chunk" => SessionEventType::ThoughtChunk,
        "assistant_complete" => SessionEventType::AssistantComplete,
        "assistant_message_inserted" => SessionEventType::AssistantMessageInserted,
        "tool_call" => SessionEventType::ToolCall,
        "tool_call_update" => SessionEventType::ToolCallUpdate,
        "tool_result" => SessionEventType::ToolResult,
        "plan" => SessionEventType::Plan,
        "artifacts_set" => SessionEventType::ArtifactsSet,
        "done" => SessionEventType::Done,
        "interrupt_requested" => SessionEventType::InterruptRequested,
        "turn_interrupted" => SessionEventType::TurnInterrupted,
        "message_queue_added" => SessionEventType::MessageQueueAdded,
        "message_queue_updated" => SessionEventType::MessageQueueUpdated,
        "message_queue_removed" => SessionEventType::MessageQueueRemoved,
        "message_queue_promoted" => SessionEventType::MessageQueuePromoted,
        "error" => SessionEventType::Error,
        _ => SessionEventType::Notice,
    }
}

pub(super) fn is_transient_session_event(
    event_type: &SessionEventType,
    payload_json: &serde_json::Value,
) -> bool {
    if payload_json
        .get("crp_channel")
        .and_then(|v| v.as_str())
        .is_some_and(|v| v == "data")
    {
        return true;
    }
    if payload_json
        .get("crpChannel")
        .and_then(|v| v.as_str())
        .is_some_and(|v| v == "data")
    {
        return true;
    }
    if matches!(event_type, SessionEventType::ToolCallUpdate) {
        return true;
    }
    if matches!(event_type, SessionEventType::AssistantComplete) {
        return true;
    }
    if matches!(event_type, SessionEventType::ContextWindowUpdate) {
        return true;
    }
    if matches!(event_type, SessionEventType::AuthRequired) {
        return true;
    }
    if !matches!(event_type, SessionEventType::Notice) {
        return false;
    }

    if let Some(kind) = payload_json.get("kind").and_then(|v| v.as_str()) {
        if matches!(
            kind,
            "reasoning_summary"
                | "provider_guard_warning"
                | "provider_guard_kill"
                | "title_generated"
                | "git_status_snapshot"
                | "auth_started"
                | "auth_finished"
                | "auth_failed"
                | "auth_required"
        ) {
            return true;
        }
    }

    false
}
