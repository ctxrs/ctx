use std::{
    collections::VecDeque,
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use ctx_history_core::{
    CaptureProvider, CoreContentPolicyStatus, EventType, MAX_CORE_CONTENT_BYTES,
    MAX_ENCODED_CORE_RECORD_BYTES,
};
use ctx_history_index::{
    CoreEventPageBudget, CoreEventRecord, SessionRecord, VerifiedIndex,
    MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS, MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
};
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
        open_index, render_active_generation_race, resolve_core_event, resolve_lookup_for_output,
        resolve_session, validate_ctx_id, validate_session_selector, ActiveGenerationRaceCommand,
    },
};

const CLI_PRESENTATION_MAX_SESSION_EVENTS: usize = MAX_SESSION_EVENT_COORDINATE_PREFIX_ITEMS - 1;
const CORE_PRESENTATION_FETCH_MAX_EVENTS: usize = 200;
const PRESENTATION_MAX_EVENT_WINDOW_EVENTS: usize = MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PresentationEventLimitError {
    actual_events: usize,
    maximum_events: usize,
}

impl fmt::Display for PresentationEventLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Core presentation selected at least {} events; the presentation limit is {} events",
            self.actual_events, self.maximum_events
        )
    }
}

impl std::error::Error for PresentationEventLimitError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EncodedCorePresentationLimitError {
    pub(super) event_id: Uuid,
    pub(super) actual_bytes: usize,
    pub(super) maximum_bytes: usize,
}

impl fmt::Display for EncodedCorePresentationLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "stored Core encoding through ctx event {} requires {} bytes; the presentation retention limit is {} bytes",
            self.event_id, self.actual_bytes, self.maximum_bytes
        )
    }
}

impl std::error::Error for EncodedCorePresentationLimitError {}

#[cfg(test)]
thread_local! {
    static CORE_PRESENTATION_FETCH_IDS: std::cell::RefCell<Vec<Uuid>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

#[cfg(test)]
pub(super) fn take_core_presentation_fetch_ids() -> Vec<Uuid> {
    CORE_PRESENTATION_FETCH_IDS.with(|ids| std::mem::take(&mut *ids.borrow_mut()))
}

pub(crate) fn run_show(
    args: ShowArgs,
    data_root: PathBuf,
    telemetry: &mut ShowTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let json_output = matches!(
        &args.target,
        ShowTarget::Event(args) if args.format == OutputFormat::Json
    ) || matches!(
        &args.target,
        ShowTarget::Session(args) if args.format == OutputFormat::Json
    );
    let result = run_show_inner(args, data_root, telemetry, local_usage, ui);
    render_show_error(result, json_output, ui)
}

fn run_show_inner(
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
            let selected = resolve_lookup_for_output(
                resolve_core_event(&index, &args.id),
                args.format == OutputFormat::Text,
                r#"ctx search "<query>" --verbose"#,
                ui,
            )?;
            let events = event_window(
                &index,
                &selected,
                args.before,
                args.after,
                args.window,
                CLI_PRESENTATION_MAX_OUTPUT_BYTES,
            )?;
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
            let human_output = args.format == OutputFormat::Text && args.out.is_none();
            let session = resolve_lookup_for_output(
                resolve_show_session(
                    &index,
                    args.id.as_deref(),
                    args.provider_session.as_deref(),
                    args.provider.map(ProviderArg::capture_provider),
                ),
                human_output,
                r#"ctx search "<query>" --verbose"#,
                ui,
            )?;
            let value = session_json(
                &index,
                &session,
                SessionJsonOptions {
                    mode: args.mode,
                    format: args.format,
                    max_events: args.max_events,
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

pub(super) fn render_show_error<T>(result: Result<T>, json_output: bool, ui: &mut Ui) -> Result<T> {
    render_active_generation_race(result, json_output, ActiveGenerationRaceCommand::Show, ui)
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
    let events = event_window(&index, &selected, before, after, window, output_limit_bytes)?;
    let value = event_window_json(&selected, &events, OutputFormat::Json, output_limit_bytes)?;
    enforce_json_output_limit(&value, output_limit_bytes, selected.event_id.as_uuid())?;
    Ok(value)
}

pub(super) struct SessionJsonOptions {
    pub(super) mode: TranscriptMode,
    pub(super) format: OutputFormat,
    pub(super) max_events: Option<usize>,
    pub(super) output_limit_bytes: usize,
}

pub(super) fn session_json(
    index: &VerifiedIndex,
    session: &SessionRecord,
    options: SessionJsonOptions,
) -> Result<Value> {
    session_json_with_event_cap(index, session, options, CLI_PRESENTATION_MAX_SESSION_EVENTS)
}

pub(super) fn session_json_with_event_cap(
    index: &VerifiedIndex,
    session: &SessionRecord,
    options: SessionJsonOptions,
    absolute_maximum_events: usize,
) -> Result<Value> {
    let absolute_maximum_events = absolute_maximum_events.min(CLI_PRESENTATION_MAX_SESSION_EVENTS);
    let maximum_events = options
        .max_events
        .unwrap_or(absolute_maximum_events)
        .min(absolute_maximum_events);
    let coordinate_limit = maximum_events.saturating_add(1);
    let mut coordinates =
        index.session_event_coordinate_prefix(session.session_id.as_uuid(), coordinate_limit)?;
    let truncated = coordinates.len() > maximum_events;
    if options.max_events.is_none() && coordinates.len() > maximum_events {
        return Err(anyhow::Error::new(PresentationEventLimitError {
            actual_events: coordinates.len(),
            maximum_events,
        }));
    }
    coordinates.truncate(maximum_events);
    let selected_ids = coordinates
        .iter()
        .map(|coordinate| coordinate.event_id)
        .collect::<Vec<_>>();
    let events = core_events_by_ids_with_presentation_budget(
        index,
        &selected_ids,
        maximum_events,
        options.output_limit_bytes,
    )?;
    let selected = select_session_events(&events, options.mode);
    let rendered = render_event_values(&selected, options.output_limit_bytes)?;
    Ok(session_transcript_value(
        session,
        options.mode,
        options.format,
        rendered,
        truncated,
        options.max_events.map(|_| maximum_events),
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
        CoreContentPolicyStatus::Redacted { reason } => ("redacted", Some(reason.as_str()), false),
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

pub(super) fn event_window(
    index: &VerifiedIndex,
    selected: &CoreEventRecord,
    before: usize,
    after: usize,
    window: Option<usize>,
    output_limit_bytes: usize,
) -> Result<Vec<CoreEventRecord>> {
    let (before, after) = window
        .map(|window| (window, window))
        .unwrap_or((before, after));
    let coordinates = index
        .session_event_coordinate_window(
            selected.session_id.as_uuid(),
            selected.event_id.as_uuid(),
            before,
            after,
        )?
        .ok_or_else(|| anyhow!("selected event is absent from its pinned Core session"))?;
    let selected_ids = coordinates
        .iter()
        .map(|coordinate| coordinate.event_id)
        .collect::<Vec<_>>();
    core_events_by_ids_with_presentation_budget(
        index,
        &selected_ids,
        PRESENTATION_MAX_EVENT_WINDOW_EVENTS,
        output_limit_bytes,
    )
}

fn core_events_by_ids_with_presentation_budget(
    index: &VerifiedIndex,
    event_ids: &[Uuid],
    maximum_events: usize,
    output_limit_bytes: usize,
) -> Result<Vec<CoreEventRecord>> {
    core_events_by_ids_with_presentation_limits(
        index,
        event_ids,
        maximum_events,
        output_limit_bytes,
        MAX_ENCODED_CORE_RECORD_BYTES,
    )
}

pub(super) fn core_events_by_ids_with_presentation_limits(
    index: &VerifiedIndex,
    event_ids: &[Uuid],
    maximum_events: usize,
    output_limit_bytes: usize,
    encoded_core_limit_bytes: usize,
) -> Result<Vec<CoreEventRecord>> {
    if event_ids.len() > maximum_events {
        return Err(anyhow::Error::new(PresentationEventLimitError {
            actual_events: event_ids.len(),
            maximum_events,
        }));
    }

    let mut pending = VecDeque::new();
    for chunk in event_ids.chunks(CORE_PRESENTATION_FETCH_MAX_EVENTS) {
        pending.push_back(chunk);
    }
    let mut events = Vec::with_capacity(event_ids.len());
    let mut retained_content_bytes = 0_usize;
    let mut retained_encoded_core_bytes = 0_usize;
    while let Some(ids) = pending.pop_front() {
        let remaining_content_bytes = output_limit_bytes
            .saturating_sub(retained_content_bytes)
            .clamp(1, MAX_CORE_CONTENT_BYTES);
        let remaining_encoded_core_bytes = encoded_core_limit_bytes
            .saturating_sub(retained_encoded_core_bytes)
            .clamp(1, MAX_ENCODED_CORE_RECORD_BYTES);
        let budget =
            CoreEventPageBudget::new(remaining_encoded_core_bytes, remaining_content_bytes);
        #[cfg(test)]
        CORE_PRESENTATION_FETCH_IDS.with(|fetched| {
            fetched.borrow_mut().extend_from_slice(ids);
        });
        match index.core_events_by_ids_with_budget(
            ids,
            CORE_PRESENTATION_FETCH_MAX_EVENTS,
            budget,
        )? {
            Some(batch) => {
                let event_id = ids.last().copied().unwrap_or_else(Uuid::nil);
                let actual_encoded_core_bytes =
                    retained_encoded_core_bytes.saturating_add(batch.encoded_core_bytes);
                if actual_encoded_core_bytes > encoded_core_limit_bytes {
                    return Err(anyhow::Error::new(EncodedCorePresentationLimitError {
                        event_id,
                        actual_bytes: actual_encoded_core_bytes,
                        maximum_bytes: encoded_core_limit_bytes,
                    }));
                }
                retained_encoded_core_bytes = actual_encoded_core_bytes;
                let actual_bytes = retained_content_bytes.saturating_add(batch.content_bytes);
                enforce_presentation_output_limit(actual_bytes, output_limit_bytes, event_id)?;
                retained_content_bytes = actual_bytes;
                events.extend(batch.items);
            }
            None if ids.len() > 1 => {
                let middle = ids.len() / 2;
                let (left, right) = ids.split_at(middle);
                pending.push_front(right);
                pending.push_front(left);
            }
            None => {
                return Err(anyhow!(
                    "pinned Core generation could not resolve event {} within the remaining {} encoded-byte and {} content-byte presentation budgets",
                    ids[0],
                    encoded_core_limit_bytes.saturating_sub(retained_encoded_core_bytes),
                    output_limit_bytes.saturating_sub(retained_content_bytes),
                ));
            }
        }
    }
    if events.len() != event_ids.len()
        || events
            .iter()
            .zip(event_ids)
            .any(|(event, expected)| event.event_id.as_uuid() != *expected)
    {
        return Err(anyhow!(
            "pinned Core generation did not return the exact requested presentation order"
        ));
    }
    Ok(events)
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
