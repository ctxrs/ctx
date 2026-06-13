fn decode_active_head_snapshot_row(row: &sqlx::sqlite::SqliteRow) -> Result<SessionHeadSnapshot> {
    let session_id: String = row.try_get("session_id")?;
    let task_id: String = row.try_get("task_id")?;
    let workspace_id_value: String = row.try_get("workspace_id")?;
    let worktree_id: String = row.try_get("worktree_id")?;
    let created_at: String = row.try_get("created_at")?;
    let updated_at: String = row.try_get("updated_at")?;
    let status: String = row.try_get("status")?;

    let session = Session {
        id: SessionId(uuid::Uuid::parse_str(&session_id)?),
        task_id: TaskId(uuid::Uuid::parse_str(&task_id)?),
        workspace_id: WorkspaceId(uuid::Uuid::parse_str(&workspace_id_value)?),
        worktree_id: WorktreeId(uuid::Uuid::parse_str(&worktree_id)?),
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
        status: parse_session_status(&status),
        provider_session_ref: row.try_get("provider_session_ref")?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    };

    let turns_json: String = row.try_get("turns_json")?;
    let tool_summaries_json: String = row.try_get("tool_summaries_json")?;
    let messages_json: String = row.try_get("messages_json")?;
    let head_window_json: String = row.try_get("head_window_json")?;
    let summary_checkpoint_json: Option<String> = row.try_get("summary_checkpoint_json")?;

    let mut turns: Vec<SessionTurn> =
        serde_json::from_str(&turns_json).context("deserializing active head turns")?;
    let tool_summaries: Vec<SessionTurnToolSummary> = serde_json::from_str(&tool_summaries_json)
        .context("deserializing active head tool summaries")?;
    let messages: Vec<Message> =
        serde_json::from_str(&messages_json).context("deserializing active head messages")?;
    let head_window: SessionHeadWindow =
        serde_json::from_str(&head_window_json).context("deserializing active head window")?;
    let summary_checkpoint: Option<SessionSummaryCheckpoint> = summary_checkpoint_json
        .map(|json| {
            serde_json::from_str(&json).context("deserializing active head summary checkpoint")
        })
        .transpose()?;

    let mut events = Vec::new();
    strip_snapshot_partials(&mut turns, &mut events);

    let last_status = turns.last().map(|t| t.status.clone());
    let has_running_turn = turns.iter().any(|turn| {
        matches!(
            turn.status,
            SessionTurnStatus::Starting | SessionTurnStatus::Running
        )
    });
    let activity = derive_activity_from_status(last_status, has_running_turn);
    let has_more_turns: i64 = row.try_get("has_more_turns")?;
    let last_event_seq: i64 = row.try_get("last_event_seq")?;
    let projection_rev: i64 = row.try_get("projection_rev")?;

    Ok(SessionHeadSnapshot {
        session: session_metadata_from_session(&session),
        turns,
        tool_summaries,
        events,
        messages,
        last_event_seq,
        projection_rev,
        state_rev: last_event_seq,
        activity,
        has_more_turns: has_more_turns != 0,
        history_cursor: None,
        has_more_history: false,
        summary_checkpoint,
        head_window,
    })
}
