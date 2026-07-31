use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use ctx_history_core::{CaptureProvider, CoreContentPolicyStatus, EventType};
use ctx_history_index::{CoreEventRecord, SessionRecord, VerifiedIndex};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    analytics::{count_bucket, ShowTelemetry},
    local_usage::{CliUsage, ResultObservationAction},
    output::{compact_json, OutputFormat},
    presentation_limit::{
        enforce_presentation_output_limit, serialized_json_bytes, CLI_PRESENTATION_MAX_OUTPUT_BYTES,
    },
    provider_args::ProviderArg,
    transcript::TranscriptMode,
    ui::{canonical_human_output_bytes, Ui},
    ShowArgs, ShowTarget,
};

use super::{
    render::{enforce_json_output_limit, render_show_document, timestamp_json, write_show_value},
    shared::{
        open_index, resolve_core_event, resolve_session, validate_ctx_id, validate_session_selector,
    },
};

pub(crate) fn run_show(
    args: ShowArgs,
    data_root: PathBuf,
    telemetry: &mut ShowTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    validate_show_target(&args.target)?;
    let index = open_index(&data_root)?;
    match args.target {
        ShowTarget::Event(args) => {
            let selected = resolve_core_event(&index, &args.id)?;
            let events = event_window(&index, &selected, args.before, args.after, args.window)?;
            telemetry.events_returned = Some(count_bucket(events.len() as u64));
            let value = event_window_json(
                &selected,
                &events,
                args.format,
                CLI_PRESENTATION_MAX_OUTPUT_BYTES,
            )?;
            let events = value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let result_count = events.len();
            let content_bytes = serde_json::to_vec(&value["events"])?.len();
            let output_bytes = if args.format == OutputFormat::Text {
                write_show_document(&value, selected.event_id.as_uuid(), ui)?
            } else {
                write_show_value(value, args.format, None, selected.event_id.as_uuid())?
            };
            local_usage.set_result_observation(
                ResultObservationAction::OpenEvent,
                result_count,
                0,
                content_bytes,
            );
            local_usage.set_measured_output_bytes(output_bytes);
            Ok(())
        }
        ShowTarget::Session(args) => {
            let session = resolve_show_session(
                &index,
                args.id.as_deref(),
                args.provider_session.as_deref(),
                args.provider.map(ProviderArg::capture_provider),
            )?;
            let value = session_json(
                &index,
                &session,
                SessionJsonOptions {
                    mode: args.mode,
                    format: args.format,
                    max_events: None,
                    output_limit_bytes: CLI_PRESENTATION_MAX_OUTPUT_BYTES,
                },
            )?;
            telemetry.events_returned = value["events"]
                .as_array()
                .map(|events| count_bucket(events.len() as u64));
            let event_id = value["events"]
                .as_array()
                .and_then(|events| events.last())
                .and_then(|event| event["ctx_event_id"].as_str())
                .and_then(|id| Uuid::parse_str(id).ok())
                .unwrap_or_else(|| session.session_id.as_uuid());
            let events = value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let result_count = events.len();
            let content_bytes = serde_json::to_vec(&value["events"])?.len();
            let output_bytes = if args.format == OutputFormat::Text && args.out.is_none() {
                write_show_document(&value, event_id, ui)?
            } else {
                write_show_value(value, args.format, args.out, event_id)?
            };
            local_usage.set_result_observation(
                ResultObservationAction::OpenSession,
                result_count,
                0,
                content_bytes,
            );
            local_usage.set_measured_output_bytes(output_bytes);
            Ok(())
        }
    }
}

fn write_show_document(value: &Value, event_id: Uuid, ui: &mut Ui) -> Result<usize> {
    let document = render_show_document(value, ui.stdout_context());
    let output_bytes = canonical_show_output_bytes(value);
    enforce_presentation_output_limit(output_bytes, CLI_PRESENTATION_MAX_OUTPUT_BYTES, event_id)?;
    ui.write_stdout(&document)?;
    Ok(output_bytes)
}

pub(super) fn canonical_show_output_bytes(value: &Value) -> usize {
    canonical_human_output_bytes(|context| render_show_document(value, context))
}

pub(super) fn validate_show_target(target: &ShowTarget) -> Result<()> {
    match target {
        ShowTarget::Session(args) => {
            validate_session_selector(args.id.as_deref(), args.provider_session.as_deref())
        }
        ShowTarget::Event(args) => validate_ctx_id(&args.id, "event").map(|_| ()),
    }
}

pub(super) fn resolve_show_session(
    index: &VerifiedIndex,
    id: Option<&str>,
    provider_session_id: Option<&str>,
    provider: Option<CaptureProvider>,
) -> Result<SessionRecord> {
    validate_session_selector(id, provider_session_id)?;
    let session = match (id, provider_session_id) {
        (Some(id), None) => resolve_session(index, id)?,
        (None, Some(provider_session_id)) => select_show_provider_session(
            provider_session_id,
            index.sessions_by_provider_session_id(
                provider_session_id,
                provider.map(CaptureProvider::as_str),
            )?,
        )?,
        (Some(_), Some(_)) => {
            return Err(anyhow!(
                "pass either a ctx session ID or --provider-session, not both"
            ));
        }
        (None, None) => {
            return Err(anyhow!(
                "Core session lookup requires a ctx session ID or --provider-session"
            ));
        }
    };
    if let Some(provider) = provider {
        if session.provider != provider.as_str() {
            return Err(anyhow!(
                "Core session {} belongs to provider {}, not {}",
                session.session_id,
                session.provider,
                provider
            ));
        }
    }
    Ok(session)
}

fn select_show_provider_session(
    provider_session_id: &str,
    matches: Vec<SessionRecord>,
) -> Result<SessionRecord> {
    match matches.as_slice() {
        [] => Err(anyhow!(
            "provider session {provider_session_id:?} was not found in the Core generation"
        )),
        [session] => Ok(session.clone()),
        matches => Err(anyhow!(
            "provider session {provider_session_id:?} is ambiguous; first matches are {} and {}; pass --provider or a ctx session ID",
            matches[0].session_id,
            matches[1].session_id
        )),
    }
}

pub(crate) fn mcp_show_session(
    data_root: &Path,
    id: &str,
    mode: TranscriptMode,
    max_events: usize,
    output_limit_bytes: usize,
) -> Result<Value> {
    let index = open_index(data_root)?;
    let session = resolve_session(&index, id)?;
    let value = session_json(
        &index,
        &session,
        SessionJsonOptions {
            mode,
            format: OutputFormat::Json,
            max_events: Some(max_events),
            output_limit_bytes,
        },
    )?;
    let event_id = value["events"]
        .as_array()
        .and_then(|events| events.last())
        .and_then(|event| event["ctx_event_id"].as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
        .unwrap_or_else(|| session.session_id.as_uuid());
    enforce_json_output_limit(&value, output_limit_bytes, event_id)?;
    Ok(value)
}

pub(crate) fn mcp_show_event(
    data_root: &Path,
    id: &str,
    before: usize,
    after: usize,
    window: Option<usize>,
    output_limit_bytes: usize,
) -> Result<Value> {
    let index = open_index(data_root)?;
    let selected = resolve_core_event(&index, id)?;
    let events = event_window(&index, &selected, before, after, window)?;
    let value = event_window_json(&selected, &events, OutputFormat::Json, output_limit_bytes)?;
    enforce_json_output_limit(&value, output_limit_bytes, selected.event_id.as_uuid())?;
    Ok(value)
}

struct SessionJsonOptions {
    mode: TranscriptMode,
    format: OutputFormat,
    max_events: Option<usize>,
    output_limit_bytes: usize,
}

fn session_json(
    index: &VerifiedIndex,
    session: &SessionRecord,
    options: SessionJsonOptions,
) -> Result<Value> {
    // Bulk-Core integration seam: replace this ordered full-session read with
    // metadata-first selection plus a bounded Core fetch when that API lands.
    let mut events = index.core_events_for_session(session.session_id.as_uuid())?;
    let truncated = options.max_events.is_some_and(|limit| events.len() > limit);
    if let Some(limit) = options.max_events {
        events.truncate(limit);
    }
    let selected = select_session_events(&events, options.mode);
    let rendered = render_event_values(&selected, options.output_limit_bytes)?;
    Ok(session_transcript_value(
        session,
        options.mode,
        options.format,
        rendered,
        truncated,
        options.max_events,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn session_transcript_value(
    session: &SessionRecord,
    mode: TranscriptMode,
    format: OutputFormat,
    rendered: Vec<Value>,
    truncated: bool,
    max_events: Option<usize>,
) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "target": "session",
        "payload_type": "session_transcript",
        "ctx_session_id": session.session_id.as_uuid(),
        "provider": session.provider,
        "provider_session_id": session.provider_session_id,
        "mode": mode.as_str(),
        "format": format.as_str(),
        "session": {
            "id": session.session_id.as_uuid(),
            "item_id": session.session_id.as_uuid(),
            "record_type": "session",
            "ctx_session_id": session.session_id.as_uuid(),
            "provider": session.provider,
            "provider_session_id": session.provider_session_id,
            "source_format": session.source_format,
            "parent_ctx_session_id": session.parent_session_id.map(|id| id.as_uuid()),
            "root_ctx_session_id": session.root_session_id.as_uuid(),
            "branch": session.branch,
            "agent_type": session.agent_type,
            "is_primary": session.is_primary,
            "workspace": session.workspace,
            "cwd": session.cwd,
        },
        "events": rendered,
        "truncated": truncated.then(|| json!({
            "events": true,
            "max_events": max_events,
        })),
    }))
}

fn event_window_json(
    selected: &CoreEventRecord,
    events: &[CoreEventRecord],
    format: OutputFormat,
    output_limit_bytes: usize,
) -> Result<Value> {
    let references = events.iter().collect::<Vec<_>>();
    let rendered = render_event_values(&references, output_limit_bytes)?;
    event_window_value(selected, format, rendered)
}

pub(super) fn event_window_value(
    selected: &CoreEventRecord,
    format: OutputFormat,
    rendered: Vec<Value>,
) -> Result<Value> {
    let selected_value = rendered
        .iter()
        .find(|event| {
            event["ctx_event_id"].as_str() == Some(&selected.event_id.as_uuid().to_string())
        })
        .cloned()
        .ok_or_else(|| anyhow!("selected event is absent from its pinned Core event window"))?;
    Ok(compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "payload_type": "event_window",
        "ctx_event_id": selected.event_id.as_uuid(),
        "ctx_session_id": selected.session_id.as_uuid(),
        "format": format.as_str(),
        "event": selected_value,
        "events": rendered,
    })))
}

pub(super) fn render_event_values(
    events: &[&CoreEventRecord],
    output_limit_bytes: usize,
) -> Result<Vec<Value>> {
    let mut rendered = Vec::with_capacity(events.len());
    let mut serialized_event_bytes = 2_usize;
    for event in events {
        let content = &event.core_record.content;
        let content_bytes = serialized_json_bytes(&content.normalized_body)?
            .saturating_add(serialized_json_bytes(&content.structured_content)?);
        enforce_presentation_output_limit(
            serialized_event_bytes.saturating_add(content_bytes),
            output_limit_bytes,
            event.event_id.as_uuid(),
        )?;

        let value = render_event_value(event);
        serialized_event_bytes = serialized_event_bytes
            .saturating_add(usize::from(!rendered.is_empty()))
            .saturating_add(serialized_json_bytes(&value)?);
        enforce_presentation_output_limit(
            serialized_event_bytes,
            output_limit_bytes,
            event.event_id.as_uuid(),
        )?;
        rendered.push(value);
    }
    Ok(rendered)
}

pub(super) fn render_event_value(event: &CoreEventRecord) -> Value {
    let content = &event.core_record.content;
    let (policy_status, policy_reason, complete) = match &content.policy_status {
        CoreContentPolicyStatus::Selected => ("selected", None, true),
        CoreContentPolicyStatus::Redacted { reason } => ("redacted", Some(reason.as_str()), true),
        CoreContentPolicyStatus::Omitted { reason } => ("omitted", Some(reason.as_str()), false),
    };
    compact_json(json!({
        "ctx_event_id": event.event_id.as_uuid(),
        "item_id": event.event_id.as_uuid(),
        "record_type": "event",
        "ctx_session_id": event.session_id.as_uuid(),
        "provider": event.provider,
        "provider_session_id": event.provider_session_id,
        "source_format": event.source_format,
        "parent_ctx_session_id": event.parent_session_id.map(|id| id.as_uuid()),
        "root_ctx_session_id": event.root_session_id.as_uuid(),
        "branch": event.branch,
        "agent_type": event.agent_type,
        "is_primary": event.is_primary,
        "sequence": event.event_sequence,
        "event_type": event.event_type,
        "role": event.role,
        "occurred_at": timestamp_json(event.occurred_at_unix_ms),
        "workspace": event.workspace,
        "cwd": event.cwd,
        "touched_files": event.touched_files,
        "text": content.normalized_body.as_deref(),
        "structured_content": content.structured_content.as_ref(),
        "content": {
            "complete": complete,
            "policy_status": policy_status,
            "policy_reason": policy_reason,
        },
    }))
}

fn event_window(
    index: &VerifiedIndex,
    selected: &CoreEventRecord,
    before: usize,
    after: usize,
    window: Option<usize>,
) -> Result<Vec<CoreEventRecord>> {
    // Bulk-Core integration seam: resolve the bounded sequence window from
    // metadata, then fetch only that window once the limited API is available.
    let events = index.core_events_for_session(selected.session_id.as_uuid())?;
    let position = events
        .iter()
        .position(|event| event.event_id == selected.event_id)
        .ok_or_else(|| anyhow!("selected event is absent from its pinned Core session"))?;
    let (before, after) = window
        .map(|window| (window, window))
        .unwrap_or((before, after));
    let start = position.saturating_sub(before);
    let end = position
        .saturating_add(after)
        .saturating_add(1)
        .min(events.len());
    Ok(events[start..end].to_vec())
}

fn select_session_events(
    events: &[CoreEventRecord],
    mode: TranscriptMode,
) -> Vec<&CoreEventRecord> {
    match mode {
        TranscriptMode::Log => events.iter().collect(),
        TranscriptMode::Full => events
            .iter()
            .filter(|event| {
                event.event_type == EventType::Message.as_str()
                    && matches!(event.role.as_deref(), Some("user" | "assistant" | "system"))
            })
            .collect(),
        TranscriptMode::Lite => {
            let mut selected = Vec::new();
            let mut pending_assistant = None;
            for event in events {
                if event.event_type != EventType::Message.as_str() {
                    continue;
                }
                match event.role.as_deref() {
                    Some("user") => {
                        if let Some(assistant) = pending_assistant.take() {
                            selected.push(assistant);
                        }
                        selected.push(event);
                    }
                    Some("assistant") => pending_assistant = Some(event),
                    _ => {}
                }
            }
            if let Some(assistant) = pending_assistant {
                selected.push(assistant);
            }
            selected
        }
    }
}
