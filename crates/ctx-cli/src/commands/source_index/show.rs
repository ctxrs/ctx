use std::{
    collections::VecDeque,
    fmt,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ctx_history_core::{
    CaptureProvider, CoreContentPolicyStatus, EventType, MAX_CORE_CONTENT_BYTES,
    MAX_ENCODED_CORE_RECORD_BYTES,
};
use ctx_history_index::{
    CoreEventPageBudget, CoreEventRecord, SessionEventCoordinate, SessionEventCursor,
    SessionRecord, VerifiedIndex, MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
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
    transcript::{TranscriptMode, TranscriptOutput},
    ui::{canonical_human_output_bytes, RenderContext, Ui},
    ShowArgs, ShowTarget,
};

use super::{
    render::{enforce_json_output_limit, render_show_document, timestamp_json, write_show_value},
    shared::{
        open_index, resolve_core_event, resolve_lookup_for_output, resolve_session,
        validate_ctx_id, validate_session_selector,
    },
};

const CORE_PRESENTATION_FETCH_MAX_EVENTS: usize = 200;
const CLI_SESSION_EVENT_PAGE_ITEMS: usize = CORE_PRESENTATION_FETCH_MAX_EVENTS;
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
            let result = stream_cli_session(
                &index,
                &session,
                args.mode,
                args.format,
                args.max_events,
                args.out,
                ui,
            )?;
            telemetry.events_returned = Some(count_bucket(result.events_returned as u64));
            local_usage.set_result_observation(
                ResultObservationAction::OpenSession,
                result.events_returned,
                0,
                result.content_bytes,
            );
            local_usage.set_measured_output_bytes(result.output_bytes);
            Ok(())
        }
    }
}

pub(super) struct SessionStreamResult {
    pub(super) events_returned: usize,
    pub(super) content_bytes: usize,
    pub(super) output_bytes: usize,
}

pub(super) fn stream_cli_session(
    index: &VerifiedIndex,
    session: &SessionRecord,
    mode: TranscriptMode,
    format: OutputFormat,
    max_events: Option<usize>,
    out: Option<PathBuf>,
    ui: &mut Ui,
) -> Result<SessionStreamResult> {
    let writes_stdout = out.is_none();
    let human_context =
        (format == OutputFormat::Text && writes_stdout).then(|| *ui.stdout_context());
    let output = TranscriptOutput::create(out, ui.stdout_writer())?;
    let mut renderer = SessionStreamRenderer::new(
        output,
        session,
        mode,
        format,
        max_events,
        human_context,
        writes_stdout,
    )?;
    let mut selector = SessionEventSelector::new(mode);
    let mut cursor: Option<SessionEventCursor> = None;
    let mut truncated = false;

    'pages: loop {
        renderer.begin_page();
        let page = index.core_session_event_page_with_budget(
            session.session_id.as_uuid(),
            cursor.as_ref(),
            CLI_SESSION_EVENT_PAGE_ITEMS,
            CoreEventPageBudget::new(MAX_ENCODED_CORE_RECORD_BYTES, MAX_CORE_CONTENT_BYTES),
        )?;
        let terminal = page.terminal;
        let next_cursor = page.next_cursor;
        for event in page.items {
            for selected in selector.push(event) {
                if max_events.is_some_and(|maximum| renderer.events_returned() >= maximum) {
                    truncated = true;
                    break 'pages;
                }
                renderer.emit(selected)?;
            }
        }
        if terminal {
            if let Some(selected) = selector.finish() {
                if max_events.is_some_and(|maximum| renderer.events_returned() >= maximum) {
                    truncated = true;
                } else {
                    renderer.emit(selected)?;
                }
            }
            break;
        }
        cursor = Some(next_cursor.ok_or_else(|| {
            anyhow!("nonterminal Core session event page omitted its continuation cursor")
        })?);
    }

    renderer.finish(truncated, max_events)
}

struct SessionEventSelector {
    mode: TranscriptMode,
    pending_assistant: Option<CoreEventRecord>,
}

impl SessionEventSelector {
    const fn new(mode: TranscriptMode) -> Self {
        Self {
            mode,
            pending_assistant: None,
        }
    }

    fn push(&mut self, event: CoreEventRecord) -> Vec<CoreEventRecord> {
        match self.mode {
            TranscriptMode::Log => vec![event],
            TranscriptMode::Full => {
                if event.event_type == EventType::Message.as_str()
                    && matches!(event.role.as_deref(), Some("user" | "assistant" | "system"))
                {
                    vec![event]
                } else {
                    Vec::new()
                }
            }
            TranscriptMode::Lite => {
                if event.event_type != EventType::Message.as_str() {
                    return Vec::new();
                }
                match event.role.as_deref() {
                    Some("user") => {
                        let mut selected = Vec::with_capacity(2);
                        if let Some(assistant) = self.pending_assistant.take() {
                            selected.push(assistant);
                        }
                        selected.push(event);
                        selected
                    }
                    Some("assistant") => {
                        self.pending_assistant = Some(event);
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            }
        }
    }

    fn finish(&mut self) -> Option<CoreEventRecord> {
        self.pending_assistant.take()
    }
}

struct SessionStreamRenderer<'a> {
    output: TranscriptOutput<'a>,
    metadata: Value,
    format: OutputFormat,
    human_context: Option<RenderContext>,
    writes_stdout: bool,
    events_returned: usize,
    content_bytes: usize,
    page_output_bytes: usize,
    last_event_id: Uuid,
}

impl<'a> SessionStreamRenderer<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        output: TranscriptOutput<'a>,
        session: &SessionRecord,
        mode: TranscriptMode,
        format: OutputFormat,
        max_events: Option<usize>,
        human_context: Option<RenderContext>,
        writes_stdout: bool,
    ) -> Result<Self> {
        let metadata =
            session_transcript_value(session, mode, format, Vec::new(), false, max_events);
        let mut renderer = Self {
            output,
            metadata,
            format,
            human_context,
            writes_stdout,
            events_returned: 0,
            content_bytes: 2,
            page_output_bytes: 0,
            last_event_id: session.session_id.as_uuid(),
        };
        renderer.write_header()?;
        Ok(renderer)
    }

    const fn events_returned(&self) -> usize {
        self.events_returned
    }

    fn begin_page(&mut self) {
        self.page_output_bytes = 0;
    }

    fn emit(&mut self, event: CoreEventRecord) -> Result<()> {
        let event_id = event.event_id.as_uuid();
        let value = render_event_value(&event);
        let event_json = serde_json::to_vec(&value)?;
        self.content_bytes = self
            .content_bytes
            .saturating_add(usize::from(self.events_returned > 0))
            .saturating_add(event_json.len());
        let fragment = match self.format {
            OutputFormat::Text if self.human_context.is_some() => {
                let context = self
                    .human_context
                    .as_ref()
                    .ok_or_else(|| anyhow!("human transcript rendering requires a context"))?;
                render_show_document(
                    &json!({
                        "_stream_part": "session_event",
                        "position": self.events_returned.saturating_add(1),
                        "event": value,
                    }),
                    context,
                )
                .render(context)
                .into_bytes()
            }
            OutputFormat::Text => render_stream_text_event(&value).into_bytes(),
            OutputFormat::Markdown => render_stream_markdown_event(&value).into_bytes(),
            OutputFormat::Json => {
                let mut fragment = Vec::with_capacity(event_json.len().saturating_add(1));
                if self.events_returned > 0 {
                    fragment.push(b',');
                }
                fragment.extend(event_json);
                fragment
            }
            OutputFormat::Jsonl => {
                let line = compact_json(json!({
                    "schema_version": 1,
                    "payload_type": "session_transcript_event",
                    "mode": self.metadata["mode"],
                    "ctx_session_id": self.metadata["ctx_session_id"],
                    "provider": self.metadata["provider"],
                    "provider_session_id": self.metadata["provider_session_id"],
                    "event": value,
                }));
                let mut fragment = serde_json::to_vec(&line)?;
                fragment.push(b'\n');
                fragment
            }
        };
        let actual_page_bytes = self.page_output_bytes.saturating_add(fragment.len());
        enforce_presentation_output_limit(
            actual_page_bytes,
            CLI_PRESENTATION_MAX_OUTPUT_BYTES,
            event_id,
        )?;
        self.output.write_all(&fragment)?;
        self.page_output_bytes = actual_page_bytes;
        self.events_returned = self.events_returned.saturating_add(1);
        self.last_event_id = event_id;
        Ok(())
    }

    fn finish(mut self, truncated: bool, max_events: Option<usize>) -> Result<SessionStreamResult> {
        if self.events_returned == 0 {
            if let Some(context) = self.human_context.as_ref() {
                self.output.write_all(
                    render_show_document(&json!({"_stream_part": "session_empty"}), context)
                        .render(context)
                        .as_bytes(),
                )?;
            }
        }
        let max_events_u64 = max_events.map(|maximum| u64::try_from(maximum).unwrap_or(u64::MAX));
        match self.format {
            OutputFormat::Text if self.human_context.is_some() && truncated => {
                let context = self
                    .human_context
                    .as_ref()
                    .ok_or_else(|| anyhow!("human transcript rendering requires a context"))?;
                self.output.write_all(
                    render_show_document(
                        &json!({
                            "_stream_part": "session_truncated",
                            "max_events": max_events_u64,
                        }),
                        context,
                    )
                    .render(context)
                    .as_bytes(),
                )?;
            }
            OutputFormat::Text if truncated => {
                self.output.write_all(
                    format!(
                        "transcript_truncated: true\nmax_events: {}\n",
                        max_events.unwrap_or(self.events_returned)
                    )
                    .as_bytes(),
                )?;
            }
            OutputFormat::Markdown if truncated => {
                self.output.write_all(
                    format!(
                        "\n> Transcript is truncated after {} events.\n",
                        max_events.unwrap_or(self.events_returned)
                    )
                    .as_bytes(),
                )?;
            }
            OutputFormat::Json => {
                self.output.write_all(b"]")?;
                if truncated {
                    self.output.write_all(b",\"truncated\":")?;
                    serde_json::to_writer(
                        &mut self.output,
                        &json!({"events": true, "max_events": max_events}),
                    )?;
                }
                self.output.write_all(b"}")?;
                if self.writes_stdout {
                    self.output.write_all(b"\n")?;
                }
            }
            OutputFormat::Jsonl => {
                let completion = compact_json(json!({
                    "schema_version": 1,
                    "payload_type": "session_transcript_completion",
                    "mode": self.metadata["mode"],
                    "ctx_session_id": self.metadata["ctx_session_id"],
                    "provider": self.metadata["provider"],
                    "provider_session_id": self.metadata["provider_session_id"],
                    "events_returned": self.events_returned,
                    "complete": !truncated,
                    "truncated": truncated.then(|| json!({
                        "events": true,
                        "max_events": max_events,
                    })),
                }));
                serde_json::to_writer(&mut self.output, &completion)?;
                self.output.write_all(b"\n")?;
            }
            _ => {}
        }
        let events_returned = self.events_returned;
        let content_bytes = self.content_bytes;
        let output_bytes = self.output.finish()?;
        Ok(SessionStreamResult {
            events_returned,
            content_bytes,
            output_bytes,
        })
    }

    fn write_header(&mut self) -> Result<()> {
        match self.format {
            OutputFormat::Text if self.human_context.is_some() => {
                let context = self
                    .human_context
                    .as_ref()
                    .ok_or_else(|| anyhow!("human transcript rendering requires a context"))?;
                let mut header = self.metadata.clone();
                header["_stream_part"] = Value::String("session_header".to_owned());
                self.output.write_all(
                    render_show_document(&header, context)
                        .render(context)
                        .as_bytes(),
                )?;
            }
            OutputFormat::Text => {
                self.output
                    .write_all(render_stream_text_header(&self.metadata).as_bytes())?;
            }
            OutputFormat::Markdown => {
                self.output
                    .write_all(render_stream_markdown_header(&self.metadata).as_bytes())?;
            }
            OutputFormat::Json => write_stream_json_header(&mut self.output, &self.metadata)?,
            OutputFormat::Jsonl => {}
        }
        Ok(())
    }
}

fn write_stream_json_header(writer: &mut impl Write, metadata: &Value) -> Result<()> {
    writer.write_all(b"{")?;
    let object = metadata
        .as_object()
        .ok_or_else(|| anyhow!("session transcript metadata must be a JSON object"))?;
    let mut first = true;
    for (key, value) in object {
        if matches!(key.as_str(), "events" | "truncated") {
            continue;
        }
        if !first {
            writer.write_all(b",")?;
        }
        serde_json::to_writer(&mut *writer, key)?;
        writer.write_all(b":")?;
        serde_json::to_writer(&mut *writer, value)?;
        first = false;
    }
    if !first {
        writer.write_all(b",")?;
    }
    writer.write_all(b"\"events\":[")?;
    Ok(())
}

fn render_stream_text_header(value: &Value) -> String {
    let mut output = format!(
        "ctx_session_id: {}\nprovider: {}\n",
        value["ctx_session_id"].as_str().unwrap_or("unknown"),
        value["provider"].as_str().unwrap_or("unknown")
    );
    if let Some(provider_session_id) = value["provider_session_id"].as_str() {
        output.push_str(&format!("provider_session_id: {provider_session_id}\n"));
    }
    output.push_str(&format!(
        "mode: {}\nformat: text\n\n",
        value["mode"].as_str().unwrap_or("lite")
    ));
    output
}

fn render_stream_text_event(event: &Value) -> String {
    let role = event["role"]
        .as_str()
        .unwrap_or_else(|| event["event_type"].as_str().unwrap_or("event"));
    format!(
        "[{}] {} {} {}\n{}\n\n",
        event["occurred_at"].as_str().unwrap_or("-"),
        role,
        event["event_type"].as_str().unwrap_or("event"),
        event["ctx_event_id"].as_str().unwrap_or("unknown"),
        event["text"].as_str().unwrap_or_default()
    )
}

fn render_stream_markdown_header(value: &Value) -> String {
    format!(
        "# {} session {}\n\n- ctx_session_id: `{}`\n",
        value["provider"].as_str().unwrap_or("unknown"),
        value["provider_session_id"]
            .as_str()
            .or_else(|| value["ctx_session_id"].as_str())
            .unwrap_or("unknown"),
        value["ctx_session_id"].as_str().unwrap_or("unknown")
    )
}

fn render_stream_markdown_event(event: &Value) -> String {
    let role = event["role"]
        .as_str()
        .unwrap_or_else(|| event["event_type"].as_str().unwrap_or("event"));
    format!(
        "\n## {} - {} - {}\n\nctx_event_id: `{}`\n\n{}\n",
        role,
        event["event_type"].as_str().unwrap_or("event"),
        event["occurred_at"].as_str().unwrap_or("-"),
        event["ctx_event_id"].as_str().unwrap_or("unknown"),
        event["text"].as_str().unwrap_or_default()
    )
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
    limit: usize,
    cursor: Option<&str>,
    output_limit_bytes: usize,
) -> Result<Value> {
    let index = open_index(data_root)?;
    let session = resolve_session(&index, id)?;
    let cursor = cursor.map(decode_session_event_cursor).transpose()?;
    let (rendered, has_more, next_cursor) =
        collect_selected_session_page(&index, &session, mode, limit, cursor, output_limit_bytes)?;
    let returned = rendered.len();
    let mut value =
        session_transcript_value(&session, mode, OutputFormat::Json, rendered, false, None);
    value["pagination"] = compact_json(json!({
        "limit": limit,
        "returned": returned,
        "has_more": has_more,
        "next_cursor": next_cursor.as_ref().map(encode_session_event_cursor).transpose()?,
    }));
    let event_id = value["events"]
        .as_array()
        .and_then(|events| events.last())
        .and_then(|event| event["ctx_event_id"].as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
        .unwrap_or_else(|| session.session_id.as_uuid());
    enforce_json_output_limit(&value, output_limit_bytes, event_id)?;
    Ok(value)
}

fn collect_selected_session_page(
    index: &VerifiedIndex,
    session: &SessionRecord,
    mode: TranscriptMode,
    limit: usize,
    mut cursor: Option<SessionEventCursor>,
    output_limit_bytes: usize,
) -> Result<(Vec<Value>, bool, Option<SessionEventCursor>)> {
    let mut selector = SessionEventSelector::new(mode);
    let mut selected = Vec::with_capacity(limit);
    let mut serialized_event_bytes = 2_usize;
    let mut continuation = None;
    let mut has_more = false;

    'pages: loop {
        let page = index.core_session_event_page_with_budget(
            session.session_id.as_uuid(),
            cursor.as_ref(),
            CLI_SESSION_EVENT_PAGE_ITEMS,
            CoreEventPageBudget::new(
                output_limit_bytes.clamp(1, MAX_ENCODED_CORE_RECORD_BYTES),
                output_limit_bytes.clamp(1, MAX_CORE_CONTENT_BYTES),
            ),
        )?;
        let terminal = page.terminal;
        let next_page_cursor = page.next_cursor;
        for event in page.items {
            for event in selector.push(event) {
                if !retain_mcp_selected_event(
                    index,
                    session,
                    event,
                    limit,
                    output_limit_bytes,
                    &mut selected,
                    &mut serialized_event_bytes,
                    &mut continuation,
                )? {
                    has_more = true;
                    break 'pages;
                }
            }
        }
        if terminal {
            if let Some(event) = selector.finish() {
                if !retain_mcp_selected_event(
                    index,
                    session,
                    event,
                    limit,
                    output_limit_bytes,
                    &mut selected,
                    &mut serialized_event_bytes,
                    &mut continuation,
                )? {
                    has_more = true;
                }
            }
            break;
        }
        cursor = Some(next_page_cursor.ok_or_else(|| {
            anyhow!("nonterminal Core session event page omitted its continuation cursor")
        })?);
    }

    if !has_more {
        continuation = None;
    }
    Ok((selected, has_more, continuation))
}

#[allow(clippy::too_many_arguments)]
fn retain_mcp_selected_event(
    index: &VerifiedIndex,
    session: &SessionRecord,
    event: CoreEventRecord,
    limit: usize,
    output_limit_bytes: usize,
    selected: &mut Vec<Value>,
    serialized_event_bytes: &mut usize,
    continuation: &mut Option<SessionEventCursor>,
) -> Result<bool> {
    if selected.len() == limit {
        return Ok(false);
    }
    let event_id = event.event_id.as_uuid();
    let value = render_event_value(&event);
    let candidate_bytes = serialized_event_bytes
        .saturating_add(usize::from(!selected.is_empty()))
        .saturating_add(serialized_json_bytes(&value)?);
    if candidate_bytes > output_limit_bytes {
        if selected.is_empty() {
            enforce_presentation_output_limit(candidate_bytes, output_limit_bytes, event_id)?;
        }
        return Ok(false);
    }
    *serialized_event_bytes = candidate_bytes;
    *continuation = Some(cursor_after_event(index, session, &event));
    selected.push(value);
    Ok(true)
}

fn cursor_after_event(
    index: &VerifiedIndex,
    session: &SessionRecord,
    event: &CoreEventRecord,
) -> SessionEventCursor {
    SessionEventCursor::new(
        index.generation_id(),
        session.session_id,
        SessionEventCoordinate {
            event_id: event.event_id.as_uuid(),
            event_sequence: event.event_sequence,
            occurred_at_unix_ms: event.occurred_at_unix_ms,
        },
    )
}

fn encode_session_event_cursor(cursor: &SessionEventCursor) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor)?))
}

fn decode_session_event_cursor(encoded: &str) -> Result<SessionEventCursor> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        anyhow::Error::new(ctx_history_index::IndexError::InvalidSessionEventCursorCoordinate)
    })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::Error::new(ctx_history_index::IndexError::InvalidSessionEventCursorCoordinate)
    })
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
