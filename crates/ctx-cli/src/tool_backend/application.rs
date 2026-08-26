use std::path::PathBuf;

use ctx_agent_application::{
    mcp_tool_call::invoke_mcp_tool_call,
    tool_backend::{
        HistoryReadOutcome, HistoryReadPort, SearchReadOutcome, SearchReadinessPort, SourceCatalog,
        SourceCatalogPort,
    },
};
use ctx_history_core::MAX_CORE_CONTENT_BYTES;
use ctx_history_index::{
    CoreEventPageBudget, CoreEventRangeDirection, CoreEventRangeFilters, CoreEventRangeScope,
    SearchContentScope,
};
use serde_json::Value;

use super::{
    CursorFailureKind, OpaqueMcpProxyError, QueryEventsRequest, ShowEventRequest,
    ShowSessionRequest, StructuredToolError, ToolBackend, ToolBackendError, ToolEventContent,
    ToolEventRangeDirection, ToolEventRangeScope, ToolExecutionError, ToolOperation, ToolOutcome,
    ToolSearchBackend, ToolSearchConcentrationFacts, ToolSearchContentScope,
    ToolSearchCopyClusterAvailability, ToolSearchDiversificationStatus, ToolSearchFailurePhase,
    ToolSearchLiteralRootFacts, ToolSearchRefreshStatus, ToolSearchRequest, ToolSearchStopReason,
    ToolSearchTerminalFacts, ToolSearchUsageFacts, ToolTranscriptMode, ToolUsageFacts,
};
use crate::{
    commands::list::events::{
        decode_cursor, event_range_page_value, mcp_event_query_core_record_bytes, selection,
        validated_limit, EventContentProjection, EventQueryError, EventQueryWireRequest,
    },
    config,
    observability_composition::{local_usage_storage_authority, usage_control_snapshot},
    ProviderArg,
};

#[derive(Debug, Clone)]
pub(crate) struct LocalToolBackend {
    data_root: PathBuf,
}

fn adapt_tool_search_request(
    request: ToolSearchRequest,
) -> crate::commands::source_index::SourceSearchRequest {
    crate::commands::source_index::SourceSearchRequest {
        query: request.query,
        terms: Vec::new(),
        limit: request.limit,
        provider: request.provider,
        history_source: request.history_source,
        provider_key: request.provider_key,
        source_id: request.source_id,
        source_format: request.source_format,
        source_roots: request.source_roots,
        source_groups: request.source_groups,
        workspace: request.workspace,
        since: request.since,
        primary_only: request.primary_only,
        content_scope: match request.content_scope {
            ToolSearchContentScope::All => SearchContentScope::All,
            ToolSearchContentScope::Transcript => SearchContentScope::Transcript,
            ToolSearchContentScope::Calls => SearchContentScope::Calls,
            ToolSearchContentScope::Outputs => SearchContentScope::Outputs,
        },
        event_type: request.event_type,
        file: request.file,
        session: request.session,
        exclude_sessions: Vec::new(),
        events: request.events,
        include_current_session: request.include_current_session,
        backend: request.backend.map(|backend| match backend {
            ToolSearchBackend::Lexical => crate::SearchBackendArg::Lexical,
            ToolSearchBackend::Semantic => crate::SearchBackendArg::Semantic,
            ToolSearchBackend::Hybrid => crate::SearchBackendArg::Hybrid,
        }),
        semantic_weight: request.semantic_weight,
    }
}

impl LocalToolBackend {
    pub(crate) fn new(data_root: PathBuf) -> Self {
        Self { data_root }
    }

    fn status(&self) -> Result<Value, ToolBackendError> {
        let config =
            config::AppConfig::load(&self.data_root).map_err(classify_application_error)?;
        let storage = local_usage_storage_authority(&self.data_root);
        let control = usage_control_snapshot(config.local_usage.enabled);
        let value = crate::commands::status::status_read_model_authorized(
            &self.data_root,
            &config,
            &storage,
            &control,
        )
        .map_err(classify_application_error)?
        .report;
        Ok(value)
    }

    fn sources(&self) -> Result<SourceCatalog, ToolBackendError> {
        let config =
            config::AppConfig::load(&self.data_root).map_err(classify_application_error)?;
        let automatic_discovery = config.automatic_source_discovery_enabled();
        let provider_roots = config.provider_root_definitions();
        let report = ctx_history_cli::discovered_sources_report_with_data_root_and_provider_roots(
            crate::identity::home_dir().as_deref(),
            &self.data_root,
            automatic_discovery,
            &provider_roots,
        );
        let mut source_values = crate::sources_json(&report.sources);
        crate::provider_sources::enrich_sources_json_with_selection(
            &mut source_values,
            &report.sources,
            &provider_roots,
        );
        source_values.extend(
            crate::discovered_plugin_sources_json(&self.data_root)
                .map_err(classify_application_error)?,
        );
        let (issues, issues_truncated) =
            crate::provider_sources::discovery_report_issues_json_with_provider_roots(
                &report,
                &provider_roots,
                automatic_discovery,
            );
        Ok(SourceCatalog {
            automatic_discovery,
            sources: source_values,
            issues,
            issues_truncated,
        })
    }

    fn search(&self, request: ToolSearchRequest) -> Result<SearchReadOutcome, ToolExecutionError> {
        let request = adapt_tool_search_request(request);
        crate::commands::source_index::validate_explicit_semantic_scope(&request)
            .map_err(classify_mcp_search_error)?;
        let config = config::AppConfig::load(&self.data_root);
        if let Ok(config) = &config {
            self.recover_enabled_daemon_before_search(config);
        }
        let config = match config {
            Ok(config) => config,
            Err(error) => {
                let mut request = request;
                crate::commands::source_index::normalize_mcp_search_request(&mut request)
                    .map_err(classify_mcp_search_error)?;
                return Err(classify_application_error(error).into());
            }
        };
        let (structured, observation, compact, execution) =
            match crate::commands::source_index::mcp_search_with_compact(
                request,
                &self.data_root,
                ctx_history_cli::HistoryCliConfig {
                    daemon_enabled: config.automatic_indexing_enabled(),
                    semantic_search_enabled: config.semantic_search_enabled(),
                    local_usage_enabled: config.local_usage.enabled,
                    automatic_provider_discovery: config.automatic_source_discovery_enabled(),
                    provider_roots: config.provider_root_definitions(),
                },
            ) {
                Ok(result) => result,
                Err(failure) => {
                    let (error, observation) = failure.into_parts();
                    return Err(ToolExecutionError {
                        error: Box::new(classify_mcp_search_error(error)),
                        usage: Box::new(ToolUsageFacts {
                            search: None,
                            search_execution: Some(search_terminal_facts(observation)),
                        }),
                    });
                }
            };
        let search = observation
            .complete_byte_totals()
            .map(|(delivered, matched)| ToolSearchUsageFacts::complete(delivered, matched))
            .unwrap_or_else(ToolSearchUsageFacts::unavailable);
        Ok(SearchReadOutcome {
            structured,
            compact,
            usage: search,
            execution: search_terminal_facts(execution),
        })
    }

    fn recover_enabled_daemon_before_search(&self, config: &config::AppConfig) {
        if !config.automatic_indexing_enabled()
            || crate::semantic::daemon_autostart_suppression_reason().is_some()
        {
            return;
        }
        let _ = crate::semantic::autostart_daemon_and_wait(
            &self.data_root,
            config,
            crate::DaemonTriggerCommandArg::Search,
        );
    }

    fn show_session(
        &self,
        request: ShowSessionRequest,
    ) -> Result<HistoryReadOutcome, ToolBackendError> {
        let mode = match request.mode {
            ToolTranscriptMode::Full => crate::TranscriptMode::Full,
            ToolTranscriptMode::Lite => crate::TranscriptMode::Lite,
            ToolTranscriptMode::Log => crate::TranscriptMode::Log,
        };
        crate::commands::source_index::mcp_show_session_application(
            &self.data_root,
            &request.selector,
            match mode {
                crate::TranscriptMode::Full => ctx_history_cli::TranscriptMode::Full,
                crate::TranscriptMode::Lite => ctx_history_cli::TranscriptMode::Lite,
                crate::TranscriptMode::Log => ctx_history_cli::TranscriptMode::Log,
            },
            request.limit,
            request.cursor.as_deref(),
            request.output_limit_bytes,
        )
        .map(|(structured, compact)| HistoryReadOutcome {
            structured,
            compact,
        })
        .map_err(classify_show_error)
    }

    fn show_event(
        &self,
        request: ShowEventRequest,
    ) -> Result<HistoryReadOutcome, ToolBackendError> {
        crate::commands::source_index::mcp_show_event_application(
            &self.data_root,
            &request.selector,
            request.before,
            request.after,
            request.window,
            request.output_limit_bytes,
        )
        .map(|(structured, compact)| HistoryReadOutcome {
            structured,
            compact,
        })
        .map_err(classify_show_error)
    }

    fn query_events(&self, request: QueryEventsRequest) -> Result<Value, ToolBackendError> {
        let filters = CoreEventRangeFilters {
            providers: request.filters.providers,
            source_identity: request.filters.source_identity,
            history_source: request.filters.history_source,
            provider_key: request.filters.provider_key,
            source_id: request.filters.source_id,
            source_format: request.filters.source_format,
            provider_session_id: request.filters.provider_session_id,
            session_id: request.filters.session_id,
            parent_session_id: request.filters.parent_session_id,
            root_session_id: request.filters.root_session_id,
            branch: request.filters.branch,
            workspace: request.filters.workspace,
            event_type: request.filters.event_type,
            role: request.filters.role,
            scope: match request.filters.scope {
                ToolEventRangeScope::All => CoreEventRangeScope::All,
                ToolEventRangeScope::Primary => CoreEventRangeScope::Primary,
                ToolEventRangeScope::Subagent => CoreEventRangeScope::Subagent,
            },
            file: request.filters.file,
            direction: match request.filters.direction {
                ToolEventRangeDirection::Ascending => CoreEventRangeDirection::Ascending,
                ToolEventRangeDirection::Descending => CoreEventRangeDirection::Descending,
            },
        };
        let selection = selection(request.since.as_deref(), request.until.as_deref(), filters)
            .map_err(event_query_failure)?;
        let cursor = request
            .cursor
            .as_deref()
            .map(decode_cursor)
            .transpose()
            .map_err(event_query_failure)?;
        let limit = validated_limit(request.limit).map_err(event_query_failure)?;
        let content = match request.content {
            ToolEventContent::Full => EventContentProjection::Full,
            ToolEventContent::Text => EventContentProjection::Text,
            ToolEventContent::None => EventContentProjection::None,
        };
        let wire = EventQueryWireRequest::from_selection(&selection, content, limit);
        let record_bytes = mcp_event_query_core_record_bytes(request.output_limit_bytes);
        let strict_budget =
            CoreEventPageBudget::new(record_bytes, record_bytes.min(MAX_CORE_CONTENT_BYTES));
        event_range_page_value(
            &self.data_root,
            &selection,
            cursor.as_ref(),
            &wire,
            Some(strict_budget),
        )
        .map_err(event_query_failure)
    }
}

fn search_terminal_facts(
    observation: ctx_history_cli::SearchExecutionObservation,
) -> ToolSearchTerminalFacts {
    ToolSearchTerminalFacts {
        refresh_duration: observation.refresh_duration,
        refresh_status: observation.refresh_status.map(search_refresh_status),
        refresh_source_count: observation.refresh_source_count,
        query_duration: observation.query_duration,
        backend_requested: observation.backend_requested.map(search_backend),
        backend_effective: observation.backend_effective.map(search_backend),
        retrieval_rounds: observation.work.retrieval_rounds,
        query_executions: observation.work.query_executions,
        candidate_rows: observation.work.candidate_rows,
        records_decoded: observation.work.records_decoded,
        encoded_core_bytes_decoded: observation.work.encoded_core_bytes_decoded,
        final_candidate_pool: observation.final_candidate_pool,
        candidate_pool_truncated: observation.candidate_pool_truncated,
        concentration: tool_search_concentration_facts(&observation),
        stop_reason: observation.stop_reason.map(search_stop_reason),
        failure_phase: observation.failure_phase.map(search_failure_phase),
        output_duration: None,
        output_served: None,
    }
}

fn tool_search_concentration_facts(
    observation: &ctx_history_cli::SearchExecutionObservation,
) -> Option<ToolSearchConcentrationFacts> {
    let concentration = observation.concentration?;
    let diversification = observation.diversification?;
    let literal_roots = match concentration.literal_roots {
        ctx_history_read_application::SearchLiteralRootConcentration::Observed {
            distinct_families,
            candidate_count,
            largest_family_candidate_count,
        } => ToolSearchLiteralRootFacts::Observed {
            candidate_families: distinct_families,
            candidate_count,
            largest_family_candidate_count,
        },
        ctx_history_read_application::SearchLiteralRootConcentration::NotObservedDense => {
            ToolSearchLiteralRootFacts::NotObservedDense
        }
    };
    Some(ToolSearchConcentrationFacts {
        candidate_sessions: concentration.distinct_sessions,
        largest_session_candidate_count: concentration.largest_session_candidate_count,
        literal_roots,
        provider_copy_candidate_count: concentration.provider_copy_candidate_count,
        copy_cluster_availability: match concentration.copy_clusters {
            ctx_history_read_application::SearchCopyClusterAvailability::NotConstructedV1 => {
                ToolSearchCopyClusterAvailability::NotConstructedV1
            }
        },
        diversification_status: match diversification.status {
            ctx_history_read_application::SearchDiversificationStatus::Applied => {
                ToolSearchDiversificationStatus::Applied
            }
            ctx_history_read_application::SearchDiversificationStatus::NotApplicable => {
                ToolSearchDiversificationStatus::NotApplicable
            }
            ctx_history_read_application::SearchDiversificationStatus::Indeterminate => {
                ToolSearchDiversificationStatus::Indeterminate
            }
        },
        diversification_changed_final_top_n: diversification.changed_final_top_n,
    })
}

const fn search_refresh_status(
    status: ctx_history_cli::SearchRefreshStatus,
) -> ToolSearchRefreshStatus {
    match status {
        ctx_history_cli::SearchRefreshStatus::ExistingGeneration => {
            ToolSearchRefreshStatus::ExistingGeneration
        }
        ctx_history_cli::SearchRefreshStatus::DaemonBackground => {
            ToolSearchRefreshStatus::DaemonBackground
        }
        ctx_history_cli::SearchRefreshStatus::DaemonUnavailable => {
            ToolSearchRefreshStatus::DaemonUnavailable
        }
        ctx_history_cli::SearchRefreshStatus::Completed => ToolSearchRefreshStatus::Completed,
        ctx_history_cli::SearchRefreshStatus::Failed => ToolSearchRefreshStatus::Failed,
    }
}

const fn search_backend(backend: ctx_history_read_application::SearchBackend) -> ToolSearchBackend {
    match backend {
        ctx_history_read_application::SearchBackend::Lexical => ToolSearchBackend::Lexical,
        ctx_history_read_application::SearchBackend::Semantic => ToolSearchBackend::Semantic,
        ctx_history_read_application::SearchBackend::Hybrid => ToolSearchBackend::Hybrid,
    }
}

const fn search_stop_reason(
    reason: ctx_history_read_application::SearchStopReason,
) -> ToolSearchStopReason {
    match reason {
        ctx_history_read_application::SearchStopReason::Decisive => ToolSearchStopReason::Decisive,
        ctx_history_read_application::SearchStopReason::Exhausted => {
            ToolSearchStopReason::Exhausted
        }
        ctx_history_read_application::SearchStopReason::CandidateCap => {
            ToolSearchStopReason::CandidateCap
        }
        ctx_history_read_application::SearchStopReason::FixedPool => {
            ToolSearchStopReason::FixedPool
        }
    }
}

const fn search_failure_phase(
    phase: ctx_history_cli::SearchFailurePhase,
) -> ToolSearchFailurePhase {
    match phase {
        ctx_history_cli::SearchFailurePhase::Preparation => ToolSearchFailurePhase::Preparation,
        ctx_history_cli::SearchFailurePhase::Refresh => ToolSearchFailurePhase::Refresh,
        ctx_history_cli::SearchFailurePhase::GenerationOpen => {
            ToolSearchFailurePhase::GenerationOpen
        }
        ctx_history_cli::SearchFailurePhase::QueryPreparation => {
            ToolSearchFailurePhase::QueryPreparation
        }
        ctx_history_cli::SearchFailurePhase::SemanticRetrieval => {
            ToolSearchFailurePhase::SemanticRetrieval
        }
        ctx_history_cli::SearchFailurePhase::IndexQueryDecode => {
            ToolSearchFailurePhase::IndexQueryDecode
        }
        ctx_history_cli::SearchFailurePhase::ResultProjection => {
            ToolSearchFailurePhase::ResultProjection
        }
        ctx_history_cli::SearchFailurePhase::Render => ToolSearchFailurePhase::Render,
        ctx_history_cli::SearchFailurePhase::Output => ToolSearchFailurePhase::Output,
    }
}

impl HistoryReadPort for LocalToolBackend {
    fn status(&self) -> Result<Value, ToolBackendError> {
        LocalToolBackend::status(self)
    }

    fn show_session(
        &self,
        request: ShowSessionRequest,
    ) -> Result<HistoryReadOutcome, ToolBackendError> {
        LocalToolBackend::show_session(self, request)
    }

    fn show_event(
        &self,
        request: ShowEventRequest,
    ) -> Result<HistoryReadOutcome, ToolBackendError> {
        LocalToolBackend::show_event(self, request)
    }

    fn query_events(&self, request: QueryEventsRequest) -> Result<Value, ToolBackendError> {
        LocalToolBackend::query_events(self, request)
    }
}

impl SearchReadinessPort for LocalToolBackend {
    fn search_ready(
        &self,
        request: ToolSearchRequest,
    ) -> Result<SearchReadOutcome, ToolExecutionError> {
        self.search(request)
    }
}

impl SourceCatalogPort for LocalToolBackend {
    fn source_catalog(&self) -> Result<SourceCatalog, ToolBackendError> {
        self.sources()
    }
}

impl ToolBackend for LocalToolBackend {
    fn execute(&self, operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError> {
        invoke_mcp_tool_call(operation, self, self, self)
    }

    fn proxy_companion_mcp(&self, request: &[u8]) -> Result<Vec<u8>, OpaqueMcpProxyError> {
        crate::companion::proxy_paid_mcp(request, &self.data_root).map_err(|error| match error {
            crate::companion::CompanionRouteError::Unavailable => {
                OpaqueMcpProxyError::CompanionUnavailable
            }
            crate::companion::CompanionRouteError::Incompatible => {
                OpaqueMcpProxyError::CompanionIncompatible
            }
        })
    }

    fn parse_provider(&self, value: &str) -> Option<ctx_history_core::CaptureProvider> {
        ProviderArg::parse_name(value)
            .map(ProviderArg::capture_provider)
            .filter(|provider| crate::provider_args::cli_supported_provider(*provider))
    }

    fn provider_names(&self) -> Vec<&'static str> {
        ProviderArg::mcp_names()
    }
}

fn event_query_failure(error: EventQueryError) -> ToolBackendError {
    ToolBackendError::EventQuery(StructuredToolError {
        structured: crate::commands::list::events::event_query_error_value(&error),
        detail: error.to_string(),
    })
}

fn classify_mcp_search_error(
    error: crate::commands::source_index::McpSearchError,
) -> ToolBackendError {
    match error {
        crate::commands::source_index::McpSearchError::SemanticNotReady {
            code,
            detail,
            retryable,
        } => ToolBackendError::SemanticNotReady {
            code,
            detail,
            retryable,
        },
        crate::commands::source_index::McpSearchError::SemanticFailed { detail } => {
            ToolBackendError::internal(detail)
        }
        crate::commands::source_index::McpSearchError::SourceUnavailable => {
            ToolBackendError::SourceUnavailable
        }
        crate::commands::source_index::McpSearchError::GenerationChanged => {
            ToolBackendError::GenerationChanged
        }
        crate::commands::source_index::McpSearchError::GenerationAuthority(error) => {
            ToolBackendError::GenerationAuthority(StructuredToolError {
                structured: crate::commands::source_index::generation_query_authority_error_json(
                    &error,
                ),
                detail: error.to_string(),
            })
        }
        crate::commands::source_index::McpSearchError::Application { detail } => {
            if detail.contains("unknown provider root selector in the pinned generation")
                || detail.contains("unknown provider root selector `")
                || detail.contains("invalid source root selector `")
            {
                ToolBackendError::invalid_request(
                    "source_roots contains an invalid or unavailable selector",
                )
            } else if detail.contains("unknown provider root group in the pinned generation")
                || detail.contains("unknown provider root group `")
                || detail.contains("invalid source group selector `")
            {
                ToolBackendError::invalid_request(
                    "source_groups contains an invalid or unavailable selector",
                )
            } else {
                ToolBackendError::internal(detail)
            }
        }
    }
}

fn classify_show_error(
    error: crate::commands::source_index::ShowApplicationError,
) -> ToolBackendError {
    match error {
        crate::commands::source_index::ShowApplicationError::GenerationChanged => {
            ToolBackendError::GenerationChanged
        }
        crate::commands::source_index::ShowApplicationError::GenerationAuthority(error) => {
            ToolBackendError::GenerationAuthority(StructuredToolError {
                structured: crate::commands::source_index::generation_query_authority_error_json(
                    &error,
                ),
                detail: error.to_string(),
            })
        }
        crate::commands::source_index::ShowApplicationError::CursorStale { detail } => {
            ToolBackendError::Cursor {
                kind: CursorFailureKind::Stale,
                detail,
            }
        }
        crate::commands::source_index::ShowApplicationError::CursorMismatch { detail } => {
            ToolBackendError::Cursor {
                kind: CursorFailureKind::Mismatch,
                detail,
            }
        }
        crate::commands::source_index::ShowApplicationError::InvalidCursor { detail } => {
            ToolBackendError::Cursor {
                kind: CursorFailureKind::Invalid,
                detail,
            }
        }
        crate::commands::source_index::ShowApplicationError::OutputLimit {
            event_id,
            actual_bytes,
            maximum_bytes,
        } => ToolBackendError::OutputLimit {
            event_id,
            actual_bytes,
            maximum_bytes,
        },
        crate::commands::source_index::ShowApplicationError::Application { detail } => {
            ToolBackendError::internal(detail)
        }
    }
}

fn classify_application_error(error: anyhow::Error) -> ToolBackendError {
    ToolBackendError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    fn private_tempdir() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        ctx_history_platform::platform_security::restrict_private_directory(root.path()).unwrap();
        root
    }

    fn lexical_search_request() -> ToolSearchRequest {
        ToolSearchRequest {
            query: "config snapshot canary".to_owned(),
            limit: 8,
            provider: None,
            history_source: None,
            provider_key: None,
            source_id: None,
            source_format: None,
            source_roots: Vec::new(),
            source_groups: Vec::new(),
            workspace: None,
            since: None,
            primary_only: false,
            content_scope: ToolSearchContentScope::All,
            event_type: None,
            file: None,
            session: None,
            events: false,
            include_current_session: true,
            backend: Some(ToolSearchBackend::Lexical),
            semantic_weight: 0.35,
        }
    }

    #[test]
    fn mcp_terminal_adapter_retains_partial_failure_work() {
        let facts = search_terminal_facts(ctx_history_cli::SearchExecutionObservation {
            backend_requested: Some(ctx_history_read_application::SearchBackend::Hybrid),
            work: ctx_history_read_application::SearchWorkReceipt {
                retrieval_rounds: Some(2),
                query_executions: Some(3),
                candidate_rows: Some(8),
                records_decoded: Some(1),
                encoded_core_bytes_decoded: Some(144),
            },
            failure_phase: Some(ctx_history_cli::SearchFailurePhase::IndexQueryDecode),
            ..ctx_history_cli::SearchExecutionObservation::default()
        });

        assert_eq!(facts.backend_requested, Some(ToolSearchBackend::Hybrid));
        assert_eq!(facts.backend_effective, None);
        assert_eq!(facts.retrieval_rounds, Some(2));
        assert_eq!(facts.query_executions, Some(3));
        assert_eq!(facts.candidate_rows, Some(8));
        assert_eq!(facts.records_decoded, Some(1));
        assert_eq!(facts.encoded_core_bytes_decoded, Some(144));
        assert_eq!(
            facts.failure_phase,
            Some(ToolSearchFailurePhase::IndexQueryDecode)
        );
        assert_eq!(facts.output_served, None);
    }

    #[test]
    fn cli_and_mcp_search_adapters_produce_the_same_typed_query_request() {
        let cli = crate::Cli::try_parse_from([
            "ctx",
            "search",
            "adapter parity",
            "--limit",
            "8",
            "--provider",
            "claude",
            "--workspace",
            "/workspace/pinned",
            "--since",
            "30d",
            "--content-scope",
            "transcript",
            "--file",
            "src/lib.rs",
            "--session",
            "019fa000-0000-7000-8000-0000000000d1",
            "--events",
            "--primary-only",
            "--include-current-session",
            "--backend",
            "lexical",
            "--semantic-weight",
            "0.4",
            "--refresh",
            "off",
        ])
        .unwrap();
        let crate::cli::CommandRoot::Search(args) = cli.command else {
            panic!("expected search command")
        };
        let cli_request = ctx_history_read_application::SearchRequest::from(
            ctx_history_cli::SearchRequest::from(crate::commands::search::adapt(args)),
        );
        let mcp_request = adapt_tool_search_request(ToolSearchRequest {
            query: "adapter parity".to_owned(),
            limit: 8,
            provider: Some(ctx_history_core::CaptureProvider::Claude),
            history_source: None,
            provider_key: None,
            source_id: None,
            source_format: None,
            source_roots: Vec::new(),
            source_groups: Vec::new(),
            workspace: Some("/workspace/pinned".to_owned()),
            since: Some("30d".to_owned()),
            primary_only: true,
            content_scope: ToolSearchContentScope::Transcript,
            event_type: None,
            file: Some(PathBuf::from("src/lib.rs")),
            session: Some("019fa000-0000-7000-8000-0000000000d1".to_owned()),
            events: true,
            include_current_session: true,
            backend: Some(ToolSearchBackend::Lexical),
            semantic_weight: 0.4,
        });
        assert_eq!(cli_request, mcp_request);
    }

    #[test]
    fn status_surfaces_use_one_config_load_and_no_usage_metadata_probe() {
        let root = private_tempdir();
        let backend = LocalToolBackend::new(root.path().to_path_buf());
        let ((result, metadata_reads), config_loads) =
            crate::config::count_app_config_loads(|| {
                crate::observability_composition::count_usage_control_metadata_reads(|| {
                    backend.status()
                })
            });
        result.unwrap();
        assert_eq!(config_loads, 1);
        assert_eq!(metadata_reads, 0);
    }

    #[test]
    fn mcp_search_recovery_and_backend_selection_share_one_config_snapshot() {
        let root = private_tempdir();
        config::set_daemon_enabled(root.path(), false).unwrap();
        let backend = LocalToolBackend::new(root.path().to_path_buf());

        let (result, config_loads) =
            crate::config::count_app_config_loads(|| backend.search(lexical_search_request()));

        assert!(matches!(
            result,
            Err(error) if matches!(*error.error, ToolBackendError::SourceUnavailable)
        ));
        assert_eq!(config_loads, 1);
    }

    #[test]
    fn mcp_selector_errors_are_typed_and_do_not_echo_selector_contents() {
        let rejected = "Private_Selector_7f98";
        for (detail, expected) in [
            (
                format!("unknown provider root selector `{rejected}` in the pinned generation"),
                "source_roots contains an invalid or unavailable selector",
            ),
            (
                format!("unknown provider root group `{rejected}` in the pinned generation"),
                "source_groups contains an invalid or unavailable selector",
            ),
        ] {
            let error = classify_mcp_search_error(
                crate::commands::source_index::McpSearchError::Application { detail },
            );
            assert!(matches!(
                error,
                ToolBackendError::InvalidRequest { detail }
                    if detail == expected && !detail.contains(rejected)
            ));
        }
    }

    #[test]
    fn typed_show_error_mapping_preserves_cursor_kind_and_detail() {
        let detail = "cursor belongs to another session".to_owned();
        let error = classify_show_error(
            crate::commands::source_index::ShowApplicationError::CursorMismatch {
                detail: detail.clone(),
            },
        );
        assert!(matches!(
            error,
            ToolBackendError::Cursor {
                kind: CursorFailureKind::Mismatch,
                detail: observed,
            } if observed == detail
        ));
    }

    #[test]
    fn typed_search_generation_authority_maps_without_anyhow_flattening() {
        let error = ctx_history_refresh::GenerationQueryAuthorityError::UncertifiedEmpty {
            generation_id: "11".repeat(32),
        };
        let mapped = classify_mcp_search_error(
            crate::commands::source_index::McpSearchError::GenerationAuthority(error),
        );
        assert!(matches!(
            mapped,
            ToolBackendError::GenerationAuthority(StructuredToolError { structured, .. })
                if structured["error_code"] == "source_unavailable"
        ));
    }
}
