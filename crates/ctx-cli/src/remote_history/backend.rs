use anyhow::{ensure, Result};
use ctx_agent_integrations::tool_backend::{
    ToolBackend, ToolBackendError, ToolExecutionError, ToolOperation, ToolOutcome,
    ToolTranscriptMode,
};
use ctx_history_core::CaptureProvider;
use ctx_history_server::CitationKind;
use ctx_history_sharing::RemoteClient;
use serde::Serialize;
use serde_json::{json, Value};

pub(crate) struct RemoteBackend {
    client: RemoteClient,
}

impl RemoteBackend {
    pub(crate) fn new(client: RemoteClient) -> Self {
        Self { client }
    }

    fn perform(&self, operation: ToolOperation) -> Result<Value> {
        match operation {
            ToolOperation::Status => Ok(json!({"scope":"shared", "status": self.client.status()?})),
            ToolOperation::Search(request) => {
                super::validation::tool_search(&request)?;
                Ok(serde_json::to_value(self.client.search(&request.query, request.limit)?)?)
            }
            ToolOperation::ShowEvent(request) => {
                ensure!(request.before == 0 && request.after == 0 && request.window.is_none_or(|n| n == 0), "shared event show returns the exact cited event; use its session citation for surrounding events");
                super::validation::citation(&request.selector, CitationKind::Event, &self.client.connection().collection)?;
                bounded(&self.client.event(&request.selector)?, request.output_limit_bytes)
            }
            ToolOperation::ShowSession(request) => {
                ensure!(request.mode == ToolTranscriptMode::Log, "shared sessions support mode=log only; lite/full transcript selection is local-only");
                ensure!((1..=100).contains(&request.limit), "shared session page limit must be 1..100");
                super::validation::citation(&request.selector, CitationKind::Session, &self.client.connection().collection)?;
                bounded(&self.client.session(&request.selector, request.cursor.as_deref(), request.limit)?, request.output_limit_bytes)
            }
            _ => anyhow::bail!("this server connection supports status, search, show_event, and show_session; this tool remains local-only"),
        }
    }
}

fn bounded(value: &impl Serialize, maximum: usize) -> Result<Value> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= maximum, "shared response exceeds output_limit_bytes; request a smaller page or increase the output limit");
    Ok(serde_json::from_slice(&bytes)?)
}

impl ToolBackend for RemoteBackend {
    fn history_tool_surface(&self) -> ctx_agent_integrations::mcp::HistoryToolSurface {
        ctx_agent_integrations::mcp::HistoryToolSurface::RemoteLog
    }

    fn execute(&self, operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        self.perform(operation)
            .and_then(|value| {
                let text = super::render::tool_text(&value)?;
                let mut outcome = ToolOutcome::plain(value);
                outcome.text = Some(text);
                Ok(outcome)
            })
            .map_err(|error| {
                // The client exposes closed errors, never remote response bodies.
                ToolBackendError::invalid_request(error.to_string()).into()
            })
    }

    fn parse_provider(&self, value: &str) -> Option<CaptureProvider> {
        crate::provider_args::ProviderArg::parse_name(value)
            .map(crate::provider_args::ProviderArg::capture_provider)
    }

    fn provider_names(&self) -> Vec<&'static str> {
        crate::provider_args::ProviderArg::mcp_names()
    }
}
