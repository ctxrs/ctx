use std::path::Path;

use ctx_history_core::{MAX_CORE_CONTENT_BYTES, MAX_ENCODED_CORE_RECORD_BYTES};
use ctx_history_index::CoreEventPageBudget;
use ctx_history_read_application::{
    execute_show_event, execute_show_session_page, EventWindowBudget, ShowEventApplicationRequest,
    ShowEventRequest, ShowSessionApplicationRequest,
};
use serde_json::Value;
use uuid::Uuid;

use crate::{output::OutputFormat, TranscriptMode};

use super::{
    render, session_event_mode, show_event_application_error, ShowApplicationError,
    ShowApplicationResult, CLI_SESSION_EVENT_PAGE_ITEMS,
};
use crate::source_index::{
    open_generation_read, render::enforce_json_output_limit, shared::externalize_query_error,
};

#[cfg(test)]
pub(crate) fn mcp_show_session(
    data_root: &Path,
    id: &str,
    mode: TranscriptMode,
    limit: usize,
    cursor: Option<&str>,
    output_limit_bytes: usize,
) -> anyhow::Result<Value> {
    mcp_show_session_with_compact(data_root, id, mode, limit, cursor, output_limit_bytes)
        .map(|(value, _)| value)
}

#[cfg(test)]
pub(crate) fn mcp_show_session_with_compact(
    data_root: &Path,
    id: &str,
    mode: TranscriptMode,
    limit: usize,
    cursor: Option<&str>,
    output_limit_bytes: usize,
) -> anyhow::Result<(Value, Value)> {
    mcp_show_session_application(data_root, id, mode, limit, cursor, output_limit_bytes)
        .map_err(ShowApplicationError::into_cli_error)
}

pub fn mcp_show_session_application(
    data_root: &Path,
    id: &str,
    mode: TranscriptMode,
    limit: usize,
    cursor: Option<&str>,
    output_limit_bytes: usize,
) -> ShowApplicationResult<(Value, Value)> {
    let mut generation = |read: &ctx_history_read_application::GenerationReadRequest| {
        open_generation_read(data_root, read)
    };
    let result = execute_show_session_page(
        ShowSessionApplicationRequest {
            selector: Some(id.to_owned()),
            provider_session_id: None,
            provider: None,
            provider_key: None,
            source_id: None,
            mode: session_event_mode(mode),
            cursor: cursor.map(str::to_owned),
            limit,
            page_items: CLI_SESSION_EVENT_PAGE_ITEMS,
            page_budget: CoreEventPageBudget::new(
                output_limit_bytes.clamp(1, MAX_ENCODED_CORE_RECORD_BYTES),
                output_limit_bytes.clamp(1, MAX_CORE_CONTENT_BYTES),
            ),
            compact_projection: true,
        },
        &mut generation,
    )
    .map_err(show_event_application_error)
    .map_err(externalize_query_error)
    .map_err(ShowApplicationError::from_application_error)?;
    let session_id = result.page().session.session_id.as_uuid();
    let rendered = result
        .into_read_models(
            render::structured_mode(mode),
            render::structured_format(OutputFormat::Json),
            limit,
            output_limit_bytes,
        )
        .map_err(externalize_query_error)
        .map_err(ShowApplicationError::from_application_error)?;
    let value = rendered.value;
    let event_id = value["events"]
        .as_array()
        .and_then(|events| events.last())
        .and_then(|event| event["ctx_event_id"].as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
        .unwrap_or(session_id);
    enforce_json_output_limit(&value, output_limit_bytes, event_id)
        .map_err(ShowApplicationError::from_application_error)?;
    let compact_value = rendered
        .compact_value
        .ok_or_else(|| ShowApplicationError::application("compact show projection was omitted"))?;
    Ok((value, compact_value))
}

#[cfg(test)]
pub(crate) fn mcp_show_event(
    data_root: &Path,
    id: &str,
    before: usize,
    after: usize,
    window: Option<usize>,
    output_limit_bytes: usize,
) -> anyhow::Result<Value> {
    mcp_show_event_with_compact(data_root, id, before, after, window, output_limit_bytes)
        .map(|(value, _)| value)
}

#[cfg(test)]
pub(crate) fn mcp_show_event_with_compact(
    data_root: &Path,
    id: &str,
    before: usize,
    after: usize,
    window: Option<usize>,
    output_limit_bytes: usize,
) -> anyhow::Result<(Value, Value)> {
    mcp_show_event_application(data_root, id, before, after, window, output_limit_bytes)
        .map_err(ShowApplicationError::into_cli_error)
}

pub fn mcp_show_event_application(
    data_root: &Path,
    id: &str,
    before: usize,
    after: usize,
    window: Option<usize>,
    output_limit_bytes: usize,
) -> ShowApplicationResult<(Value, Value)> {
    let mut generation = |read: &ctx_history_read_application::GenerationReadRequest| {
        open_generation_read(data_root, read)
    };
    let result = execute_show_event(
        ShowEventApplicationRequest {
            request: ShowEventRequest {
                selector: id.to_owned(),
                before,
                after,
                window,
                budget: EventWindowBudget {
                    maximum_events: ctx_history_index::MAX_SESSION_EVENT_COORDINATE_WINDOW_ITEMS,
                    maximum_encoded_core_bytes: MAX_ENCODED_CORE_RECORD_BYTES,
                    maximum_content_bytes: output_limit_bytes,
                },
            },
            generation_target: ctx_history_read_application::GenerationReadTarget::Active,
            compact_projection: true,
        },
        &mut generation,
    )
    .map_err(show_event_application_error)
    .map_err(externalize_query_error)
    .map_err(ShowApplicationError::from_application_error)?;
    let selected_event_id = result.result().selected.event_id.as_uuid();
    let value = result
        .read_model(
            render::structured_format(OutputFormat::Json),
            output_limit_bytes,
        )
        .map_err(ShowApplicationError::from_application_error)?;
    enforce_json_output_limit(&value, output_limit_bytes, selected_event_id)
        .map_err(ShowApplicationError::from_application_error)?;
    let compact_value = result
        .project_read_model(&value)
        .map_err(ShowApplicationError::from_application_error)?;
    Ok((value, compact_value))
}
