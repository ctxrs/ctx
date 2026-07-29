use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_index::{EventRecord, SessionRecord, VerifiedIndex};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    analytics::{count_bucket, ShowTelemetry},
    complete_content::{ContentPolicy, CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES},
    local_usage::{CliUsage, ResultObservationAction},
    output::{compact_json, OutputFormat},
    provider_args::ProviderArg,
    semantic::PinnedSourceBackedGeneration,
    transcript::TranscriptMode,
    ShowArgs, ShowTarget,
};

use super::{
    render::{enforce_json_output_limit, timestamp_json, write_show_value},
    shared::{
        event_source_json, open_index, resolve_event, resolve_session, session_source_json,
        source_path_exists, validate_ctx_id, validate_session_selector,
    },
};

#[derive(Debug)]
pub(super) struct ResolvedIndexContent {
    pub(super) text: String,
}

pub(crate) fn run_show(
    args: ShowArgs,
    data_root: PathBuf,
    telemetry: &mut ShowTelemetry,
    local_usage: &mut CliUsage,
) -> Result<()> {
    validate_show_target(&args.target)?;
    let index = open_index(&data_root)?;
    match args.target {
        ShowTarget::Event(args) => {
            let selected = resolve_event(&index, &args.id)?;
            let events = event_window(&index, &selected, args.before, args.after, args.window)?;
            telemetry.events_returned = Some(count_bucket(events.len() as u64));
            let value = event_window_json(
                &index,
                &data_root,
                &selected,
                &events,
                args.content,
                args.format,
                CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
            )?;
            let events = value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let result_count = events.len();
            let content_bytes = serde_json::to_vec(&value["events"])?.len();
            let output_bytes =
                write_show_value(value, args.format, None, selected.event_id.as_uuid())?;
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
                &data_root,
                &session,
                args.mode,
                args.content,
                args.format,
                None,
                CLI_COMPLETE_CONTENT_MAX_OUTPUT_BYTES,
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
            let output_bytes = write_show_value(value, args.format, args.out, event_id)?;
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
                "source-backed session lookup requires a ctx session ID or --provider-session"
            ));
        }
    };
    if let Some(provider) = provider {
        if session.provider != provider.as_str() {
            return Err(anyhow!(
                "source-backed session {} belongs to provider {}, not {}",
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
            "provider session {provider_session_id:?} was not found in the source-backed Core generation"
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
    content: ContentPolicy,
    max_events: usize,
    output_limit_bytes: usize,
) -> Result<Value> {
    let index = open_index(data_root)?;
    let session = resolve_session(&index, id)?;
    let value = session_json(
        &index,
        data_root,
        &session,
        mode,
        content,
        OutputFormat::Json,
        Some(max_events),
        output_limit_bytes,
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
    content: ContentPolicy,
    output_limit_bytes: usize,
) -> Result<Value> {
    let index = open_index(data_root)?;
    let selected = resolve_event(&index, id)?;
    let events = event_window(&index, &selected, before, after, window)?;
    let value = event_window_json(
        &index,
        data_root,
        &selected,
        &events,
        content,
        OutputFormat::Json,
        output_limit_bytes,
    )?;
    enforce_json_output_limit(&value, output_limit_bytes, selected.event_id.as_uuid())?;
    Ok(value)
}

fn session_json(
    index: &VerifiedIndex,
    data_root: &Path,
    session: &SessionRecord,
    mode: TranscriptMode,
    content: ContentPolicy,
    format: OutputFormat,
    max_events: Option<usize>,
    output_limit_bytes: usize,
) -> Result<Value> {
    let mut events = index.events_for_session(session.session_id.as_uuid())?;
    let source = session_source_json(session, events.first());
    let truncated = max_events.is_some_and(|limit| events.len() > limit);
    if let Some(limit) = max_events {
        events.truncate(limit);
    }
    let selected = select_session_events(&events, mode);
    let rendered = render_event_values(index, data_root, &selected, content, output_limit_bytes)?;
    Ok(session_transcript_value(
        session, mode, content, format, source, rendered, truncated, max_events,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn session_transcript_value(
    session: &SessionRecord,
    mode: TranscriptMode,
    content: ContentPolicy,
    format: OutputFormat,
    source: Value,
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
        "content_policy": content.as_str(),
        "format": format.as_str(),
        "session": {
            "id": session.session_id.as_uuid(),
            "item_id": session.session_id.as_uuid(),
            "record_type": "session",
            "ctx_session_id": session.session_id.as_uuid(),
            "provider": session.provider,
            "provider_session_id": session.provider_session_id,
            "source_format": session.source_format,
            "source_path": session.source_path,
            "parent_ctx_session_id": session.parent_session_id.map(|id| id.as_uuid()),
            "root_ctx_session_id": session.root_session_id.as_uuid(),
            "branch": session.branch,
            "agent_type": session.agent_type,
            "is_primary": session.is_primary,
            "workspace": session.workspace,
            "cwd": session.cwd,
            "source_exists": source_path_exists(session.source_path.as_deref()),
        },
        "source": source,
        "events": rendered,
        "truncated": truncated.then(|| json!({
            "events": true,
            "max_events": max_events,
        })),
    }))
}

fn event_window_json(
    index: &VerifiedIndex,
    data_root: &Path,
    selected: &EventRecord,
    events: &[EventRecord],
    content: ContentPolicy,
    format: OutputFormat,
    output_limit_bytes: usize,
) -> Result<Value> {
    let references = events.iter().collect::<Vec<_>>();
    let rendered = render_event_values(index, data_root, &references, content, output_limit_bytes)?;
    event_window_value(selected, content, format, rendered)
}

pub(super) fn event_window_value(
    selected: &EventRecord,
    content: ContentPolicy,
    format: OutputFormat,
    rendered: Vec<Value>,
) -> Result<Value> {
    let selected_value = rendered
        .iter()
        .find(|event| {
            event["ctx_event_id"].as_str() == Some(&selected.event_id.as_uuid().to_string())
        })
        .cloned()
        .ok_or_else(|| anyhow!("selected source-backed event is absent from its event window"))?;
    Ok(compact_json(json!({
        "schema_version": 1,
        "target": "event",
        "payload_type": "event_window",
        "ctx_event_id": selected.event_id.as_uuid(),
        "ctx_session_id": selected.session_id.as_uuid(),
        "content_policy": content.as_str(),
        "format": format.as_str(),
        "event": selected_value,
        "source": event_source_json(selected),
        "events": rendered,
    })))
}

fn render_event_values(
    index: &VerifiedIndex,
    data_root: &Path,
    events: &[&EventRecord],
    policy: ContentPolicy,
    output_limit_bytes: usize,
) -> Result<Vec<Value>> {
    let resolved = resolve_contents(index, data_root, events, output_limit_bytes)?;
    events
        .iter()
        .zip(resolved)
        .map(|(event, resolved)| Ok(render_event_value(event, resolved.text, policy)))
        .collect()
}

pub(super) fn render_event_value(
    event: &EventRecord,
    text: String,
    policy: ContentPolicy,
) -> Value {
    compact_json(json!({
        "ctx_event_id": event.event_id.as_uuid(),
        "item_id": event.event_id.as_uuid(),
        "record_type": "event",
        "ctx_session_id": event.session_id.as_uuid(),
        "provider": event.provider,
        "provider_session_id": event.provider_session_id,
        "source_format": event.source_format,
        "source_path": event.source_path,
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
        "source_id": event.locator.source().identity().as_uuid(),
        "source_exists": source_path_exists(event.source_path.as_deref()),
        "source": event_source_json(event),
        "text": text,
        "content": {
            "requested": policy.as_str(),
            "complete": true,
            "origin": "provider_source",
            "stored_truncated": false,
            "source_verified": true,
            "complete_content_available": true,
        },
    }))
}

fn resolve_contents(
    index: &VerifiedIndex,
    data_root: &Path,
    events: &[&EventRecord],
    output_limit_bytes: usize,
) -> Result<Vec<ResolvedIndexContent>> {
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let hydrated =
        PinnedSourceBackedGeneration::hydrate_source_complete_events(index, data_root, events)?;
    resolved_contents_from_map(events, output_limit_bytes, hydrated)
}

fn resolved_contents_from_map(
    events: &[&EventRecord],
    output_limit_bytes: usize,
    mut hydrated: HashMap<Uuid, String>,
) -> Result<Vec<ResolvedIndexContent>> {
    let mut output_bytes = 0usize;
    let mut resolved = Vec::with_capacity(events.len());
    for event in events {
        let text = hydrated
            .remove(&event.event_id.as_uuid())
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "generation-bound source hydration omitted complete event {}",
                    event.event_id
                )
            })?;
        output_bytes = output_bytes.saturating_add(text.len());
        if output_bytes > output_limit_bytes {
            return Err(anyhow!(
                "source-backed complete content exceeds the {output_limit_bytes}-byte output limit at event {}",
                event.event_id
            ));
        }
        resolved.push(ResolvedIndexContent { text });
    }
    if !hydrated.is_empty() {
        return Err(anyhow!(
            "generation-bound source hydration returned unrequested events"
        ));
    }
    Ok(resolved)
}

#[cfg(test)]
pub(super) fn resolve_complete_contents(
    events: &[&EventRecord],
    output_limit_bytes: usize,
    resolver: &dyn ctx_history_core::ContentSourceResolver,
) -> Result<Vec<ResolvedIndexContent>> {
    use ctx_history_core::{BatchHydrationRequest, EventHydrationRequest};

    let requests = events
        .iter()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let request = BatchHydrationRequest::new(requests)?;
    let result = resolver.hydrate_batch(&request).map_err(|failure| {
        anyhow!(
            "hydrate ordered generation-bound source batch: {:?}: {}",
            failure.kind,
            failure.detail
        )
    })?;
    result
        .validate_for_request(&request)
        .map_err(|failure| anyhow!("validate generation-bound source batch: {}", failure.detail))?;
    let mut hydrated = HashMap::with_capacity(events.len());
    for (event, record) in events.iter().zip(result.into_records()) {
        let text = String::from_utf8(record.provider_bytes).map_err(|error| {
            anyhow!(
                "provider registry returned non-UTF-8 exact content for {} event {}: {}",
                event.provider,
                event.event_id,
                error.utf8_error()
            )
        })?;
        if hydrated.insert(event.event_id.as_uuid(), text).is_some() {
            return Err(anyhow!(
                "generation-bound source batch duplicated event {}",
                event.event_id
            ));
        }
    }
    resolved_contents_from_map(events, output_limit_bytes, hydrated)
}

fn event_window(
    index: &VerifiedIndex,
    selected: &EventRecord,
    before: usize,
    after: usize,
    window: Option<usize>,
) -> Result<Vec<EventRecord>> {
    let events = index.events_for_session(selected.session_id.as_uuid())?;
    let position = events
        .iter()
        .position(|event| event.event_id == selected.event_id)
        .ok_or_else(|| anyhow!("selected source-backed event is absent from its session"))?;
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

fn select_session_events(events: &[EventRecord], mode: TranscriptMode) -> Vec<&EventRecord> {
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
