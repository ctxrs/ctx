use std::time::Duration;

use ctx_agent_integrations::{
    mcp::{McpToolKind, RequestDescriptor},
    tool_backend::{
        ToolSearchBackend, ToolSearchFailurePhase, ToolSearchRefreshStatus, ToolSearchStopReason,
        ToolSearchTerminalFacts, ToolUsageFacts,
    },
};
use ctx_client_observability::{
    analytics::{
        McpErrorClassV1, McpResponseBoundV1, McpResultMetadataV1, McpStopReasonV1, Outcome,
        PublicEventV1, RefreshStatus, SearchBackend, SearchFailurePhase, SearchHealthFacts,
        SearchStopReason, SearchTerminalFacts,
    },
    mcp_observation::{
        McpDeliveredResponse, McpObservation, McpObservedTool, McpRequestObservation,
    },
    operation_descriptor::ObservedMcpProductOperation,
};
use serde_json::Value;

fn observed_operation(kind: McpToolKind) -> Option<ObservedMcpProductOperation> {
    match kind {
        McpToolKind::Status => Some(ObservedMcpProductOperation::Status),
        McpToolKind::Sources => Some(ObservedMcpProductOperation::Sources),
        McpToolKind::Search => Some(ObservedMcpProductOperation::Search),
        McpToolKind::ShowSession => Some(ObservedMcpProductOperation::ShowSession),
        McpToolKind::ShowEvent => Some(ObservedMcpProductOperation::ShowEvent),
        McpToolKind::QueryEvents => Some(ObservedMcpProductOperation::QueryEvents),
        McpToolKind::Blame => Some(ObservedMcpProductOperation::Blame),
        McpToolKind::Unknown | McpToolKind::Missing | McpToolKind::Unified(_) => None,
    }
}

fn request_observation(descriptor: RequestDescriptor) -> McpRequestObservation {
    match descriptor {
        RequestDescriptor::Initialize => McpRequestObservation::Initialize,
        RequestDescriptor::Ping => McpRequestObservation::Ping,
        RequestDescriptor::ToolsList => McpRequestObservation::ToolsList,
        RequestDescriptor::ToolCall { operation } => {
            McpRequestObservation::ToolCall(match observed_operation(operation) {
                Some(operation) => McpObservedTool::Product(operation),
                None if operation == McpToolKind::Unknown => McpObservedTool::Unknown,
                None => McpObservedTool::Missing,
            })
        }
        RequestDescriptor::UnknownRequest => McpRequestObservation::UnknownRequest,
        RequestDescriptor::MissingRequest => McpRequestObservation::MissingRequest,
        RequestDescriptor::InitializedNotification => {
            McpRequestObservation::InitializedNotification
        }
        RequestDescriptor::UnknownNotification => McpRequestObservation::UnknownNotification,
        RequestDescriptor::InvalidJson => McpRequestObservation::InvalidJson,
        RequestDescriptor::InvalidUtf8 => McpRequestObservation::InvalidUtf8,
        RequestDescriptor::LineTooLarge => McpRequestObservation::LineTooLarge,
    }
}

pub struct McpTelemetry {
    observation: Option<McpObservation>,
    remote: bool,
}

impl McpTelemetry {
    /// Starts telemetry only after the product has authorized it. The injected
    /// delivery port may re-check opt-out immediately before sending a batch.
    pub fn start(
        authorized: bool,
        dispatch: impl Fn(&[PublicEventV1]) -> Result<(), ()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            observation: authorized.then(|| McpObservation::start(dispatch)),
            remote: false,
        }
    }

    /// A remote reader has no local-history tool executions, including rejected
    /// requests that never reached its backend and therefore have no carrier.
    pub fn for_remote(mut self) -> Self {
        self.remote = true;
        self
    }

    pub fn record_delivered(
        &mut self,
        descriptor: RequestDescriptor,
        response: Option<&Value>,
        usage: Option<&ToolUsageFacts>,
        duration: Duration,
    ) {
        let engine = self.engine_request(descriptor, usage);
        let Some(observation) = &mut self.observation else {
            return;
        };
        if engine {
            observation.record_engine_request();
            if response.is_some() {
                submit_engine(observation, usage, duration, None);
            }
            return;
        }
        let delivered = response.map(|response| delivered_response(descriptor, response, usage));
        observation.record_delivered(request_observation(descriptor), delivered, duration);
    }

    pub fn record_response_failure(
        &mut self,
        descriptor: RequestDescriptor,
        duration: Duration,
        class: McpErrorClassV1,
        usage: Option<&ToolUsageFacts>,
    ) {
        let engine = self.engine_request(descriptor, usage);
        if let Some(observation) = &mut self.observation {
            if engine {
                observation.record_engine_request();
                submit_engine(observation, usage, duration, Some(class));
                return;
            }
            observation.record_response_failure_with_result(
                request_observation(descriptor),
                duration,
                class,
                search_result_metadata(usage),
            );
        }
    }

    fn engine_request(
        &self,
        descriptor: RequestDescriptor,
        usage: Option<&ToolUsageFacts>,
    ) -> bool {
        matches!(
            descriptor,
            RequestDescriptor::ToolCall {
                operation: McpToolKind::Unified(_)
            }
        ) || (self.remote && matches!(descriptor, RequestDescriptor::ToolCall { .. }))
            || usage.is_some_and(|u| u.graph.is_some() || u.sift.is_some() || u.remote.is_some())
    }

    pub fn stop(mut self, reason: McpStopReasonV1, outcome: Outcome, duration: Duration) {
        if let Some(observation) = self.observation.take() {
            observation.stop(reason, outcome, duration);
        }
    }
}

/// Called only at the existing response encode/write/flush finalizer. Carriers
/// preserve work facts even when the response is an error or the writer fails.
fn submit_engine(
    observation: &McpObservation,
    usage: Option<&ToolUsageFacts>,
    duration: Duration,
    failure: Option<McpErrorClassV1>,
) {
    use ctx_client_observability::analytics::{
        DeliveryEvidence, GraphOutputBoundary, SiftDelivery, SiftFailureKind, SiftMissingness,
        SiftPhase,
    };
    let Some(usage) = usage else { return };
    let delivery = if failure.is_some() {
        DeliveryEvidence::Failed
    } else {
        DeliveryEvidence::KnownComplete
    };
    if let Some(mut graph) = usage.graph {
        graph.completion.elapsed = duration;
        if graph.completion.delivery != DeliveryEvidence::Failed {
            graph.completion.delivery = delivery;
        }
        graph.details.output_boundary =
            Some(if failure == Some(McpErrorClassV1::ResponseSerialize) {
                GraphOutputBoundary::Unobserved
            } else {
                GraphOutputBoundary::StdioFlush
            });
        observation.submit_post_flush_event(graph.into_event());
    }
    if let Some(mut remote) = usage.remote {
        remote.completion.elapsed = duration;
        if remote.completion.delivery != DeliveryEvidence::Failed {
            remote.completion.delivery = delivery;
        }
        observation.submit_post_flush_event(remote.into_event());
    }
    if let Some(mut sift) = usage.sift {
        let mut latency = [0; 14];
        latency[ctx_client_observability::analytics::native_duration_index(duration)] = 1;
        sift.latency = Some(latency);
        sift.cohort.delivery = Some(if failure.is_some() || sift.output_failed > 0 {
            SiftDelivery::Failed
        } else {
            SiftDelivery::Flushed
        });
        if let Some(failure) = failure {
            sift.output_failed = 1;
            if sift.complete > 0 {
                sift.complete = 0;
                sift.partial = 1;
            }
            sift.bytes = None;
            sift.tokens = None;
            sift.cohort.missingness = Some(SiftMissingness::Incomplete);
            if sift.cohort.failure_phase.is_none() {
                sift.cohort.failure_phase =
                    Some(if failure == McpErrorClassV1::ResponseSerialize {
                        SiftPhase::Render
                    } else {
                        SiftPhase::Output
                    });
                sift.cohort.failure_kind = Some(if failure == McpErrorClassV1::ResponseSerialize {
                    SiftFailureKind::Other
                } else {
                    SiftFailureKind::Io
                });
            }
        }
        if let Some(event) = sift.into_event() {
            observation.submit_post_flush_event(event);
        }
    }
}

fn delivered_response(
    descriptor: RequestDescriptor,
    response: &Value,
    usage: Option<&ToolUsageFacts>,
) -> McpDeliveredResponse {
    let error_class = response
        .get("error")
        .map(|error| json_rpc_error_class(descriptor, error));
    let tool_error = response.pointer("/result/isError").and_then(Value::as_bool) == Some(true);
    let result = match descriptor {
        RequestDescriptor::ToolCall { operation } => result_metadata(operation, response, usage),
        _ => McpResultMetadataV1::default(),
    };
    McpDeliveredResponse {
        error_class,
        tool_error,
        result,
    }
}

fn json_rpc_error_class(descriptor: RequestDescriptor, error: &Value) -> McpErrorClassV1 {
    if descriptor == RequestDescriptor::InvalidUtf8 {
        return McpErrorClassV1::InvalidUtf8;
    }
    if descriptor == RequestDescriptor::LineTooLarge {
        return McpErrorClassV1::LineTooLarge;
    }
    if descriptor == RequestDescriptor::InvalidJson {
        return McpErrorClassV1::InvalidJson;
    }
    if matches!(
        descriptor,
        RequestDescriptor::ToolCall {
            operation: McpToolKind::Missing
        }
    ) {
        return McpErrorClassV1::MissingTool;
    }
    if matches!(
        descriptor,
        RequestDescriptor::ToolCall {
            operation: McpToolKind::Unknown
        }
    ) {
        return McpErrorClassV1::UnknownTool;
    }
    match error.get("code").and_then(Value::as_i64) {
        Some(-32700) => McpErrorClassV1::InvalidJson,
        Some(-32600) => McpErrorClassV1::InvalidRequest,
        Some(-32602) => McpErrorClassV1::InvalidParams,
        Some(-32002) => McpErrorClassV1::ServerNotInitialized,
        Some(-32601) => McpErrorClassV1::MethodNotFound,
        _ => McpErrorClassV1::InvalidRequest,
    }
}

fn result_metadata(
    operation: McpToolKind,
    response: &Value,
    usage: Option<&ToolUsageFacts>,
) -> McpResultMetadataV1 {
    let mut metadata = McpResultMetadataV1::default();
    let result = response.pointer("/result/structuredContent");
    match (operation, result) {
        (McpToolKind::Sources, Some(result)) => {
            if let Some(count) = result
                .get("sources")
                .and_then(Value::as_array)
                .map(Vec::len)
            {
                metadata = metadata.with_result_count(count);
            }
        }
        (McpToolKind::Search, Some(result)) => {
            if let Some(count) = result
                .get("results")
                .and_then(Value::as_array)
                .map(Vec::len)
            {
                metadata = metadata.with_result_count(count);
            }
            let truncated = result
                .pointer("/truncation/truncated")
                .and_then(Value::as_bool);
            let has_more = result
                .pointer("/pagination/has_more")
                .and_then(Value::as_bool);
            metadata.result_truncated = match (truncated, has_more) {
                (Some(a), Some(b)) => Some(a || b),
                (value @ Some(_), None) | (None, value @ Some(_)) => value,
                (None, None) => None,
            };
        }
        (McpToolKind::ShowSession, Some(result)) | (McpToolKind::ShowEvent, Some(result)) => {
            if let Some(count) = result.get("events").and_then(Value::as_array).map(Vec::len) {
                metadata = metadata.with_result_count(count);
            }
            metadata.events_truncated =
                result.pointer("/truncated/events").and_then(Value::as_bool);
            metadata.response_bound = Some(
                if result.get("error_code").and_then(Value::as_str) == Some("output_limit_exceeded")
                {
                    McpResponseBoundV1::Replaced
                } else {
                    McpResponseBoundV1::WithinLimit
                },
            );
        }
        (McpToolKind::QueryEvents, Some(result)) => {
            if let Some(count) = result.get("events").and_then(Value::as_array).map(Vec::len) {
                metadata = metadata.with_result_count(count);
            }
            metadata.result_truncated = result.get("truncated").and_then(Value::as_bool);
            metadata.response_bound = Some(
                if result.get("error_code").and_then(Value::as_str) == Some("output_limit_exceeded")
                {
                    McpResponseBoundV1::Replaced
                } else {
                    McpResponseBoundV1::WithinLimit
                },
            );
        }
        _ => {}
    }
    if operation == McpToolKind::Search {
        apply_search_execution(&mut metadata, usage);
    }
    metadata.blame = usage.and_then(|usage| usage.blame);
    metadata
}

fn search_result_metadata(usage: Option<&ToolUsageFacts>) -> McpResultMetadataV1 {
    let mut metadata = McpResultMetadataV1::default();
    apply_search_execution(&mut metadata, usage);
    metadata.blame = usage.and_then(|usage| usage.blame);
    metadata
}

fn apply_search_execution(metadata: &mut McpResultMetadataV1, usage: Option<&ToolUsageFacts>) {
    metadata.search = usage
        .and_then(|usage| usage.search_execution.as_ref())
        .copied()
        .map(search_terminal_facts);
}

fn search_terminal_facts(facts: ToolSearchTerminalFacts) -> SearchTerminalFacts {
    SearchTerminalFacts {
        refresh_duration: facts.refresh_duration,
        refresh_status: facts.refresh_status.map(search_refresh_status),
        refresh_source_count: facts.refresh_source_count,
        query_duration: facts.query_duration,
        backend_requested: facts.backend_requested.map(search_backend),
        backend_effective: facts.backend_effective.map(search_backend),
        health: SearchHealthFacts {
            retrieval_rounds: facts.retrieval_rounds,
            query_executions: facts.query_executions,
            candidate_rows: facts.candidate_rows,
            records_decoded: facts.records_decoded,
            encoded_core_bytes_decoded: facts.encoded_core_bytes_decoded,
            final_candidate_pool: facts.final_candidate_pool,
            candidate_pool_truncated: facts.candidate_pool_truncated,
            stop_reason: facts.stop_reason.map(search_stop_reason),
            failure_phase: facts.failure_phase.map(search_failure_phase),
        },
        output_duration: facts.output_duration,
        output_served: facts.output_served,
    }
}

const fn search_refresh_status(status: ToolSearchRefreshStatus) -> RefreshStatus {
    match status {
        ToolSearchRefreshStatus::ExistingGeneration => RefreshStatus::ExistingGeneration,
        ToolSearchRefreshStatus::DaemonBackground => RefreshStatus::DaemonBackground,
        ToolSearchRefreshStatus::DaemonUnavailable => RefreshStatus::DaemonUnavailable,
        ToolSearchRefreshStatus::Completed => RefreshStatus::Completed,
        ToolSearchRefreshStatus::Failed => RefreshStatus::Failed,
    }
}

const fn search_backend(backend: ToolSearchBackend) -> SearchBackend {
    match backend {
        ToolSearchBackend::Lexical => SearchBackend::Lexical,
        ToolSearchBackend::Semantic => SearchBackend::Semantic,
        ToolSearchBackend::Hybrid => SearchBackend::Hybrid,
    }
}

const fn search_stop_reason(reason: ToolSearchStopReason) -> SearchStopReason {
    match reason {
        ToolSearchStopReason::Decisive => SearchStopReason::Decisive,
        ToolSearchStopReason::Exhausted => SearchStopReason::Exhausted,
        ToolSearchStopReason::CandidateCap => SearchStopReason::CandidateCap,
        ToolSearchStopReason::FixedPool => SearchStopReason::FixedPool,
    }
}

const fn search_failure_phase(phase: ToolSearchFailurePhase) -> SearchFailurePhase {
    match phase {
        ToolSearchFailurePhase::Preparation => SearchFailurePhase::Preparation,
        ToolSearchFailurePhase::Refresh => SearchFailurePhase::Refresh,
        ToolSearchFailurePhase::GenerationOpen => SearchFailurePhase::GenerationOpen,
        ToolSearchFailurePhase::QueryPreparation => SearchFailurePhase::QueryPreparation,
        ToolSearchFailurePhase::SemanticRetrieval => SearchFailurePhase::SemanticRetrieval,
        ToolSearchFailurePhase::IndexQueryDecode => SearchFailurePhase::IndexQueryDecode,
        ToolSearchFailurePhase::ResultProjection => SearchFailurePhase::ResultProjection,
        ToolSearchFailurePhase::Render => SearchFailurePhase::Render,
        ToolSearchFailurePhase::Output => SearchFailurePhase::Output,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod engine_tests;
