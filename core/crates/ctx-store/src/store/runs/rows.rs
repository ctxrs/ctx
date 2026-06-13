use super::*;

pub(super) struct SequencedAuditEvent {
    pub(super) ingest_seq: i64,
    pub(super) event: AuditEvent,
}

pub(super) fn build_run_archive_ingest_cursor_from_row(
    row: SqliteRow,
) -> Result<RunArchiveIngestCursor> {
    let run_id: String = row.try_get("run_id")?;
    let workspace_id: String = row.try_get("workspace_id")?;
    let org_id: Option<String> = row.try_get("org_id")?;
    let archive_visibility: String = row.try_get("archive_visibility")?;
    let retention_policy_key: Option<String> = row.try_get("retention_policy_key")?;
    let retention_legal_hold_key: Option<String> = row.try_get("retention_legal_hold_key")?;
    let last_session_event_seq: i64 = row.try_get("last_session_event_seq")?;
    let last_audit_event_seq: i64 = row.try_get("last_audit_event_seq")?;
    let last_batch_id: Option<String> = row.try_get("last_batch_id")?;
    let last_synced_at: Option<String> = row.try_get("last_synced_at")?;
    let updated_at: String = row.try_get("updated_at")?;

    Ok(RunArchiveIngestCursor {
        run_id: parse_uuid_id(run_id, "run_archive_ingest_cursors.run_id", RunId)?,
        workspace_id: parse_uuid_id(
            workspace_id,
            "run_archive_ingest_cursors.workspace_id",
            WorkspaceId,
        )?,
        org_id: parse_opt_uuid_id(org_id, "run_archive_ingest_cursors.org_id", OrgId)?,
        archive_visibility: ArchiveVisibility::parse(&archive_visibility).with_context(|| {
            format!("invalid run_archive_ingest_cursors.archive_visibility: {archive_visibility}")
        })?,
        retention_policy: retention_policy_key.map(|policy_key| RetentionPolicyRef {
            policy_key,
            legal_hold_key: retention_legal_hold_key,
        }),
        watermark: RunArchiveIngestWatermark {
            session_event_seq: last_session_event_seq,
            audit_event_seq: last_audit_event_seq,
        },
        last_batch_id,
        last_synced_at: last_synced_at.as_deref().map(parse_dt).transpose()?,
        updated_at: parse_dt(&updated_at)?,
    })
}

pub(super) fn build_session_event_from_row(row: SqliteRow) -> Result<SessionEvent> {
    let id: String = row.try_get("id")?;
    let session_id: String = row.try_get("session_id")?;
    let run_id: Option<String> = row.try_get("run_id")?;
    let turn_id: Option<String> = row.try_get("turn_id")?;
    let event_type: String = row.try_get("event_type")?;
    let payload_json: String = row.try_get("payload_json")?;
    let transient: i64 = row.try_get("transient")?;
    let created_at: String = row.try_get("created_at")?;

    Ok(SessionEvent {
        seq: row.try_get("seq")?,
        id: parse_uuid_id(id, "session_events.id", SessionEventId)?,
        session_id: parse_uuid_id(session_id, "session_events.session_id", SessionId)?,
        run_id: parse_opt_uuid_id(run_id, "session_events.run_id", RunId)?,
        turn_id: parse_opt_uuid_id(turn_id, "session_events.turn_id", TurnId)?,
        event_type: parse_session_event_type(&event_type),
        payload_json: serde_json::from_str(&payload_json)
            .with_context(|| "failed to parse session_events.payload_json".to_string())?,
        transient: transient != 0,
        created_at: parse_dt(&created_at)?,
    })
}

pub(super) fn build_message_from_row(row: SqliteRow) -> Result<Message> {
    let id: String = row.try_get("id")?;
    let session_id: String = row.try_get("session_id")?;
    let task_id: String = row.try_get("task_id")?;
    let run_id: Option<String> = row.try_get("run_id")?;
    let turn_id: Option<String> = row.try_get("turn_id")?;
    let turn_sequence: Option<i64> = row.try_get("turn_sequence")?;
    let order_seq: Option<i64> = row.try_get("order_seq")?;
    let role: String = row.try_get("role")?;
    let content: String = row.try_get("content")?;
    let attachments_json: Option<String> = row.try_get("attachments_json")?;
    let delivery: String = row.try_get("delivery")?;
    let delivered_at: Option<String> = row.try_get("delivered_at")?;
    let created_at: String = row.try_get("created_at")?;
    let attachments = attachments_json
        .as_deref()
        .and_then(|value| serde_json::from_str::<Vec<MessageAttachment>>(value).ok())
        .unwrap_or_default();

    Ok(Message {
        id: parse_uuid_id(id, "messages.id", MessageId)?,
        session_id: parse_uuid_id(session_id, "messages.session_id", SessionId)?,
        task_id: parse_uuid_id(task_id, "messages.task_id", TaskId)?,
        run_id: parse_opt_uuid_id(run_id, "messages.run_id", RunId)?,
        turn_id: parse_opt_uuid_id(turn_id, "messages.turn_id", TurnId)?,
        turn_sequence,
        order_seq,
        role: parse_message_role(&role),
        content,
        attachments,
        delivery: parse_message_delivery(&delivery),
        delivered_at: delivered_at.as_deref().map(parse_dt).transpose()?,
        created_at: parse_dt(&created_at)?,
    })
}

pub(super) fn build_sequenced_audit_event_from_row(row: SqliteRow) -> Result<SequencedAuditEvent> {
    let ingest_seq: i64 = row.try_get("ingest_seq")?;
    Ok(SequencedAuditEvent {
        ingest_seq,
        event: build_audit_event_from_row(row)?,
    })
}

pub(super) fn build_run_record_from_row(row: SqliteRow) -> Result<RunRecord> {
    let id: String = row.try_get("id")?;
    let session_id: String = row.try_get("session_id")?;
    let task_id: String = row.try_get("task_id")?;
    let workspace_id: String = row.try_get("workspace_id")?;
    let worktree_id: String = row.try_get("worktree_id")?;
    let parent_run_id: Option<String> = row.try_get("parent_run_id")?;
    let account_id: Option<String> = row.try_get("account_id")?;
    let org_id: Option<String> = row.try_get("org_id")?;
    let run_grant_id: Option<String> = row.try_get("run_grant_id")?;
    let status: String = row.try_get("status")?;
    let archive_state: String = row.try_get("archive_state")?;
    let archive_visibility: String = row.try_get("archive_visibility")?;
    let retention_policy_key: Option<String> = row.try_get("retention_policy_key")?;
    let retention_legal_hold_key: Option<String> = row.try_get("retention_legal_hold_key")?;
    let created_at: String = row.try_get("created_at")?;
    let started_at: Option<String> = row.try_get("started_at")?;
    let completed_at: Option<String> = row.try_get("completed_at")?;
    let archived_at: Option<String> = row.try_get("archived_at")?;
    let updated_at: String = row.try_get("updated_at")?;

    Ok(RunRecord {
        id: parse_uuid_id(id, "runs.id", RunId)?,
        session_id: parse_uuid_id(session_id, "runs.session_id", SessionId)?,
        task_id: parse_uuid_id(task_id, "runs.task_id", TaskId)?,
        workspace_id: parse_uuid_id(workspace_id, "runs.workspace_id", WorkspaceId)?,
        worktree_id: parse_uuid_id(worktree_id, "runs.worktree_id", WorktreeId)?,
        parent_run_id: parse_opt_uuid_id(parent_run_id, "runs.parent_run_id", RunId)?,
        account_id: parse_opt_uuid_id(account_id, "runs.account_id", AccountId)?,
        org_id: parse_opt_uuid_id(org_id, "runs.org_id", OrgId)?,
        run_grant_id: parse_opt_uuid_id(run_grant_id, "runs.run_grant_id", RunGrantId)?,
        status: RunStatus::parse(&status)
            .with_context(|| format!("invalid runs.status value: {status}"))?,
        archive_state: RunArchiveState::parse(&archive_state)
            .with_context(|| format!("invalid runs.archive_state value: {archive_state}"))?,
        archive_visibility: ArchiveVisibility::parse(&archive_visibility).with_context(|| {
            format!("invalid runs.archive_visibility value: {archive_visibility}")
        })?,
        retention_policy: retention_policy_key.map(|policy_key| RetentionPolicyRef {
            policy_key,
            legal_hold_key: retention_legal_hold_key,
        }),
        created_at: parse_dt(&created_at)?,
        started_at: started_at.as_deref().map(parse_dt).transpose()?,
        completed_at: completed_at.as_deref().map(parse_dt).transpose()?,
        archived_at: archived_at.as_deref().map(parse_dt).transpose()?,
        updated_at: parse_dt(&updated_at)?,
    })
}

pub(super) fn build_audit_event_from_row(row: SqliteRow) -> Result<AuditEvent> {
    let id: String = row.try_get("id")?;
    let workspace_id: String = row.try_get("workspace_id")?;
    let task_id: Option<String> = row.try_get("task_id")?;
    let session_id: Option<String> = row.try_get("session_id")?;
    let run_id: Option<String> = row.try_get("run_id")?;
    let account_id: Option<String> = row.try_get("account_id")?;
    let org_id: Option<String> = row.try_get("org_id")?;
    let actor_kind: String = row.try_get("actor_kind")?;
    let actor_account_id: Option<String> = row.try_get("actor_account_id")?;
    let actor_org_id: Option<String> = row.try_get("actor_org_id")?;
    let actor_membership_role: Option<String> = row.try_get("actor_membership_role")?;
    let event_kind: String = row.try_get("event_kind")?;
    let archive_visibility: Option<String> = row.try_get("archive_visibility")?;
    let retention_policy_key: Option<String> = row.try_get("retention_policy_key")?;
    let retention_legal_hold_key: Option<String> = row.try_get("retention_legal_hold_key")?;
    let payload_json: String = row.try_get("payload_json")?;
    let created_at: String = row.try_get("created_at")?;

    Ok(AuditEvent {
        id,
        workspace_id: parse_uuid_id(workspace_id, "run_audit_events.workspace_id", WorkspaceId)?,
        task_id: parse_opt_uuid_id(task_id, "run_audit_events.task_id", TaskId)?,
        session_id: parse_opt_uuid_id(session_id, "run_audit_events.session_id", SessionId)?,
        run_id: parse_opt_uuid_id(run_id, "run_audit_events.run_id", RunId)?,
        account_id: parse_opt_uuid_id(account_id, "run_audit_events.account_id", AccountId)?,
        org_id: parse_opt_uuid_id(org_id, "run_audit_events.org_id", OrgId)?,
        actor: AuditActor {
            kind: AuditActorKind::parse(&actor_kind)
                .with_context(|| format!("invalid run_audit_events.actor_kind: {actor_kind}"))?,
            account_id: parse_opt_uuid_id(
                actor_account_id,
                "run_audit_events.actor_account_id",
                AccountId,
            )?,
            org_id: parse_opt_uuid_id(actor_org_id, "run_audit_events.actor_org_id", OrgId)?,
            membership_role: actor_membership_role,
        },
        event_kind: AuditEventKind::parse(&event_kind)
            .with_context(|| format!("invalid run_audit_events.event_kind: {event_kind}"))?,
        archive_visibility: archive_visibility
            .map(|value| {
                ArchiveVisibility::parse(&value).with_context(|| {
                    format!("invalid run_audit_events.archive_visibility: {value}")
                })
            })
            .transpose()?,
        retention_policy: retention_policy_key.map(|policy_key| RetentionPolicyRef {
            policy_key,
            legal_hold_key: retention_legal_hold_key,
        }),
        payload_json: serde_json::from_str(&payload_json)
            .with_context(|| "failed to parse run_audit_events.payload_json".to_string())?,
        created_at: parse_dt(&created_at)?,
    })
}

fn parse_uuid_id<T>(value: String, field_name: &str, wrap: fn(uuid::Uuid) -> T) -> Result<T> {
    Ok(wrap(uuid::Uuid::parse_str(&value).with_context(|| {
        format!("invalid UUID in {field_name}: {value}")
    })?))
}

fn parse_opt_uuid_id<T>(
    value: Option<String>,
    field_name: &str,
    wrap: fn(uuid::Uuid) -> T,
) -> Result<Option<T>> {
    value
        .map(|value| parse_uuid_id(value, field_name, wrap))
        .transpose()
}
