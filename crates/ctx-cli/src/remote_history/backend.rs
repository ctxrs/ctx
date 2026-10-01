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

    /// Facts are returned with this execution, never stored as a global last
    /// result. The MCP carrier adds its own final response/write outcome.
    pub(crate) fn execute_with_facts(
        &self,
        operation: ToolOperation,
    ) -> (
        Result<ToolOutcome, ToolExecutionError>,
        super::RemoteCompletion,
    ) {
        let kind = match &operation {
            ToolOperation::Status => super::RemoteOperation::Status,
            ToolOperation::Search(_) => super::RemoteOperation::Search,
            ToolOperation::ShowEvent(_) => super::RemoteOperation::Event,
            ToolOperation::ShowSession(_) => super::RemoteOperation::Session,
            _ => super::RemoteOperation::Unsupported,
        };
        let mut observed = super::Observation::new(kind);
        let result = self.perform(operation, &mut observed).and_then(|value| {
            observed.stage = super::RemoteStage::Render;
            let text = super::render::tool_text(&value)?;
            let mut outcome = ToolOutcome::plain(value);
            outcome.text = Some(text);
            if let Some(count) = observed.facts.returned {
                observed.rendered(count);
            }
            observed.stage = super::RemoteStage::Complete;
            Ok(outcome)
        });
        // Classify before flattening the typed source error for the tool API.
        let terminal = observed.completion(result.as_ref().err());
        let result =
            result.map_err(|error| ToolBackendError::invalid_request(error.to_string()).into());
        (result, terminal)
    }

    fn perform(
        &self,
        operation: ToolOperation,
        observed: &mut super::Observation,
    ) -> Result<Value> {
        match operation {
            ToolOperation::Status => {
                let status = observed.request(|| self.client.status())?;
                observed.status(&status);
                observed.stage = super::RemoteStage::Render;
                Ok(json!({"scope":"shared", "status": status}))
            },
            ToolOperation::Search(request) => {
                super::validation::tool_search(&request)?;
                let response = observed.request(|| self.client.search(&request.query, request.limit))?;
                observed.search(&response, request.limit);
                observed.stage = super::RemoteStage::Render;
                Ok(serde_json::to_value(response)?)
            }
            ToolOperation::ShowEvent(request) => {
                ensure!(request.before == 0 && request.after == 0 && request.window.is_none_or(|n| n == 0), "shared event show returns the exact cited event; use its session citation for surrounding events");
                super::validation::citation(&request.selector, CitationKind::Event, &self.client.connection().collection)?;
                let event = observed.request(|| self.client.event(&request.selector))?;
                observed.facts.returned = Some(1);
                observed.stage = super::RemoteStage::Render;
                bounded(&event, request.output_limit_bytes, observed)
            }
            ToolOperation::ShowSession(request) => {
                ensure!(request.mode == ToolTranscriptMode::Log, "shared sessions support mode=log only; lite/full transcript selection is local-only");
                ensure!((1..=100).contains(&request.limit), "shared session page limit must be 1..100");
                super::validation::citation(&request.selector, CitationKind::Session, &self.client.connection().collection)?;
                let page = observed.request(|| self.client.session(&request.selector, request.cursor.as_deref(), request.limit))?;
                observed.facts.limit = Some(request.limit as u64);
                observed.page(&page, request.cursor.is_some());
                observed.stage = super::RemoteStage::Render;
                bounded(&page, request.output_limit_bytes, observed)
            }
            _ => anyhow::bail!("this server connection supports status, search, show_event, and show_session; this tool remains local-only"),
        }
    }
}

fn bounded(
    value: &impl Serialize,
    maximum: usize,
    observed: &mut super::Observation,
) -> Result<Value> {
    let bytes = serde_json::to_vec(value)?;
    observed.facts.encoded_bytes = Some(bytes.len() as u64);
    ensure!(bytes.len() <= maximum, "shared response exceeds output_limit_bytes; request a smaller page or increase the output limit");
    Ok(serde_json::from_slice(&bytes)?)
}

impl ToolBackend for RemoteBackend {
    fn history_tool_surface(&self) -> ctx_agent_integrations::mcp::HistoryToolSurface {
        ctx_agent_integrations::mcp::HistoryToolSurface::RemoteLog
    }

    fn execute(&self, operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        let (mut result, terminal) = self.execute_with_facts(operation);
        let remote = crate::engine_telemetry::remote(terminal, true);
        match &mut result {
            Ok(outcome) => outcome.usage.remote = Some(remote),
            Err(error) => error.usage.remote = Some(remote),
        }
        result
    }

    fn parse_provider(&self, value: &str) -> Option<CaptureProvider> {
        crate::provider_args::ProviderArg::parse_name(value)
            .map(crate::provider_args::ProviderArg::capture_provider)
    }

    fn provider_names(&self) -> Vec<&'static str> {
        crate::provider_args::ProviderArg::mcp_names()
    }
}
