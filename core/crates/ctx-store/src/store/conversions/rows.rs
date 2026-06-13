pub(super) fn build_subagent_invocation_from_row(r: SqliteRow) -> Result<SubagentInvocation> {
    let id: String = r.try_get("id")?;
    let tool_call_id: String = r.try_get("tool_call_id")?;
    let parent_session_id: String = r.try_get("parent_session_id")?;
    let parent_turn_id: Option<String> = r.try_get("parent_turn_id")?;
    let created_at: String = r.try_get("created_at")?;
    let updated_at: String = r.try_get("updated_at")?;
    let request_json = r
        .try_get::<Option<String>, _>("request_json")?
        .and_then(|raw| serde_json::from_str(&raw).ok());

    Ok(SubagentInvocation {
        id,
        tool_call_id,
        parent_session_id: SessionId(uuid::Uuid::parse_str(&parent_session_id)?),
        parent_turn_id: parent_turn_id
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(TurnId),
        requested_count: r.try_get("requested_count")?,
        request_json,
        status: r.try_get("status")?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
        children: Vec::new(),
    })
}

pub(super) fn build_subagent_invocation_child_from_row(
    r: SqliteRow,
) -> Result<SubagentInvocationChild> {
    let invocation_id: String = r.try_get("invocation_id")?;
    let child_session_id: String = r.try_get("child_session_id")?;
    let run_id: Option<String> = r.try_get("run_id")?;
    let created_at: String = r.try_get("created_at")?;
    let updated_at: String = r.try_get("updated_at")?;

    Ok(SubagentInvocationChild {
        invocation_id,
        child_session_id: SessionId(uuid::Uuid::parse_str(&child_session_id)?),
        run_id: run_id
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(RunId),
        position: r.try_get("position")?,
        status: r.try_get("status")?,
        label: r.try_get("label")?,
        harness: r.try_get("harness")?,
        model: r.try_get("model")?,
        reasoning_effort: r.try_get("reasoning_effort")?,
        prompt_length: r.try_get("prompt_length")?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    })
}

pub(super) fn build_artifact_from_row(r: SqliteRow) -> Result<Artifact> {
    let id: String = r.try_get("id")?;
    let session_id: String = r.try_get("session_id")?;
    let task_id: String = r.try_get("task_id")?;
    let workspace_id: String = r.try_get("workspace_id")?;
    let worktree_id: String = r.try_get("worktree_id")?;
    let created_at: String = r.try_get("created_at")?;

    Ok(Artifact {
        id: ArtifactId(uuid::Uuid::parse_str(&id)?),
        session_id: SessionId(uuid::Uuid::parse_str(&session_id)?),
        task_id: TaskId(uuid::Uuid::parse_str(&task_id)?),
        workspace_id: WorkspaceId(uuid::Uuid::parse_str(&workspace_id)?),
        worktree_id: WorktreeId(uuid::Uuid::parse_str(&worktree_id)?),
        name: r.try_get("name")?,
        absolute_path: r.try_get("absolute_path")?,
        mime_type: r.try_get("mime_type")?,
        bytes: r.try_get("bytes")?,
        created_at: parse_dt(&created_at)?,
        missing: None,
    })
}

pub(super) fn build_session_turn_from_row(r: SqliteRow) -> Result<SessionTurn> {
    let turn_id: String = r.try_get("turn_id")?;
    let session_id: String = r.try_get("session_id")?;
    let run_id: Option<String> = r.try_get("run_id")?;
    let user_message_id: Option<String> = r.try_get("user_message_id")?;
    let status: String = r.try_get("status")?;
    let started_at: String = r.try_get("started_at")?;
    let updated_at: String = r.try_get("updated_at")?;
    let metrics_json: Option<String> = r.try_get("metrics_json")?;
    let metrics_json = metrics_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    let failure_json: Option<String> = r.try_get("failure_json")?;
    let failure = failure_json
        .as_deref()
        .map(serde_json::from_str::<SessionTurnFailure>)
        .transpose()
        .context("parsing turn failure_json")?;

    Ok(SessionTurn {
        turn_id: TurnId(uuid::Uuid::parse_str(&turn_id)?),
        session_id: SessionId(uuid::Uuid::parse_str(&session_id)?),
        run_id: run_id
            .as_deref()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .map(RunId),
        user_message_id: user_message_id
            .as_deref()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .map(MessageId),
        status: parse_session_turn_status(status.as_str()),
        start_seq: r.try_get("start_seq")?,
        end_seq: r.try_get("end_seq")?,
        started_at: parse_dt(&started_at)?,
        updated_at: parse_dt(&updated_at)?,
        assistant_partial: r.try_get("assistant_partial")?,
        thought_partial: r.try_get("thought_partial")?,
        metrics_json,
        failure,
        tool_total: r.try_get("tool_total")?,
        tool_pending: r.try_get("tool_pending")?,
        tool_running: r.try_get("tool_running")?,
        tool_completed: r.try_get("tool_completed")?,
        tool_failed: r.try_get("tool_failed")?,
    })
}

pub(super) fn build_session_turn_tool_from_row(r: SqliteRow) -> Result<SessionTurnTool> {
    let session_id: String = r.try_get("session_id")?;
    let tool_call_id: String = r.try_get("tool_call_id")?;
    let turn_id: String = r.try_get("turn_id")?;
    let created_at: String = r.try_get("created_at")?;
    let updated_at: String = r.try_get("updated_at")?;
    let input_json: Option<String> = r.try_get("input_json")?;
    let input_json = input_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    let order_seq: i64 = r.try_get("order_seq")?;
    let first_event_seq: Option<i64> = r.try_get("first_event_seq")?;
    let input_truncated: Option<i64> = r.try_get("input_truncated")?;
    let input_original_bytes: Option<i64> = r.try_get("input_original_bytes")?;
    let output_truncated: Option<i64> = r.try_get("output_truncated")?;
    let output_original_bytes: Option<i64> = r.try_get("output_original_bytes")?;

    Ok(SessionTurnTool {
        session_id: SessionId(uuid::Uuid::parse_str(&session_id)?),
        tool_call_id,
        turn_id: TurnId(uuid::Uuid::parse_str(&turn_id)?),
        tool_kind: r.try_get("tool_kind")?,
        provider_tool_name: r.try_get("provider_tool_name")?,
        title: r.try_get("title")?,
        subtitle: r.try_get("subtitle")?,
        status: r.try_get("status")?,
        input_json,
        output_text: r.try_get("output_text")?,
        order_seq,
        first_event_seq,
        input_truncated: input_truncated.map(|value| value != 0),
        input_original_bytes,
        output_truncated: output_truncated.map(|value| value != 0),
        output_original_bytes,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    })
}

pub(super) fn build_session_turn_tool_summary_from_row(
    r: SqliteRow,
) -> Result<SessionTurnToolSummary> {
    let session_id: String = r.try_get("session_id")?;
    let tool_call_id: String = r.try_get("tool_call_id")?;
    let turn_id: String = r.try_get("turn_id")?;
    let created_at: String = r.try_get("created_at")?;
    let updated_at: String = r.try_get("updated_at")?;
    let input_json: Option<String> = r.try_get("input_json")?;
    let input_json = input_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    let output_text: Option<String> = r.try_get("output_text")?;
    let order_seq: i64 = r.try_get("order_seq")?;
    let first_event_seq: Option<i64> = r.try_get("first_event_seq")?;
    let input_truncated: Option<i64> = r.try_get("input_truncated")?;
    let input_original_bytes: Option<i64> = r.try_get("input_original_bytes")?;
    let output_truncated: Option<i64> = r.try_get("output_truncated")?;
    let output_original_bytes: Option<i64> = r.try_get("output_original_bytes")?;
    let input_preview = tool_input_preview_from_value(input_json.as_ref());

    Ok(SessionTurnToolSummary {
        session_id: SessionId(uuid::Uuid::parse_str(&session_id)?),
        tool_call_id,
        turn_id: TurnId(uuid::Uuid::parse_str(&turn_id)?),
        tool_kind: r.try_get("tool_kind")?,
        provider_tool_name: r.try_get("provider_tool_name")?,
        title: r.try_get("title")?,
        subtitle: r.try_get("subtitle")?,
        status: r.try_get("status")?,
        input_preview,
        output_preview: output_text,
        order_seq,
        first_event_seq,
        input_truncated: input_truncated.map(|value| value != 0),
        input_original_bytes,
        output_truncated: output_truncated.map(|value| value != 0),
        output_original_bytes,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
    })
}
