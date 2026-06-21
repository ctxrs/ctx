use super::*;

const PROJECTOR_TEXT_LIMIT: usize = 8 * 1024;
const PROJECTOR_SEQUENCE_TIE_SPAN: i64 = 100_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkProjectionResult {
    pub work_records: usize,
    pub links: usize,
    pub events: usize,
}

impl WorkProjectionResult {
    fn add(&mut self, other: WorkProjectionResult) {
        self.work_records += other.work_records;
        self.links += other.links;
        self.events += other.events;
    }
}

impl Store {
    pub async fn project_task_sessions_to_work(
        &self,
        task_id: TaskId,
    ) -> Result<WorkProjectionResult> {
        let sessions = self.list_all_sessions_for_task(task_id).await?;
        let mut result = WorkProjectionResult::default();
        for session in sessions {
            result.add(self.project_session_to_work(session.id).await?);
        }
        Ok(result)
    }

    pub async fn project_session_to_work(
        &self,
        session_id: SessionId,
    ) -> Result<WorkProjectionResult> {
        let Some(session) = self.get_session(session_id).await? else {
            anyhow::bail!("session does not exist");
        };
        let task = self
            .get_task(session.task_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session task does not exist"))?;
        let worktree = self.get_worktree(session.worktree_id).await?;

        let now = Utc::now();
        let work_id = if let Some(record) = self
            .find_work_record_by_link(
                session.workspace_id,
                WorkLinkTargetKind::Session,
                &session.id.0.to_string(),
            )
            .await?
        {
            record.work_id
        } else if let Some(record) = self
            .find_work_record_by_link(
                session.workspace_id,
                WorkLinkTargetKind::Task,
                &session.task_id.0.to_string(),
            )
            .await?
        {
            record.work_id
        } else {
            stable_task_work_id(session.task_id)
        };

        let created_at = [task.created_at, session.created_at]
            .into_iter()
            .min()
            .unwrap_or(session.created_at);
        let updated_at = [task.updated_at, session.updated_at]
            .into_iter()
            .max()
            .unwrap_or(now);

        let record = WorkRecord {
            work_id: work_id.clone(),
            workspace_id: session.workspace_id,
            title: Some(bounded_redacted_text(&task.title, 1_000)),
            objective: task
                .description
                .as_deref()
                .map(|description| bounded_redacted_text(description, 2_000)),
            lifecycle: lifecycle_from_task_status(&task.status),
            primary_repo_root: None,
            primary_branch: worktree.as_ref().and_then(|worktree| {
                worktree
                    .git_branch
                    .as_deref()
                    .map(|branch| bounded_redacted_text(branch, 500))
            }),
            base_commit: worktree
                .as_ref()
                .map(|worktree| worktree.base_commit_sha.clone()),
            head_commit: None,
            current_diff_fingerprint: None,
            trust_verdict: WorkTrustVerdict::UntrustedLocalCapture,
            summary_freshness: WorkSummaryFreshness::Missing,
            metadata_json: Some(serde_json::json!({
                "projection": "ade_session",
                "bounded": true,
                "notes": [
                    "projects durable session events only",
                    "transient stream-only events are unavailable for backfill"
                ]
            })),
            created_at,
            updated_at,
            schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
        };
        self.upsert_work_record(&record).await?;

        let mut result = WorkProjectionResult {
            work_records: 1,
            links: 0,
            events: 0,
        };

        for link in base_session_links(&session, &work_id, now) {
            self.upsert_work_record_link(&link).await?;
            result.links += 1;
        }

        let events = self.list_session_events(session.id).await?;
        let mut run_ids = HashSet::new();
        for event in &events {
            if let Some(run_id) = event.run_id {
                run_ids.insert(run_id);
            }
        }
        for run_id in run_ids {
            let link = projection_link(
                session.workspace_id,
                &work_id,
                WorkLinkTargetKind::Run,
                &run_id.0.to_string(),
                WorkLinkRole::Source,
                now,
            );
            self.upsert_work_record_link(&link).await?;
            result.links += 1;
        }

        for event in session_state_events(&session, &work_id, now) {
            self.append_work_event(&event).await?;
            result.events += 1;
        }

        for event in events
            .iter()
            .filter_map(|event| projected_session_event(&session, &work_id, event))
        {
            self.append_work_event(&event).await?;
            result.events += 1;
        }

        for artifact in self.list_session_artifacts(session.id).await? {
            let link = WorkRecordLink {
                target_json: Some(serde_json::json!({
                    "name": artifact.name.as_deref().map(|name| bounded_redacted_text(name, 300)),
                    "mime_type": artifact.mime_type,
                    "bytes": artifact.bytes,
                })),
                ..projection_link(
                    session.workspace_id,
                    &work_id,
                    WorkLinkTargetKind::Artifact,
                    &artifact.id.0.to_string(),
                    WorkLinkRole::Result,
                    artifact.created_at,
                )
            };
            self.upsert_work_record_link(&link).await?;
            result.links += 1;

            let source_kind = "session_artifact";
            let source_id = artifact.id.0.to_string();
            let sequence = stable_projector_sequence(artifact.created_at, &source_id);
            self.append_work_event(&WorkEvent {
                event_id: stable_work_event_id(&work_id, source_kind, &source_id, sequence),
                work_id: work_id.clone(),
                workspace_id: session.workspace_id,
                sequence,
                source_kind: Some(source_kind.to_string()),
                source_id: Some(source_id),
                event_type: WorkEventType::ArtifactCreated,
                event_time: artifact.created_at,
                actor_kind: WorkActorKind::Agent,
                provider: Some(session.provider_id.clone()),
                harness: Some(session.agent_role.clone()),
                model: Some(session.model_id.clone()),
                redaction_class: WorkRedactionClass::LocalRedacted,
                source: RecordSource::Session,
                fidelity: RecordFidelity::Declared,
                trust: RecordTrust::Low,
                payload_json: None,
                redacted_text: Some(bounded_redacted_text(
                    &format!(
                        "Artifact created: {} ({}, {} bytes)",
                        artifact.name.as_deref().unwrap_or("unnamed"),
                        artifact.mime_type,
                        artifact.bytes
                    ),
                    PROJECTOR_TEXT_LIMIT,
                )),
                artifact_ref: None,
                created_at: now,
                schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
            })
            .await?;
            result.events += 1;
        }

        for invocation in self
            .list_subagent_invocations_for_session(session.id, None)
            .await?
        {
            let source_kind = "subagent_invocation";
            let source_id = invocation.id.clone();
            let sequence = stable_projector_sequence(invocation.updated_at, &source_id);
            self.append_work_event(&WorkEvent {
                event_id: stable_work_event_id(&work_id, source_kind, &source_id, sequence),
                work_id: work_id.clone(),
                workspace_id: session.workspace_id,
                sequence,
                source_kind: Some(source_kind.to_string()),
                source_id: Some(source_id),
                event_type: WorkEventType::Session,
                event_time: invocation.updated_at,
                actor_kind: WorkActorKind::Subagent,
                provider: Some(session.provider_id.clone()),
                harness: Some(session.agent_role.clone()),
                model: Some(session.model_id.clone()),
                redaction_class: WorkRedactionClass::LocalRedacted,
                source: RecordSource::Session,
                fidelity: RecordFidelity::Declared,
                trust: RecordTrust::Low,
                payload_json: None,
                redacted_text: Some(bounded_redacted_text(
                    &format!(
                        "Subagent invocation {} status {} children {}",
                        invocation.id,
                        invocation.status,
                        invocation.children.len()
                    ),
                    PROJECTOR_TEXT_LIMIT,
                )),
                artifact_ref: None,
                created_at: now,
                schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
            })
            .await?;
            result.events += 1;

            for child in invocation.children {
                let link = projection_link(
                    session.workspace_id,
                    &work_id,
                    WorkLinkTargetKind::Session,
                    &child.child_session_id.0.to_string(),
                    WorkLinkRole::Child,
                    child.updated_at,
                );
                self.upsert_work_record_link(&link).await?;
                result.links += 1;
                if let Some(run_id) = child.run_id {
                    let link = projection_link(
                        session.workspace_id,
                        &work_id,
                        WorkLinkTargetKind::Run,
                        &run_id.0.to_string(),
                        WorkLinkRole::Child,
                        child.updated_at,
                    );
                    self.upsert_work_record_link(&link).await?;
                    result.links += 1;
                }
            }
        }

        Ok(result)
    }
}

fn stable_task_work_id(task_id: TaskId) -> WorkRecordId {
    WorkRecordId::from_id(format!("wrk_ade_task_{}", task_id.0.simple()))
}

fn stable_work_link_id(
    work_id: &WorkRecordId,
    target_kind: WorkLinkTargetKind,
    target_id: &str,
    role: WorkLinkRole,
) -> WorkRecordLinkId {
    WorkRecordLinkId::from_id(format!(
        "wln_ade_{}_{target_kind:?}_{}_{role:?}",
        work_id.0,
        target_id.replace('-', "")
    ))
}

fn stable_work_event_id(
    work_id: &WorkRecordId,
    source_kind: &str,
    source_id: &str,
    sequence: i64,
) -> WorkEventId {
    let material = format!("{}:{source_kind}:{source_id}:{sequence}", work_id.0);
    WorkEventId::from_id(format!("wev_ade_{:016x}", stable_u64(&material)))
}

fn stable_projector_sequence(event_time: DateTime<Utc>, source_id: &str) -> i64 {
    let hash = stable_u64(source_id);
    event_time
        .timestamp_millis()
        .saturating_mul(PROJECTOR_SEQUENCE_TIE_SPAN)
        .saturating_add((hash % PROJECTOR_SEQUENCE_TIE_SPAN as u64) as i64)
}

fn stable_u64(value: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn projection_link(
    workspace_id: WorkspaceId,
    work_id: &WorkRecordId,
    target_kind: WorkLinkTargetKind,
    target_id: &str,
    role: WorkLinkRole,
    timestamp: DateTime<Utc>,
) -> WorkRecordLink {
    WorkRecordLink {
        link_id: stable_work_link_id(work_id, target_kind, target_id, role),
        work_id: work_id.clone(),
        workspace_id,
        target_kind,
        target_id: Some(target_id.to_string()),
        target_json: None,
        role,
        source: RecordSource::Session,
        fidelity: RecordFidelity::Declared,
        trust: RecordTrust::Low,
        created_at: timestamp,
        updated_at: timestamp,
        schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
    }
}

fn base_session_links(
    session: &Session,
    work_id: &WorkRecordId,
    now: DateTime<Utc>,
) -> Vec<WorkRecordLink> {
    vec![
        projection_link(
            session.workspace_id,
            work_id,
            WorkLinkTargetKind::Task,
            &session.task_id.0.to_string(),
            WorkLinkRole::Source,
            now,
        ),
        projection_link(
            session.workspace_id,
            work_id,
            WorkLinkTargetKind::Session,
            &session.id.0.to_string(),
            WorkLinkRole::Source,
            now,
        ),
        projection_link(
            session.workspace_id,
            work_id,
            WorkLinkTargetKind::Worktree,
            &session.worktree_id.0.to_string(),
            WorkLinkRole::Context,
            now,
        ),
    ]
}

fn session_state_events(
    session: &Session,
    work_id: &WorkRecordId,
    now: DateTime<Utc>,
) -> Vec<WorkEvent> {
    let actor_kind = if session.relationship.as_deref() == Some("sub_agent") {
        WorkActorKind::Subagent
    } else {
        WorkActorKind::Agent
    };
    let source_kind = "session_state";
    let source_id = session.id.0.to_string();
    let sequence = stable_projector_sequence(session.created_at, &source_id);
    vec![WorkEvent {
        event_id: stable_work_event_id(work_id, source_kind, &source_id, sequence),
        work_id: work_id.clone(),
        workspace_id: session.workspace_id,
        sequence,
        source_kind: Some(source_kind.to_string()),
        source_id: Some(source_id),
        event_type: WorkEventType::Session,
        event_time: session.created_at,
        actor_kind,
        provider: Some(session.provider_id.clone()),
        harness: Some(session.agent_role.clone()),
        model: Some(session.model_id.clone()),
        redaction_class: WorkRedactionClass::LocalRedacted,
        source: RecordSource::Session,
        fidelity: RecordFidelity::Declared,
        trust: RecordTrust::Low,
        payload_json: None,
        redacted_text: Some(bounded_redacted_text(
            &format!("Session {} status {:?}", session.title, session.status),
            PROJECTOR_TEXT_LIMIT,
        )),
        artifact_ref: None,
        created_at: now,
        schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
    }]
}

fn projected_session_event(
    session: &Session,
    work_id: &WorkRecordId,
    event: &SessionEvent,
) -> Option<WorkEvent> {
    let (event_type, actor_kind) = match event.event_type {
        SessionEventType::UserMessage | SessionEventType::InputQueued => {
            (WorkEventType::UserMessage, WorkActorKind::Human)
        }
        SessionEventType::AssistantComplete | SessionEventType::AssistantMessageInserted => {
            (WorkEventType::AssistantMessage, WorkActorKind::Agent)
        }
        SessionEventType::ToolCall => (WorkEventType::ToolCallStart, WorkActorKind::Agent),
        SessionEventType::ToolResult => (WorkEventType::ToolOutput, WorkActorKind::Agent),
        SessionEventType::ArtifactsSet => (WorkEventType::ArtifactCreated, WorkActorKind::Agent),
        SessionEventType::TurnStarted
        | SessionEventType::TurnFinished
        | SessionEventType::TurnInterrupted
        | SessionEventType::Done
        | SessionEventType::Notice
        | SessionEventType::Plan => (WorkEventType::Session, WorkActorKind::Agent),
        _ => return None,
    };

    let actor_kind = if session.relationship.as_deref() == Some("sub_agent")
        && actor_kind == WorkActorKind::Agent
    {
        WorkActorKind::Subagent
    } else {
        actor_kind
    };

    let redacted_text = event_redacted_text(event);
    let source_kind = "session_event";
    let source_id = event.id.0.to_string();
    let sequence =
        stable_projector_sequence(event.created_at, &format!("{}:{}", event.seq, event.id.0));
    Some(WorkEvent {
        event_id: stable_work_event_id(work_id, source_kind, &source_id, sequence),
        work_id: work_id.clone(),
        workspace_id: session.workspace_id,
        sequence,
        source_kind: Some(source_kind.to_string()),
        source_id: Some(source_id),
        event_type,
        event_time: event.created_at,
        actor_kind,
        provider: Some(session.provider_id.clone()),
        harness: Some(session.agent_role.clone()),
        model: Some(session.model_id.clone()),
        redaction_class: WorkRedactionClass::LocalRedacted,
        source: RecordSource::Session,
        fidelity: RecordFidelity::Exact,
        trust: RecordTrust::Low,
        payload_json: None,
        redacted_text: Some(redacted_text),
        artifact_ref: None,
        created_at: event.created_at,
        schema_version: WORK_OBSERVABILITY_SCHEMA_VERSION,
    })
}

fn event_redacted_text(event: &SessionEvent) -> String {
    let event_label = session_event_type_to_str(&event.event_type);
    let text = extract_text(&event.payload_json).unwrap_or_else(|| {
        ctx_core::redaction::redact_json_value(event.payload_json.clone()).to_string()
    });
    bounded_redacted_text(&format!("{event_label}: {text}"), PROJECTOR_TEXT_LIMIT)
}

fn extract_text(value: &Value) -> Option<String> {
    for pointer in [
        "/content",
        "/message/content",
        "/message",
        "/text",
        "/input",
        "/prompt",
        "/outputText",
        "/output_text",
        "/result",
        "/toolCall/outputText",
        "/toolCall/output_text",
        "/rawOutput/aggregated_output",
        "/rawOutput/output",
    ] {
        if let Some(text) = value.pointer(pointer).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

fn lifecycle_from_task_status(status: &TaskStatus) -> WorkLifecycle {
    match status {
        TaskStatus::Pending | TaskStatus::Running => WorkLifecycle::Active,
        TaskStatus::Completed => WorkLifecycle::ReadyForReview,
        TaskStatus::Failed => WorkLifecycle::Blocked,
        TaskStatus::Cancelled => WorkLifecycle::Abandoned,
    }
}

fn bounded_redacted_text(value: &str, limit: usize) -> String {
    let redacted = redact_local_paths(ctx_core::redaction::redact_sensitive(value));
    let redacted = ctx_core::models::normalize_archive_text(&redacted).text;
    if redacted.len() <= limit {
        return redacted;
    }
    let mut end = 0;
    for (idx, _) in redacted.char_indices() {
        if idx > limit {
            break;
        }
        end = idx;
    }
    format!("{}\n[truncated]", &redacted[..end])
}

fn redact_local_paths(input: String) -> String {
    let mut output = input;
    for marker in [
        "/home/",
        "/Users/",
        "/tmp/",
        "/var/folders/",
        "/private/var/",
        "C:\\Users\\",
        "C:/Users/",
    ] {
        output = redact_path_segments(output, marker);
    }
    output
}

fn redact_path_segments(input: String, marker: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input.as_str();
    while let Some(start) = rest.find(marker) {
        output.push_str(&rest[..start]);
        output.push_str("[redacted:local_path]");
        let matched = &rest[start..];
        let end = matched
            .find(|ch: char| {
                ch.is_whitespace()
                    || matches!(ch, '"' | '\'' | ')' | ']' | '}' | '<' | '>' | ',' | ';')
            })
            .unwrap_or(matched.len());
        rest = &matched[end..];
    }
    output.push_str(rest);
    output
}
