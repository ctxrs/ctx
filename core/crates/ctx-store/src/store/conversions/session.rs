pub(super) fn parse_optional_session_id(raw: Option<String>) -> Option<SessionId> {
    raw.and_then(|value| uuid::Uuid::parse_str(&value).ok())
        .map(SessionId)
}

pub(super) fn vcs_kind_to_str(kind: &VcsKind) -> &'static str {
    match kind {
        VcsKind::Git => "git",
        VcsKind::Jj => "jj",
        VcsKind::Hg => "hg",
        VcsKind::Svn => "svn",
        VcsKind::P4 => "p4",
        VcsKind::Other => "other",
    }
}

pub(super) fn parse_vcs_kind(raw: Option<String>) -> Option<VcsKind> {
    match raw.as_deref() {
        Some("git") => Some(VcsKind::Git),
        Some("jj") => Some(VcsKind::Jj),
        Some("hg") => Some(VcsKind::Hg),
        Some("svn") => Some(VcsKind::Svn),
        Some("p4") => Some(VcsKind::P4),
        Some("other") => Some(VcsKind::Other),
        Some(_) => Some(VcsKind::Other),
        None => None,
    }
}

pub(super) fn decode_session_row(row: &SqliteRow) -> Result<Session> {
    let id: String = row.try_get("id")?;
    let task_id: String = row.try_get("task_id")?;
    let ws_id: String = row.try_get("workspace_id")?;
    let wt_id: String = row.try_get("worktree_id")?;
    let created_at: String = row.try_get("created_at")?;
    let updated_at: String = row.try_get("updated_at")?;
    Ok(Session {
        id: SessionId(uuid::Uuid::parse_str(&id)?),
        task_id: TaskId(uuid::Uuid::parse_str(&task_id)?),
        workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id)?),
        worktree_id: WorktreeId(uuid::Uuid::parse_str(&wt_id)?),
        execution_environment: parse_execution_environment(
            row.try_get::<String, _>("execution_environment")?.as_str(),
        )?,
        parent_session_id: parse_optional_session_id(row.try_get("parent_session_id")?),
        relationship: row.try_get("relationship")?,
        provider_id: row.try_get("provider_id")?,
        model_id: row.try_get("model_id")?,
        reasoning_effort: row.try_get("reasoning_effort")?,
        title: row.try_get("title")?,
        agent_role: row.try_get("agent_role")?,
        status: parse_session_status(row.try_get::<String, _>("status")?.as_str()),
        provider_session_ref: row.try_get("provider_session_ref")?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    })
}

pub(super) fn build_mobile_connection_profile_from_row(
    row: SqliteRow,
) -> Result<MobileConnectionProfile> {
    let id: String = row.try_get("id")?;
    let scopes_json: String = row.try_get("scopes_json")?;
    let created_at: String = row.try_get("created_at")?;
    let last_used_at: Option<String> = row.try_get("last_used_at")?;
    let scopes: Vec<String> = serde_json::from_str(&scopes_json).map_err(|err| {
        anyhow::anyhow!(
            "invalid mobile profile scopes_json for profile {}: {err}",
            id
        )
    })?;
    Ok(MobileConnectionProfile {
        id: ConnectionProfileId(uuid::Uuid::parse_str(&id)?),
        label: row.try_get("label")?,
        base_url: row.try_get("base_url")?,
        token_prefix: row.try_get("token_prefix")?,
        scopes,
        created_at: parse_dt(&created_at)?,
        last_used_at: last_used_at.as_deref().map(parse_dt).transpose()?,
    })
}

pub(super) fn build_mobile_device_from_row(row: SqliteRow) -> Result<MobileDeviceRegistration> {
    let id: String = row.try_get("id")?;
    let profile_id: String = row.try_get("profile_id")?;
    let created_at: String = row.try_get("created_at")?;
    let last_seen_at: String = row.try_get("last_seen_at")?;
    Ok(MobileDeviceRegistration {
        id: MobileDeviceId(uuid::Uuid::parse_str(&id)?),
        profile_id: ConnectionProfileId(uuid::Uuid::parse_str(&profile_id)?),
        device_label: row.try_get("device_label")?,
        platform: row.try_get("platform")?,
        push_token: row.try_get("push_token")?,
        push_provider: row.try_get("push_provider")?,
        public_key: row.try_get("public_key")?,
        app_version: row.try_get("app_version")?,
        created_at: parse_dt(&created_at)?,
        last_seen_at: parse_dt(&last_seen_at)?,
    })
}

pub(super) fn map_merge_queue_entry(row: SqliteRow) -> Option<MergeQueueEntry> {
    let id: String = row.try_get("id").ok()?;
    let workspace_id: String = row.try_get("workspace_id").ok()?;
    let worktree_id: Option<String> = row.try_get("worktree_id").ok()?;
    let session_id: Option<String> = row.try_get("session_id").ok()?;
    let target_branch: String = row.try_get("target_branch").ok()?;
    let patch_source: String = row.try_get("patch_source").ok()?;
    let created_at: String = row.try_get("created_at").ok()?;
    let updated_at: String = row.try_get("updated_at").ok()?;
    let status: String = row.try_get("status").ok()?;
    Some(MergeQueueEntry {
        id: MergeQueueEntryId(uuid::Uuid::parse_str(&id).ok()?),
        workspace_id: WorkspaceId(uuid::Uuid::parse_str(&workspace_id).ok()?),
        worktree_id: worktree_id
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(WorktreeId),
        session_id: parse_optional_session_id(session_id),
        target_branch,
        message: row.try_get("message").ok(),
        patch_source: parse_merge_queue_patch_source(&patch_source),
        base_commit_sha: row.try_get("base_commit_sha").ok(),
        head_commit_sha: row.try_get("head_commit_sha").ok(),
        patch_path: row.try_get("patch_path").ok()?,
        patch_size: row.try_get("patch_size").ok()?,
        status: parse_merge_queue_entry_status(&status),
        result_commit_sha: row.try_get("result_commit_sha").ok(),
        error_message: row.try_get("error_message").ok(),
        created_at: parse_dt(&created_at).ok()?,
        updated_at: parse_dt(&updated_at).ok()?,
    })
}

pub(super) fn map_merge_queue_run(row: SqliteRow) -> Option<MergeQueueRun> {
    let id: String = row.try_get("id").ok()?;
    let entry_id: String = row.try_get("entry_id").ok()?;
    let status: String = row.try_get("status").ok()?;
    let started_at: String = row.try_get("started_at").ok()?;
    let finished_at: Option<String> = row.try_get("finished_at").ok()?;
    Some(MergeQueueRun {
        id: MergeQueueRunId(uuid::Uuid::parse_str(&id).ok()?),
        entry_id: MergeQueueEntryId(uuid::Uuid::parse_str(&entry_id).ok()?),
        status: parse_merge_queue_run_status(&status),
        started_at: parse_dt(&started_at).ok()?,
        finished_at: finished_at.as_deref().and_then(|v| parse_dt(v).ok()),
        exit_code: row.try_get("exit_code").ok(),
        log_path: row.try_get("log_path").ok(),
        error_message: row.try_get("error_message").ok(),
        result_commit_sha: row.try_get("result_commit_sha").ok(),
    })
}

pub(super) fn session_metadata_from_session(session: &Session) -> SessionMetadata {
    SessionMetadata {
        id: session.id,
        task_id: session.task_id,
        workspace_id: session.workspace_id,
        worktree_id: session.worktree_id,
        execution_environment: session.execution_environment,
        parent_session_id: session.parent_session_id,
        relationship: session.relationship.clone(),
        provider_id: session.provider_id.clone(),
        model_id: session.model_id.clone(),
        reasoning_effort: session.reasoning_effort.clone(),
        title: session.title.clone(),
        agent_role: session.agent_role.clone(),
        status: session.status.clone(),
        provider_session_ref: session.provider_session_ref.clone(),
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}

pub(super) fn session_head_to_snapshot(head: SessionHead) -> SessionHeadSnapshot {
    SessionHeadSnapshot {
        session: session_metadata_from_session(&head.session),
        turns: head.turns,
        tool_summaries: head.tool_summaries,
        events: head.events,
        messages: head.messages,
        last_event_seq: head.last_event_seq,
        projection_rev: head.projection_rev,
        state_rev: head.last_event_seq,
        activity: head.activity,
        has_more_turns: head.has_more_turns,
        history_cursor: None,
        has_more_history: false,
        summary_checkpoint: head.summary_checkpoint,
        head_window: head.head_window,
    }
}

pub(super) fn decode_session_snapshot_summary_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<SessionSnapshotSummary> {
    let id: String = row.try_get("id")?;
    let task_id: String = row.try_get("task_id")?;
    let ws_id: String = row.try_get("workspace_id")?;
    let wt_id: String = row.try_get("worktree_id")?;
    let created_at: String = row.try_get("created_at")?;
    let updated_at: String = row.try_get("updated_at")?;
    let last_message_at: Option<String> = row.try_get("last_message_at")?;
    let last_message_preview: Option<String> = row.try_get("last_message_preview")?;
    let last_message_content: Option<String> = row.try_get("last_message_content").ok().flatten();
    let last_event_seq: Option<i64> = row.try_get("last_event_seq")?;
    let projection_rev: i64 = row.try_get("projection_rev")?;
    let last_turn_status: Option<String> = row.try_get("last_turn_status")?;
    let running_turn_count: i64 = row.try_get("running_turn_count")?;

    let session = SessionMetadata {
        id: SessionId(uuid::Uuid::parse_str(&id)?),
        task_id: TaskId(uuid::Uuid::parse_str(&task_id)?),
        workspace_id: WorkspaceId(uuid::Uuid::parse_str(&ws_id)?),
        worktree_id: WorktreeId(uuid::Uuid::parse_str(&wt_id)?),
        execution_environment: parse_execution_environment(
            row.try_get::<String, _>("execution_environment")?.as_str(),
        )?,
        parent_session_id: parse_optional_session_id(row.try_get("parent_session_id")?),
        relationship: row.try_get("relationship")?,
        provider_id: row.try_get("provider_id")?,
        model_id: row.try_get("model_id")?,
        reasoning_effort: row.try_get("reasoning_effort")?,
        title: row.try_get("title")?,
        agent_role: row.try_get("agent_role")?,
        status: parse_session_status(row.try_get::<String, _>("status")?.as_str()),
        provider_session_ref: row.try_get("provider_session_ref")?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    };

    let activity = derive_activity_from_status(
        last_turn_status.as_deref().map(parse_session_turn_status),
        running_turn_count > 0,
    );
    let last_message_preview = last_message_preview
        .filter(|preview| !preview.is_empty())
        .or_else(|| {
            last_message_content.and_then(|content| {
                let preview = derive_message_preview(&content);
                if preview.is_empty() {
                    None
                } else {
                    Some(preview)
                }
            })
        });

    Ok(SessionSnapshotSummary {
        session,
        last_message_at: last_message_at.as_deref().map(parse_dt).transpose()?,
        last_message_preview,
        last_event_seq,
        projection_rev,
        state_rev: last_event_seq.unwrap_or(0),
        activity,
        unread: None,
    })
}
