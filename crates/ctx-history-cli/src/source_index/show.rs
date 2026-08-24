mod mcp;
mod render;

use std::{fmt, io::Write, path::PathBuf};

use anyhow::{anyhow, Result};
use ctx_history_core::{MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES};
use ctx_history_index::{
    CoreEventPageBudget, CoreEventRecord, IndexError, SessionRecord,
    MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
};
use ctx_history_read_application::{
    execute_show_event, execute_show_session_stream, EventWindowBudget, GenerationReadError,
    ShowEventApplicationRequest, ShowEventRequest, ShowReadApplicationError,
    ShowReadModelProjection, ShowSessionStreamCallback, ShowSessionStreamControl,
    ShowSessionStreamPage, ShowSessionStreamRequest, ShowSessionStreamStart,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    analytics::{count_bucket, ShowTelemetry},
    cli::{ShowArgs, ShowTarget},
    local_usage::{CliUsage, ResultObservationAction},
    output::{compact_json, OutputFormat},
    presentation_limit::{
        enforce_presentation_output_limit, PresentationOutputLimitError,
        CLI_PRESENTATION_MAX_OUTPUT_BYTES,
    },
    provider_args::ProviderArg,
    transcript::TranscriptOutput,
    ui::{canonical_human_output_bytes, RenderContext, Ui},
    TranscriptMode,
};

use super::{
    open_generation_read,
    render::{follow_up_command_prefix, render_show_document, write_show_value},
    shared::{
        externalize_query_error, render_active_generation_race, resolve_lookup_for_output,
        validate_ctx_id, validate_session_selector, ActiveGenerationRaceCommand,
    },
};

#[cfg(test)]
pub(crate) use mcp::{mcp_show_event, mcp_show_session};
pub use mcp::{mcp_show_event_application, mcp_show_session_application};
#[cfg(test)]
pub(super) use render::{event_window_value, render_event_values};
pub(super) use render::{render_event_value, session_transcript_value};

const CLI_SESSION_EVENT_PAGE_ITEMS: usize = ctx_history_read_application::SHOW_SESSION_PAGE_ITEMS;
const PRESENTATION_MAX_EVENT_WINDOW_EVENTS: usize = MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS;

/// Typed failures exposed by the transport-neutral show application boundary.
#[derive(Debug, thiserror::Error)]
pub enum ShowApplicationError {
    #[error(
        "History changed while ctx was opening the searchable generation. Retry the same request."
    )]
    GenerationChanged,
    #[error(transparent)]
    GenerationAuthority(ctx_history_refresh::GenerationQueryAuthorityError),
    #[error("{detail}")]
    CursorStale { detail: String },
    #[error("{detail}")]
    CursorMismatch { detail: String },
    #[error("{detail}")]
    InvalidCursor { detail: String },
    #[error(
        "Core content output for ctx event {event_id} requires {actual_bytes} bytes; the presentation limit is {maximum_bytes} bytes"
    )]
    OutputLimit {
        event_id: Uuid,
        actual_bytes: usize,
        maximum_bytes: usize,
    },
    #[error("{detail}")]
    Application { detail: String },
}

impl ShowApplicationError {
    pub(super) fn from_index(error: IndexError) -> Self {
        Self::from_index_ref(&error)
    }

    fn from_index_ref(error: &IndexError) -> Self {
        let detail = error.to_string();
        match error {
            IndexError::ConcurrentGenerationChange => Self::GenerationChanged,
            IndexError::SessionEventCursorGenerationMismatch { .. } => Self::CursorStale { detail },
            IndexError::SessionEventCursorSessionMismatch => Self::CursorMismatch { detail },
            IndexError::InvalidSessionEventCursorSessionIdentity
            | IndexError::InvalidSessionEventCursorCoordinate => Self::InvalidCursor { detail },
            _ => Self::Application { detail },
        }
    }

    pub(super) fn application(error: impl fmt::Display) -> Self {
        Self::Application {
            detail: error.to_string(),
        }
    }

    pub(super) fn from_application_error(error: anyhow::Error) -> Self {
        let error = match error.downcast::<IndexError>() {
            Ok(error) => return Self::from_index(error),
            Err(error) => error,
        };
        let error = match error.downcast::<ctx_history_refresh::GenerationQueryAuthorityError>() {
            Ok(error) => return Self::GenerationAuthority(error),
            Err(error) => error,
        };
        let error = match error.downcast::<ctx_history_read_application::ReadModelLimitError>() {
            Ok(error) => {
                return Self::OutputLimit {
                    event_id: error.event_id,
                    actual_bytes: error.actual_bytes,
                    maximum_bytes: error.maximum_bytes,
                }
            }
            Err(error) => error,
        };
        match error.downcast::<PresentationOutputLimitError>() {
            Ok(error) => Self::from(error),
            Err(error) => Self::application(error),
        }
    }

    #[cfg(test)]
    fn into_cli_error(self) -> anyhow::Error {
        match self {
            Self::GenerationChanged => anyhow::Error::new(IndexError::ConcurrentGenerationChange),
            Self::GenerationAuthority(error) => anyhow::Error::new(error),
            Self::CursorStale { .. } => {
                anyhow::Error::new(IndexError::SessionEventCursorGenerationMismatch {
                    cursor_generation: "stale".to_owned(),
                    pinned_generation: "current".to_owned(),
                })
            }
            Self::CursorMismatch { .. } => {
                anyhow::Error::new(IndexError::SessionEventCursorSessionMismatch)
            }
            Self::InvalidCursor { .. } => {
                anyhow::Error::new(IndexError::InvalidSessionEventCursorCoordinate)
            }
            Self::OutputLimit {
                event_id,
                actual_bytes,
                maximum_bytes,
            } => anyhow::Error::new(PresentationOutputLimitError {
                event_id,
                actual_bytes,
                maximum_bytes,
            }),
            Self::Application { detail } => anyhow!(detail),
        }
    }
}

impl From<IndexError> for ShowApplicationError {
    fn from(error: IndexError) -> Self {
        Self::from_index(error)
    }
}

impl From<PresentationOutputLimitError> for ShowApplicationError {
    fn from(error: PresentationOutputLimitError) -> Self {
        Self::OutputLimit {
            event_id: error.event_id,
            actual_bytes: error.actual_bytes,
            maximum_bytes: error.maximum_bytes,
        }
    }
}

pub(super) type ShowApplicationResult<T> = std::result::Result<T, ShowApplicationError>;

pub fn run_show(
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
    match args.target {
        ShowTarget::Event(args) => {
            let compact_projection =
                matches!(args.format, OutputFormat::Text | OutputFormat::Markdown);
            let mut generation = |read: &ctx_history_read_application::GenerationReadRequest| {
                open_generation_read(&data_root, read)
            };
            let result = resolve_lookup_for_output(
                execute_show_event(
                    ShowEventApplicationRequest {
                        request: ShowEventRequest {
                            selector: args.id,
                            before: args.before,
                            after: args.after,
                            window: args.window,
                            budget: EventWindowBudget {
                                maximum_events: PRESENTATION_MAX_EVENT_WINDOW_EVENTS,
                                maximum_encoded_core_bytes: MAX_ENCODED_CORE_RECORD_BYTES,
                                maximum_content_bytes: CLI_PRESENTATION_MAX_OUTPUT_BYTES,
                            },
                        },
                        generation_target:
                            ctx_history_read_application::GenerationReadTarget::Active,
                        compact_projection,
                    },
                    &mut generation,
                )
                .map_err(show_event_application_error)
                .map_err(externalize_query_error),
                args.format == OutputFormat::Text,
                r#"ctx search "<query>" --verbose"#,
                ui,
            )?;
            let selected = &result.result().selected;
            telemetry.events_returned = Some(count_bucket(result.result().events.len() as u64));
            let value = result
                .read_model(
                    render::structured_format(args.format),
                    CLI_PRESENTATION_MAX_OUTPUT_BYTES,
                )
                .map_err(render::map_read_model_error)?;
            let events = value["events"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let result_count = events.len();
            let content_bytes = serde_json::to_vec(&value["events"])?.len();
            let compact_value = compact_projection
                .then(|| {
                    let mut projected = result.project_read_model(&value)?;
                    projected["_command_prefix"] =
                        Value::String(follow_up_command_prefix(&data_root));
                    Ok::<_, anyhow::Error>(projected)
                })
                .transpose()?;
            let output_value = compact_value.as_ref().unwrap_or(&value);
            let output_bytes = if args.format == OutputFormat::Text {
                write_show_document(output_value, selected.event_id.as_uuid(), ui)?
            } else {
                write_show_value(
                    compact_value.unwrap_or(value),
                    args.format,
                    None,
                    selected.event_id.as_uuid(),
                    ui.stdout_writer(),
                )?
            };
            local_usage.set_result_observation(
                ResultObservationAction::OpenEvent,
                result_count,
                content_bytes,
            );
            local_usage.set_measured_output_bytes(output_bytes);
            Ok(())
        }
        ShowTarget::Session(args) => {
            let human_output = args.format == OutputFormat::Text && args.out.is_none();
            let result = resolve_lookup_for_output(
                stream_cli_session(
                    &data_root,
                    args.id,
                    args.provider_session,
                    args.provider.map(ProviderArg::capture_provider),
                    args.provider_key,
                    args.source_id,
                    args.mode,
                    args.format,
                    args.max_events,
                    args.out,
                    ui,
                )
                .map_err(externalize_query_error),
                human_output,
                r#"ctx search "<query>" --verbose"#,
                ui,
            )?;
            telemetry.events_returned = Some(count_bucket(result.events_returned as u64));
            local_usage.set_result_observation(
                ResultObservationAction::OpenSession,
                result.events_returned,
                result.content_bytes,
            );
            local_usage.set_measured_output_bytes(result.output_bytes);
            Ok(())
        }
    }
}

fn show_read_application_error<StreamError>(
    error: ShowReadApplicationError<anyhow::Error, StreamError>,
) -> anyhow::Error
where
    StreamError: Into<anyhow::Error>,
{
    match error {
        ShowReadApplicationError::Generation(GenerationReadError::Port(error))
        | ShowReadApplicationError::Query(error) => error,
        ShowReadApplicationError::Generation(GenerationReadError::Authority(error)) => {
            anyhow::Error::new(error)
        }
        ShowReadApplicationError::Stream(error) => error.into(),
    }
}

fn show_event_application_error(error: ShowReadApplicationError<anyhow::Error>) -> anyhow::Error {
    match error {
        ShowReadApplicationError::Generation(GenerationReadError::Port(error))
        | ShowReadApplicationError::Query(error) => error,
        ShowReadApplicationError::Generation(GenerationReadError::Authority(error)) => {
            anyhow::Error::new(error)
        }
        ShowReadApplicationError::Stream(error) => match error {},
    }
}

pub(super) fn render_show_error<T>(result: Result<T>, json_output: bool, ui: &mut Ui) -> Result<T> {
    render_active_generation_race(result, json_output, ActiveGenerationRaceCommand::Show, ui)
}

pub(super) struct SessionStreamResult {
    pub(super) events_returned: usize,
    pub(super) content_bytes: usize,
    pub(super) output_bytes: usize,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn stream_cli_session(
    data_root: &std::path::Path,
    selector: Option<String>,
    provider_session_id: Option<String>,
    provider: Option<ctx_history_core::CaptureProvider>,
    provider_key: Option<String>,
    source_id: Option<String>,
    mode: TranscriptMode,
    format: OutputFormat,
    max_events: Option<usize>,
    out: Option<PathBuf>,
    ui: &mut Ui,
) -> Result<SessionStreamResult> {
    let writes_stdout = out.is_none();
    let human_context =
        (format == OutputFormat::Text && writes_stdout).then(|| *ui.stdout_context());
    let mut stream = CliSessionStream {
        out,
        stdout: Some(ui.stdout_writer()),
        mode,
        format,
        max_events,
        human_context,
        writes_stdout,
        renderer: None,
    };
    let mut generation = |read: &ctx_history_read_application::GenerationReadRequest| {
        open_generation_read(data_root, read)
    };
    let summary = execute_show_session_stream(
        ShowSessionStreamRequest {
            selector,
            provider_session_id,
            provider,
            provider_key,
            source_id,
            mode: session_event_mode(mode),
            cursor: None,
            max_events,
            page_items: CLI_SESSION_EVENT_PAGE_ITEMS,
            page_budget: CoreEventPageBudget::new(
                MAX_ENCODED_CORE_RECORD_BYTES,
                MAX_CORE_CONTENT_BYTES,
            ),
            compact_projection: matches!(format, OutputFormat::Text | OutputFormat::Markdown),
        },
        &mut generation,
        &mut stream,
    )
    .map_err(show_read_application_error)?;
    stream.finish(summary.truncated)
}

struct CliSessionStream<'a> {
    out: Option<PathBuf>,
    stdout: Option<&'a mut (dyn Write + Send)>,
    mode: TranscriptMode,
    format: OutputFormat,
    max_events: Option<usize>,
    human_context: Option<RenderContext>,
    writes_stdout: bool,
    renderer: Option<SessionStreamRenderer<'a>>,
}

impl CliSessionStream<'_> {
    fn finish(&mut self, truncated: bool) -> Result<SessionStreamResult> {
        self.renderer
            .take()
            .ok_or_else(|| anyhow!("session stream did not receive its start model"))?
            .finish(truncated, self.max_events)
    }
}

impl<'a> ShowSessionStreamCallback for CliSessionStream<'a> {
    type Error = anyhow::Error;

    fn start(&mut self, start: ShowSessionStreamStart<'_>) -> Result<()> {
        let stdout = self
            .stdout
            .take()
            .ok_or_else(|| anyhow!("session stream output was already opened"))?;
        let output = TranscriptOutput::create(self.out.take(), stdout)?;
        self.renderer = Some(SessionStreamRenderer::new(
            output,
            start.session,
            self.mode,
            self.format,
            self.max_events,
            self.human_context,
            self.writes_stdout,
            start.projection,
        )?);
        Ok(())
    }

    fn page(&mut self, page: ShowSessionStreamPage<'_>) -> Result<ShowSessionStreamControl> {
        let renderer = self
            .renderer
            .as_mut()
            .ok_or_else(|| anyhow!("session stream page preceded its start model"))?;
        renderer.begin_page();
        for selected in page.events {
            renderer.emit(selected.event, page.projection)?;
        }
        Ok(ShowSessionStreamControl::Continue)
    }
}

pub(super) const fn session_event_mode(
    mode: TranscriptMode,
) -> ctx_history_read_application::SessionEventMode {
    match mode {
        TranscriptMode::Full => ctx_history_read_application::SessionEventMode::Full,
        TranscriptMode::Lite => ctx_history_read_application::SessionEventMode::Lite,
        TranscriptMode::Log => ctx_history_read_application::SessionEventMode::Log,
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
        projection: ShowReadModelProjection<'_>,
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
        renderer.write_header(projection)?;
        Ok(renderer)
    }

    fn begin_page(&mut self) {
        self.page_output_bytes = 0;
    }

    fn emit(
        &mut self,
        event: CoreEventRecord,
        projection: ShowReadModelProjection<'_>,
    ) -> Result<()> {
        let event_id = event.event_id.as_uuid();
        let value = render_event_value(&event);
        let event_json = serde_json::to_vec(&value)?;
        self.content_bytes = self
            .content_bytes
            .saturating_add(usize::from(self.events_returned > 0))
            .saturating_add(event_json.len());
        let compact_value = projection.project(&value)?;
        if matches!(self.format, OutputFormat::Text | OutputFormat::Markdown)
            && compact_value.is_none()
        {
            return Err(anyhow!("human transcript rendering requires compact refs"));
        }
        let display_value = compact_value.as_ref().unwrap_or(&value);
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
                        "event": display_value,
                    }),
                    context,
                )
                .render(context)
                .into_bytes()
            }
            OutputFormat::Text => render_stream_text_event(display_value).into_bytes(),
            OutputFormat::Markdown => render_stream_markdown_event(display_value).into_bytes(),
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
                    "provider_key": self.metadata["provider_key"],
                    "source_id": self.metadata["source_id"],
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
                    "provider_key": self.metadata["provider_key"],
                    "source_id": self.metadata["source_id"],
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

    fn write_header(&mut self, projection: ShowReadModelProjection<'_>) -> Result<()> {
        let compact_metadata = projection.project(&self.metadata)?;
        if matches!(self.format, OutputFormat::Text | OutputFormat::Markdown)
            && compact_metadata.is_none()
        {
            return Err(anyhow!("human transcript rendering requires compact refs"));
        }
        let display_metadata = compact_metadata.as_ref().unwrap_or(&self.metadata);
        match self.format {
            OutputFormat::Text if self.human_context.is_some() => {
                let context = self
                    .human_context
                    .as_ref()
                    .ok_or_else(|| anyhow!("human transcript rendering requires a context"))?;
                let mut header = display_metadata.clone();
                header["_stream_part"] = Value::String("session_header".to_owned());
                self.output.write_all(
                    render_show_document(&header, context)
                        .render(context)
                        .as_bytes(),
                )?;
            }
            OutputFormat::Text => {
                self.output
                    .write_all(render_stream_text_header(display_metadata).as_bytes())?;
            }
            OutputFormat::Markdown => {
                self.output
                    .write_all(render_stream_markdown_header(display_metadata).as_bytes())?;
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
    if let Some(provider_key) = value["provider_key"].as_str() {
        output.push_str(&format!("provider_key: {provider_key}\n"));
    }
    if let Some(source_id) = value["source_id"].as_str() {
        output.push_str(&format!("source_id: {source_id}\n"));
    }
    if let Some(relationship) = value["session"]["session_relationship"].as_str() {
        output.push_str(&format!("session_relationship: {relationship}\n"));
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
    let mut output = format!(
        "[{}] {} {} {}\n",
        event["occurred_at"].as_str().unwrap_or("-"),
        role,
        event["event_type"].as_str().unwrap_or("event"),
        event["ctx_event_id"].as_str().unwrap_or("unknown"),
    );
    append_stream_event_copy(&mut output, event, "");
    append_stream_activity_text(&mut output, event, "");
    output.push_str(event["text"].as_str().unwrap_or_default());
    output.push_str("\n\n");
    output
}

fn render_stream_markdown_header(value: &Value) -> String {
    let mut output = format!(
        "# {} session {}\n\n- ctx_session_id: `{}`\n",
        value["provider"].as_str().unwrap_or("unknown"),
        value["provider_session_id"]
            .as_str()
            .or_else(|| value["ctx_session_id"].as_str())
            .unwrap_or("unknown"),
        value["ctx_session_id"].as_str().unwrap_or("unknown")
    );
    if let Some(provider_key) = value["provider_key"].as_str() {
        output.push_str(&format!("- provider_key: `{provider_key}`\n"));
    }
    if let Some(source_id) = value["source_id"].as_str() {
        output.push_str(&format!("- source_id: `{source_id}`\n"));
    }
    if let Some(relationship) = value["session"]["session_relationship"].as_str() {
        output.push_str(&format!("- session_relationship: `{relationship}`\n"));
    }
    output
}

fn render_stream_markdown_event(event: &Value) -> String {
    let role = event["role"]
        .as_str()
        .unwrap_or_else(|| event["event_type"].as_str().unwrap_or("event"));
    let mut output = format!(
        "\n## {} - {} - {}\n\nctx_event_id: `{}`\n\n",
        role,
        event["event_type"].as_str().unwrap_or("event"),
        event["occurred_at"].as_str().unwrap_or("-"),
        event["ctx_event_id"].as_str().unwrap_or("unknown"),
    );
    append_stream_event_copy(&mut output, event, "- ");
    append_stream_activity_markdown(&mut output, event, "- ");
    output.push_str(event["text"].as_str().unwrap_or_default());
    output.push('\n');
    output
}

fn append_stream_event_copy(output: &mut String, event: &Value, prefix: &str) {
    let copy = &event["event_copy"];
    let Some(ancestor_event_id) = copy["ancestor_ctx_event_id"].as_str() else {
        return;
    };
    for (label, key) in [
        ("ancestor_event_id", "ancestor_ctx_event_id"),
        ("ancestor_session_id", "ancestor_ctx_session_id"),
        ("copy_proof", "proof"),
    ] {
        if let Some(value) = copy[key].as_str() {
            output.push_str(&format!("{prefix}{label}: {value}\n"));
        }
    }
    debug_assert!(!ancestor_event_id.is_empty());
}

fn append_stream_activity_text(output: &mut String, event: &Value, prefix: &str) {
    if let Some(activity) = event.get("activity").filter(|value| !value.is_null()) {
        output.push_str(&format!(
            "{prefix}activity: {}\n",
            super::render::safe_activity_json(activity)
        ));
    }
}

fn append_stream_activity_markdown(output: &mut String, event: &Value, prefix: &str) {
    if let Some(activity) = event.get("activity").filter(|value| !value.is_null()) {
        let activity = super::render::safe_activity_json(activity);
        output.push_str(&format!(
            "{prefix}activity: {}\n",
            super::render::markdown_code_span(&activity)
        ));
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
                .map_err(externalize_query_error)
        }
        ShowTarget::Event(args) => validate_ctx_id(&args.id, "event")
            .map(|_| ())
            .map_err(externalize_query_error),
    }
}

#[cfg(test)]
pub(super) fn resolve_show_session(
    index: &ctx_history_index::VerifiedIndex,
    id: Option<&str>,
    provider_session_id: Option<&str>,
    provider: Option<ctx_history_core::CaptureProvider>,
) -> Result<SessionRecord> {
    ctx_history_read_application::resolve_show_session(
        index,
        id,
        provider_session_id,
        provider,
        None,
        None,
    )
    .map_err(externalize_query_error)
}
