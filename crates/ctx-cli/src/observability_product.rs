//! Product and presentation mappings retained in the CLI composition root.

use ctx_client_observability::{
    analytics::{
        ImportSourceMode, ImportTelemetry, ProgressMode, RenderFormat, TranscriptModeKind,
    },
    local_usage::{McpCompletionFacts, McpToolUsageFacts, SearchContextObservation},
};
use serde_json::Value;

pub(crate) const fn progress_mode(value: crate::progress::ProgressArg) -> ProgressMode {
    match value {
        crate::progress::ProgressArg::Auto => ProgressMode::Auto,
        crate::progress::ProgressArg::Plain => ProgressMode::Plain,
        crate::progress::ProgressArg::Json => ProgressMode::Json,
        crate::progress::ProgressArg::None => ProgressMode::None,
    }
}

pub(crate) fn import_telemetry(args: &crate::ImportArgs) -> ImportTelemetry {
    ImportTelemetry {
        resume: args.resume,
        all_sources: args.all,
        no_daemon: args.no_daemon,
        source_mode: if args.input_format.is_some() {
            ImportSourceMode::ExplicitFormat
        } else if args.history_source.is_some() || !args.history_source_manifest.is_empty() {
            ImportSourceMode::HistorySourcePlugin
        } else if args.path.is_some() {
            ImportSourceMode::ExplicitPath
        } else if args.all {
            ImportSourceMode::AllDiscovered
        } else if args.provider.is_some() {
            ImportSourceMode::DiscoveredProvider
        } else {
            ImportSourceMode::AutoDiscovered
        },
        provider_filter: args.provider.map(|provider| provider.capture_provider()),
        reset_cursor: args.reset_cursor,
        progress_mode: progress_mode(args.progress),
        sources_seen: None,
        source_bytes: None,
        source_files: None,
        failed_sources: None,
        sessions_imported: None,
        events_imported: None,
        edges_imported: None,
        skipped: None,
        rejected_records: None,
        outcome: None,
        failure_scope: None,
        failure_type: None,
    }
}

pub(crate) fn setup_import_telemetry(
    progress: crate::progress::ProgressArg,
    no_daemon: bool,
) -> ImportTelemetry {
    ImportTelemetry {
        resume: false,
        all_sources: true,
        no_daemon,
        source_mode: ImportSourceMode::AllDiscovered,
        provider_filter: None,
        reset_cursor: false,
        progress_mode: progress_mode(progress),
        sources_seen: None,
        source_bytes: None,
        source_files: None,
        failed_sources: None,
        sessions_imported: None,
        events_imported: None,
        edges_imported: None,
        skipped: None,
        rejected_records: None,
        outcome: None,
        failure_scope: None,
        failure_type: None,
    }
}

pub(crate) const fn render_format(value: crate::output::OutputFormat) -> RenderFormat {
    match value {
        crate::output::OutputFormat::Text => RenderFormat::Text,
        crate::output::OutputFormat::Json => RenderFormat::Json,
        crate::output::OutputFormat::Jsonl => RenderFormat::Jsonl,
        crate::output::OutputFormat::Markdown => RenderFormat::Markdown,
    }
}

pub(crate) const fn json_render_format(value: crate::output::JsonOutputFormat) -> RenderFormat {
    match value {
        crate::output::JsonOutputFormat::Text => RenderFormat::Text,
        crate::output::JsonOutputFormat::Json => RenderFormat::Json,
    }
}

pub(crate) const fn transcript_mode(
    value: crate::transcript::TranscriptMode,
) -> TranscriptModeKind {
    match value {
        crate::transcript::TranscriptMode::Lite => TranscriptModeKind::Lite,
        crate::transcript::TranscriptMode::Full => TranscriptModeKind::Full,
        crate::transcript::TranscriptMode::Log => TranscriptModeKind::Log,
    }
}

pub(crate) fn mcp_tool_usage(usage: crate::tool_backend::ToolUsageFacts) -> McpToolUsageFacts {
    let search_context = usage.search.map(|search| {
        if search.context_complete {
            SearchContextObservation::complete(
                usize::try_from(search.delivered_context_bytes).unwrap_or(usize::MAX),
                usize::try_from(search.matched_normalized_session_bytes).unwrap_or(usize::MAX),
            )
            .unwrap_or_else(SearchContextObservation::unavailable)
        } else {
            SearchContextObservation::unavailable()
        }
    });
    McpToolUsageFacts { search_context }
}

pub(crate) fn mcp_completion_facts(
    operation: crate::operation_descriptor::ObservedMcpProductOperation,
    response: &Value,
    delivered_output_bytes: usize,
) -> McpCompletionFacts {
    let failed = response.get("error").is_some()
        || response.pointer("/result/isError").and_then(Value::as_bool) == Some(true);
    let structured = response.pointer("/result/structuredContent");
    let result_count = structured.and_then(|structured| {
        let field = match operation {
            crate::operation_descriptor::ObservedMcpProductOperation::Sources => "sources",
            crate::operation_descriptor::ObservedMcpProductOperation::Search => "results",
            crate::operation_descriptor::ObservedMcpProductOperation::ShowSession
            | crate::operation_descriptor::ObservedMcpProductOperation::ShowEvent
            | crate::operation_descriptor::ObservedMcpProductOperation::QueryEvents => "events",
            _ => return None,
        };
        structured
            .get(field)
            .and_then(Value::as_array)
            .map(Vec::len)
    });
    McpCompletionFacts {
        failed,
        result_count,
        delivered_output_bytes,
    }
}
