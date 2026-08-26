use std::time::Duration;

use ctx_agent_integrations::{
    mcp::{McpToolKind, RequestDescriptor},
    tool_backend::{
        ToolSearchBackend, ToolSearchConcentrationFacts, ToolSearchCopyClusterAvailability,
        ToolSearchDiversificationStatus, ToolSearchFailurePhase, ToolSearchLiteralRootFacts,
        ToolSearchRefreshStatus, ToolSearchStopReason, ToolSearchTerminalFacts, ToolUsageFacts,
    },
};
use ctx_client_observability::{
    analytics::{
        McpErrorClassV1, McpResponseBoundV1, McpResultMetadataV1, McpStopReasonV1, Outcome,
        PublicEventV1, RefreshStatus, SearchBackend, SearchConcentrationFacts,
        SearchCopyClusterAvailability, SearchDiversificationStatus, SearchFailurePhase,
        SearchHealthFacts, SearchLiteralRootFacts, SearchStopReason, SearchTerminalFacts,
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
        McpToolKind::Blame | McpToolKind::ProStatus => None,
        McpToolKind::Unknown | McpToolKind::Missing => None,
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
                None if operation == McpToolKind::Unknown || operation.is_companion_owned() => {
                    McpObservedTool::Unknown
                }
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
        }
    }

    pub fn record_delivered(
        &mut self,
        descriptor: RequestDescriptor,
        response: Option<&Value>,
        usage: Option<&ToolUsageFacts>,
        duration: Duration,
    ) {
        let Some(observation) = &mut self.observation else {
            return;
        };
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
        if let Some(observation) = &mut self.observation {
            observation.record_response_failure_with_result(
                request_observation(descriptor),
                duration,
                class,
                search_result_metadata(usage),
            );
        }
    }

    pub fn stop(mut self, reason: McpStopReasonV1, outcome: Outcome, duration: Duration) {
        if let Some(observation) = self.observation.take() {
            observation.stop(reason, outcome, duration);
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
    metadata
}

fn search_result_metadata(usage: Option<&ToolUsageFacts>) -> McpResultMetadataV1 {
    let mut metadata = McpResultMetadataV1::default();
    apply_search_execution(&mut metadata, usage);
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
            concentration: facts.concentration.map(search_concentration_facts),
            stop_reason: facts.stop_reason.map(search_stop_reason),
            failure_phase: facts.failure_phase.map(search_failure_phase),
        },
        output_duration: facts.output_duration,
        output_served: facts.output_served,
    }
}

fn search_concentration_facts(value: ToolSearchConcentrationFacts) -> SearchConcentrationFacts {
    SearchConcentrationFacts {
        candidate_sessions: value.candidate_sessions,
        largest_session_candidate_count: value.largest_session_candidate_count,
        literal_roots: match value.literal_roots {
            ToolSearchLiteralRootFacts::Observed {
                candidate_families,
                candidate_count,
                largest_family_candidate_count,
            } => SearchLiteralRootFacts::Observed {
                candidate_families,
                candidate_count,
                largest_family_candidate_count,
            },
            ToolSearchLiteralRootFacts::NotObservedDense => {
                SearchLiteralRootFacts::NotObservedDense
            }
        },
        provider_copy_candidate_count: value.provider_copy_candidate_count,
        copy_cluster_availability: match value.copy_cluster_availability {
            ToolSearchCopyClusterAvailability::NotConstructedV1 => {
                SearchCopyClusterAvailability::NotConstructedV1
            }
        },
        diversification_status: match value.diversification_status {
            ToolSearchDiversificationStatus::Applied => SearchDiversificationStatus::Applied,
            ToolSearchDiversificationStatus::NotApplicable => {
                SearchDiversificationStatus::NotApplicable
            }
            ToolSearchDiversificationStatus::Indeterminate => {
                SearchDiversificationStatus::Indeterminate
            }
        },
        diversification_changed_final_top_n: value.diversification_changed_final_top_n,
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
