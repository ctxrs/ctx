#[cfg(test)]
use anyhow::Result;
#[cfg(test)]
use ctx_history_index::CoreEventRecord;
use ctx_history_index::SessionRecord;
use serde_json::Value;

use crate::presentation_limit::PresentationOutputLimitError;
use crate::{output::OutputFormat, TranscriptMode};

pub(super) fn structured_format(
    format: OutputFormat,
) -> ctx_history_read_application::StructuredOutputFormat {
    match format {
        OutputFormat::Text => ctx_history_read_application::StructuredOutputFormat::Text,
        OutputFormat::Markdown => ctx_history_read_application::StructuredOutputFormat::Markdown,
        OutputFormat::Json => ctx_history_read_application::StructuredOutputFormat::Json,
        OutputFormat::Jsonl => ctx_history_read_application::StructuredOutputFormat::Jsonl,
    }
}

pub(super) fn structured_mode(
    mode: TranscriptMode,
) -> ctx_history_read_application::StructuredTranscriptMode {
    match mode {
        TranscriptMode::Full => ctx_history_read_application::StructuredTranscriptMode::Full,
        TranscriptMode::Lite => ctx_history_read_application::StructuredTranscriptMode::Lite,
        TranscriptMode::Log => ctx_history_read_application::StructuredTranscriptMode::Log,
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::source_index) fn session_transcript_value(
    session: &SessionRecord,
    mode: TranscriptMode,
    format: OutputFormat,
    rendered: Vec<Value>,
    truncated: bool,
    max_events: Option<usize>,
    provenance: &ctx_history_read_application::ArchiveProvenanceMap,
) -> Value {
    ctx_history_read_application::session_transcript_read_model(
        session,
        structured_mode(mode),
        structured_format(format),
        rendered,
        truncated,
        max_events,
        provenance,
    )
}

#[cfg(test)]
pub(in crate::source_index) fn event_window_value(
    selected: &CoreEventRecord,
    format: OutputFormat,
    rendered: Vec<Value>,
) -> Result<Value> {
    ctx_history_read_application::event_window_value(selected, structured_format(format), rendered)
}

#[cfg(test)]
pub(in crate::source_index) fn render_event_values(
    events: &[&CoreEventRecord],
    output_limit_bytes: usize,
) -> Result<Vec<Value>> {
    ctx_history_read_application::render_event_read_model_values(
        events,
        output_limit_bytes,
        &Default::default(),
    )
    .map_err(map_read_model_error)
}

#[cfg(test)]
pub(in crate::source_index) fn render_event_value(event: &CoreEventRecord) -> Value {
    ctx_history_read_application::render_show_event_read_model(event, &Default::default())
}

pub(super) fn map_read_model_error(error: anyhow::Error) -> anyhow::Error {
    match error.downcast::<ctx_history_read_application::ReadModelLimitError>() {
        Ok(error) => anyhow::Error::new(PresentationOutputLimitError {
            event_id: error.event_id,
            actual_bytes: error.actual_bytes,
            maximum_bytes: error.maximum_bytes,
        }),
        Err(error) => error,
    }
}
