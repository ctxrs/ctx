//! Agent-owned invocation for Core MCP tools.

use ctx_agent_integrations::tool_backend::{
    ToolExecutionError, ToolOperation, ToolOutcome, ToolUsageFacts,
};
use serde_json::json;

use crate::tool_backend::{
    HistoryReadOutcome, HistoryReadPort, SearchReadOutcome, SearchReadinessPort, SourceCatalog,
    SourceCatalogPort,
};

/// Invokes one parsed MCP tool operation through coarse host ports. Each branch
/// performs at most one call to the owning operation port.
pub fn invoke_mcp_tool_call<H, R, S>(
    operation: ToolOperation,
    history: &H,
    search: &R,
    sources: &S,
) -> Result<ToolOutcome, ToolExecutionError>
where
    H: HistoryReadPort,
    R: SearchReadinessPort,
    S: SourceCatalogPort,
{
    let invocation_usage = operation.invocation_usage();
    execute(operation, history, search, sources).map_err(|mut failure| {
        failure.usage.merge(invocation_usage);
        failure
    })
}

fn execute<H, R, S>(
    operation: ToolOperation,
    history: &H,
    search: &R,
    sources: &S,
) -> Result<ToolOutcome, ToolExecutionError>
where
    H: HistoryReadPort,
    R: SearchReadinessPort,
    S: SourceCatalogPort,
{
    match operation {
        ToolOperation::Status => history.status().map(ToolOutcome::plain).map_err(Into::into),
        ToolOperation::Sources => sources
            .source_catalog()
            .map(source_catalog_outcome)
            .map_err(Into::into),
        ToolOperation::Search(request) => search
            .search_ready(request)
            .map(search_outcome)
            .map_err(Into::into),
        ToolOperation::ShowSession(request) => history
            .show_session(request)
            .map(history_outcome)
            .map_err(Into::into),
        ToolOperation::ShowEvent(request) => history
            .show_event(request)
            .map(history_outcome)
            .map_err(Into::into),
        ToolOperation::QueryEvents(request) => history
            .query_events(request)
            .map(ToolOutcome::plain)
            .map_err(Into::into),
    }
}

fn source_catalog_outcome(catalog: SourceCatalog) -> ToolOutcome {
    ToolOutcome::plain(json!({
        "schema_version": 1,
        "automatic_discovery": catalog.automatic_discovery,
        "sources": catalog.sources,
        "issues": catalog.issues,
        "issues_truncated": catalog.issues_truncated,
        "read_only": true,
    }))
}

fn search_outcome(result: SearchReadOutcome) -> ToolOutcome {
    ToolOutcome {
        structured: result.structured,
        compact: Some(result.compact),
        usage: ToolUsageFacts {
            search: Some(result.usage),
        },
    }
}

fn history_outcome(result: HistoryReadOutcome) -> ToolOutcome {
    ToolOutcome::with_compact(result.structured, result.compact)
}

#[cfg(test)]
mod tests;
