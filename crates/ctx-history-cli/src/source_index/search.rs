mod hydration;
mod observation;
mod query;
mod semantic_port;
#[cfg(test)]
mod test_support;

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use ctx_history_index::VerifiedIndex;
use serde_json::Value;

use crate::{
    cli::SearchArgs,
    config,
    local_usage::{CliUsage, ResultObservationAction, SearchContextObservation},
    output::JsonOutputFormat,
    semantic::coordinate_source_backed_refresh,
    ui::{
        canonical_human_output_bytes, diagnostic, Action, Diagnostic, DiagnosticLevel, Document,
        RenderContext, Ui,
    },
    HistoryCliConfig, RefreshMode, SearchExecutionObservation, SearchFailurePhase,
};
use ctx_daemon_cli::{
    wait_for_daemon_query_service, wait_for_daemon_semantic_generation,
    PinnedSourceBackedGeneration, SourceBackedRefreshDaemonUnavailable, SourceBackedRefreshMode,
    SourceBackedRefreshObservation,
};

use super::{
    render::{render_search_document, render_search_not_ready_document},
    shared::{
        externalize_query_error, index_root, is_active_generation_race,
        render_active_generation_race, ActiveGenerationRaceCommand,
    },
};
use ctx_history_read_application::SearchBackend;

pub(in crate::source_index) use hydration::SearchPresentation;
use observation::{
    initial_search_observation, observed_refresh_for_search, search_existing_generation_with_port,
};
#[cfg(test)]
pub(super) use query::resolve_source_search_backend;
pub(super) use query::source_search_policy;
use query::unsupported_semantic_scope;
pub(super) use query::NormalizedSearchQuery;
pub use query::SourceSearchRequest;
pub(crate) use semantic_port::{
    HistorySemanticError, HistorySemanticPort, SemanticAvailability, SemanticReason,
};
#[cfg(test)]
use test_support::collect_search_hits_with_port;
#[cfg(test)]
pub(super) use test_support::{
    collect_search_hits_with_backend, collect_search_hits_with_backend_using,
    search_existing_generation,
};

const MAX_USAGE_CONTEXT_EVENTS_PER_SESSION: usize = 256;
const SEMANTIC_GENERATION_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
type RefreshArg = RefreshMode;
pub(super) const MISSING_INDEX_ERROR: &str =
    "the Core index does not exist; retry with daemon refresh enabled";
const QUEUED_WITHOUT_GENERATION_ERROR: &str =
    "daemon source refresh was queued but no published generation exists; retry with --refresh wait";

#[derive(Debug)]
pub(super) enum SourceSearchFailure {
    Semantic(HistorySemanticError),
    SourceUnavailable,
    GenerationChanged,
    GenerationAuthority(ctx_history_refresh::GenerationQueryAuthorityError),
    Other(anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum McpSearchError {
    #[error("source-backed semantic search is not ready ({code}): {detail}")]
    SemanticNotReady {
        code: &'static str,
        detail: String,
        retryable: bool,
    },
    #[error("{detail}")]
    SemanticFailed { detail: String },
    #[error("source_unavailable")]
    SourceUnavailable,
    #[error(
        "History changed while ctx was opening the searchable generation. Retry the same request."
    )]
    GenerationChanged,
    #[error(transparent)]
    GenerationAuthority(ctx_history_refresh::GenerationQueryAuthorityError),
    #[error("{detail}")]
    Application { detail: String },
}

#[derive(Debug)]
pub struct McpSearchExecutionFailure {
    error: McpSearchError,
    observation: Box<SearchExecutionObservation>,
}

impl McpSearchExecutionFailure {
    pub fn into_parts(self) -> (McpSearchError, SearchExecutionObservation) {
        (self.error, *self.observation)
    }
}

impl SourceSearchFailure {
    pub(super) fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Semantic(error) => semantic_error_into_anyhow(error),
            Self::SourceUnavailable => {
                anyhow::Error::new(ctx_history_refresh::MissingActiveGeneration)
            }
            Self::GenerationChanged => {
                anyhow::Error::new(ctx_history_index::IndexError::ConcurrentGenerationChange)
            }
            Self::GenerationAuthority(error) => anyhow::Error::new(error),
            Self::Other(error) => error,
        }
    }

    fn into_mcp(self) -> McpSearchError {
        match self {
            Self::Semantic(HistorySemanticError::NotReady {
                reason,
                detail,
                retryable,
            }) => McpSearchError::SemanticNotReady {
                code: semantic_reason_code(reason),
                detail: semantic_external_detail(reason, &detail),
                retryable,
            },
            Self::Semantic(HistorySemanticError::Failed { detail }) => {
                McpSearchError::SemanticFailed { detail }
            }
            Self::SourceUnavailable => McpSearchError::SourceUnavailable,
            Self::GenerationChanged => McpSearchError::GenerationChanged,
            Self::GenerationAuthority(error) => McpSearchError::GenerationAuthority(error),
            Self::Other(error) => McpSearchError::Application {
                detail: error.to_string(),
            },
        }
    }
}

pub(super) fn semantic_error_into_anyhow(error: HistorySemanticError) -> anyhow::Error {
    match error {
        HistorySemanticError::NotReady {
            reason,
            detail,
            retryable,
        } => anyhow::Error::new(crate::semantic::SemanticNotReady::new_with_retryable(
            semantic_reason_code(reason),
            semantic_external_detail(reason, &detail),
            retryable,
        )),
        HistorySemanticError::Failed { detail } => anyhow::anyhow!(detail),
    }
}

pub(super) const fn semantic_reason_code(reason: SemanticReason) -> &'static str {
    match reason {
        SemanticReason::PolicyDisabled => "semantic_disabled",
        SemanticReason::PlatformUnsupported => "semantic_unsupported",
        SemanticReason::ExecutionUnavailable => "semantic_daemon_disabled",
        SemanticReason::ContentScopeUnsupported => "semantic_content_scope_unsupported",
        SemanticReason::EventTypeUnsupported => "semantic_event_type_unsupported",
        SemanticReason::QueryServiceUnavailable => "semantic_query_service_unavailable",
        SemanticReason::ExecutorUnavailable => "semantic_executor_unavailable",
        SemanticReason::ExecutorConfigurationInvalid => "semantic_executor_configuration_invalid",
        SemanticReason::StoreUnavailable => "semantic_store_unavailable",
        SemanticReason::StoreMissing => "semantic_store_missing",
        SemanticReason::GenerationUnreadable => "semantic_generation_unreadable",
        SemanticReason::GenerationNotAcknowledged => "semantic_generation_not_acknowledged",
        SemanticReason::GenerationReceiptMismatch => "semantic_generation_receipt_mismatch",
        SemanticReason::ProjectionEventMismatch => "semantic_projection_event_mismatch",
        SemanticReason::Adapter(code) => code,
    }
}

fn semantic_external_detail(reason: SemanticReason, detail: &str) -> String {
    match reason {
        SemanticReason::PolicyDisabled => "semantic search is disabled. Set [search] semantic = true in ctx config to enable local semantic search".to_owned(),
        SemanticReason::PlatformUnsupported => "local semantic search is not supported on this platform yet. Set [search] semantic = false or use --backend lexical".to_owned(),
        SemanticReason::ExecutionUnavailable => "local semantic search requires automatic indexing. Run `ctx index mode auto`, set [search] semantic = false, or use --backend lexical".to_owned(),
        SemanticReason::ContentScopeUnsupported => format!("{detail}; use --backend lexical or choose --content-scope all|transcript"),
        SemanticReason::EventTypeUnsupported => format!("{detail}; use --backend lexical or remove --event-type"),
        _ => detail.to_owned(),
    }
}

impl std::fmt::Display for SourceSearchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Semantic(error) => std::fmt::Display::fmt(error, formatter),
            Self::SourceUnavailable => std::fmt::Display::fmt(
                &ctx_history_refresh::MissingActiveGeneration,
                formatter,
            ),
            Self::GenerationChanged => formatter.write_str(
                "History changed while ctx was opening the searchable generation. Retry the same request.",
            ),
            Self::GenerationAuthority(error) => std::fmt::Display::fmt(error, formatter),
            Self::Other(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for SourceSearchFailure {}

impl From<HistorySemanticError> for SourceSearchFailure {
    fn from(error: HistorySemanticError) -> Self {
        Self::Semantic(error)
    }
}

impl From<anyhow::Error> for SourceSearchFailure {
    fn from(error: anyhow::Error) -> Self {
        let error = match error.downcast::<ctx_history_refresh::MissingActiveGeneration>() {
            Ok(_) => return Self::SourceUnavailable,
            Err(error) => error,
        };
        let error = match error.downcast::<ctx_history_index::IndexError>() {
            Ok(error) => return Self::from(error),
            Err(error) => error,
        };
        match error.downcast::<ctx_history_refresh::GenerationQueryAuthorityError>() {
            Ok(error) => Self::GenerationAuthority(error),
            Err(error) => Self::Other(externalize_query_error(error)),
        }
    }
}

impl From<std::io::Error> for SourceSearchFailure {
    fn from(error: std::io::Error) -> Self {
        Self::Other(anyhow::Error::new(error))
    }
}

impl From<ctx_history_index::IndexError> for SourceSearchFailure {
    fn from(error: ctx_history_index::IndexError) -> Self {
        match error {
            ctx_history_index::IndexError::ConcurrentGenerationChange => Self::GenerationChanged,
            other => Self::Other(anyhow::Error::new(other)),
        }
    }
}

impl From<ctx_history_read_application::SearchExecutionError> for SourceSearchFailure {
    fn from(error: ctx_history_read_application::SearchExecutionError) -> Self {
        match error {
            ctx_history_read_application::SearchExecutionError::Semantic(error) => {
                Self::Semantic(error)
            }
            ctx_history_read_application::SearchExecutionError::Index(error) => Self::from(error),
            ctx_history_read_application::SearchExecutionError::Application(error) => {
                Self::from(error)
            }
        }
    }
}

fn application_search_failure(
    error: ctx_history_read_application::ObservedSearchApplicationError<anyhow::Error>,
) -> SourceSearchFailure {
    use ctx_history_read_application::{GenerationReadError, SearchApplicationError};

    match error.into_error() {
        SearchApplicationError::Generation(failure) => match failure {
            GenerationReadError::Port(error) => SourceSearchFailure::from(error),
            GenerationReadError::Authority(error) => {
                SourceSearchFailure::Other(anyhow::Error::new(error))
            }
        },
        SearchApplicationError::Query(failure) => SourceSearchFailure::from(failure),
    }
}

type SourceSearchResult<T> = std::result::Result<T, SourceSearchFailure>;

pub(super) use ctx_history_read_application::{SearchCollection, SemanticFallbackDiagnostics};
#[cfg(test)]
pub(super) use ctx_history_read_application::{SearchEventMetadata, SearchHit, SearchResultWindow};

pub(super) struct RefreshOutcome {
    pub(super) pin: PinnedSourceBackedGeneration,
    pub(super) status: &'static str,
    pub(super) source_count: usize,
}

struct SearchRefreshContext<'a> {
    mode: RefreshArg,
    status: &'a str,
    source_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::source_index) enum ForegroundSemanticExecution {
    ReadOnly,
    Reconcile,
}

pub(in crate::source_index) fn foreground_semantic_execution(
    refresh_mode: RefreshArg,
    daemon_enabled: bool,
) -> Option<ForegroundSemanticExecution> {
    if daemon_enabled {
        return None;
    }
    match refresh_mode {
        RefreshArg::Off | RefreshArg::Background => Some(ForegroundSemanticExecution::ReadOnly),
        RefreshArg::Wait => Some(ForegroundSemanticExecution::Reconcile),
    }
}

pub(in crate::source_index) fn should_wait_for_daemon_query_service(
    refresh_mode: RefreshArg,
    daemon_enabled: bool,
) -> bool {
    refresh_mode == RefreshArg::Background && daemon_enabled
}

pub fn run_search(
    args: SearchArgs,
    data_root: PathBuf,
    config: HistoryCliConfig,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
    observe_query: impl FnOnce(SearchExecutionObservation),
) -> Result<()> {
    let human_output = args.format != JsonOutputFormat::Json;
    let config = config::AppConfig::from_snapshot(config);
    let request = crate::SearchRequest::from(args);
    let refresh_mode = request.refresh;
    let foreground_semantic = foreground_semantic_execution(refresh_mode, config.daemon.enabled);
    let semantic_port = match foreground_semantic {
        Some(ForegroundSemanticExecution::ReadOnly) => {
            crate::semantic::SemanticQueryAdapter::foreground_read_only(
                &data_root,
                config.semantic_embedding_executor().clone(),
            )
        }
        Some(ForegroundSemanticExecution::Reconcile) => {
            crate::semantic::SemanticQueryAdapter::foreground(
                &data_root,
                config.semantic_embedding_executor().clone(),
            )
        }
        None => crate::semantic::SemanticQueryAdapter::new(&data_root),
    };
    let json_output = request.format == crate::OutputFormat::Json;
    let verbose = request.verbose;
    let request = SourceSearchRequest::from(request);
    let policy = source_search_policy(&config, foreground_semantic.is_some());
    let mut observation = initial_search_observation();
    let result = run_search_inner(
        request,
        refresh_mode,
        json_output,
        verbose,
        data_root.clone(),
        config,
        policy,
        local_usage,
        ui,
        &semantic_port,
        &mut observation,
    )
    .map_err(SourceSearchFailure::into_anyhow);
    let rendering_error = result.is_err();
    let error_output_started = Instant::now();
    let result =
        render_search_error_observed(result, human_output, &data_root, ui, Some(&mut observation));
    if rendering_error {
        observation.output_duration = Some(
            observation
                .output_duration
                .unwrap_or_default()
                .saturating_add(error_output_started.elapsed()),
        );
    }
    observe_query(observation);
    result
}

#[allow(clippy::too_many_arguments)]
fn run_search_inner<P: HistorySemanticPort>(
    request: SourceSearchRequest,
    refresh_mode: RefreshArg,
    json_output: bool,
    verbose: bool,
    data_root: PathBuf,
    config: config::AppConfig,
    policy: ctx_history_read_application::SearchPolicy,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
    semantic_port: &P,
    observation: &mut SearchExecutionObservation,
) -> SourceSearchResult<()> {
    let compact_projection = compact_search_projection(json_output, verbose);
    let plan = ctx_history_read_application::plan_search(request, policy)?;
    let request = plan.request();
    let requested_backend = request.backend.unwrap_or(policy.default_backend);
    observation.backend_requested = Some(requested_backend);
    let semantic_weight = request.semantic_weight;
    let needs_semantic = search_needs_semantic_evidence(
        request,
        requested_backend,
        semantic_weight,
        policy.semantic,
    );
    if should_wait_for_daemon_query_service(refresh_mode, config.daemon.enabled) && needs_semantic {
        wait_for_daemon_query_service(&data_root, Duration::from_secs(3));
    }
    let mut refresh = observed_refresh_for_search(request, refresh_mode, &data_root, observation)?;
    if refresh_mode == RefreshArg::Wait && config.daemon.enabled && needs_semantic {
        refresh.pin = wait_for_daemon_semantic_generation(
            &data_root,
            refresh.pin,
            SEMANTIC_GENERATION_WAIT_TIMEOUT,
        )?;
    }
    let search_result = search_pinned_generation(
        plan,
        &data_root,
        refresh_mode,
        refresh,
        compact_projection,
        semantic_port,
        super::detected_active_session(),
        observation,
    );
    let (value, application) = search_result?;
    let collection = &application.query().collection;
    let index = application.index();
    if !json_output {
        if let Some(fallback) = collection.semantic_fallback.as_ref() {
            observation.failure_phase = Some(SearchFailurePhase::Output);
            let output_started = Instant::now();
            let warning = render_semantic_fallback_warning(ui.stderr_context(), fallback);
            let output_result = ui.write_stderr(&warning);
            observation.output_duration = Some(
                observation
                    .output_duration
                    .unwrap_or_default()
                    .saturating_add(output_started.elapsed()),
            );
            output_result.map_err(SourceSearchFailure::from)?;
        }
    }
    let results = value["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let result_count = results.len();
    let search_context = if config.local_usage.enabled {
        search_context_observation(&value, collection, index)
    } else {
        SearchContextObservation::unavailable()
    };
    observation.result_count = Some(result_count as u64);
    observation.citation_count = Some(collection.result_window.hits.len() as u64);
    observation.zero_result = Some(collection.result_window.hits.is_empty());
    observation.has_indexed_content_after = Some(index.document_count() > 0);

    observation.failure_phase = Some(SearchFailurePhase::ResultProjection);
    let render_started = Instant::now();
    let compact_value = match compact_projection
        .then(|| application.project_read_model(&value))
        .transpose()
    {
        Ok(value) => value,
        Err(error) => {
            observation.render_duration = Some(render_started.elapsed());
            return Err(SourceSearchFailure::from(error));
        }
    };
    let render_value = compact_value.as_ref().unwrap_or(&value);
    observation.failure_phase = Some(SearchFailurePhase::Render);
    enum RenderedOutput {
        Json(Vec<u8>),
        Human { document: Document, bytes: usize },
    }
    let rendered = if json_output {
        let mut bytes = match serde_json::to_vec_pretty(&value) {
            Ok(bytes) => bytes,
            Err(error) => {
                observation.render_duration = Some(render_started.elapsed());
                return Err(SourceSearchFailure::from(anyhow::Error::new(error)));
            }
        };
        bytes.push(b'\n');
        RenderedOutput::Json(bytes)
    } else {
        let document = render_search_document(render_value, verbose, ui.stdout_context());
        let bytes = canonical_human_output_bytes(|context| {
            render_search_document(render_value, verbose, context)
        });
        RenderedOutput::Human { document, bytes }
    };
    observation.render_duration = Some(render_started.elapsed());

    observation.failure_phase = Some(SearchFailurePhase::Output);
    let output_started = Instant::now();
    let output_result = (|| -> std::io::Result<usize> {
        let output_bytes = match &rendered {
            RenderedOutput::Json(bytes) => {
                ui.write_stdout_bytes(bytes)?;
                bytes.len()
            }
            RenderedOutput::Human { document, bytes } => {
                ui.write_stdout(document)?;
                *bytes
            }
        };
        Ok(output_bytes)
    })();
    observation.output_duration = Some(
        observation
            .output_duration
            .unwrap_or_default()
            .saturating_add(output_started.elapsed()),
    );
    let output_bytes = output_result.map_err(SourceSearchFailure::from)?;
    observation.failure_phase = None;
    local_usage.set_result_observation(ResultObservationAction::Search, result_count, 0);
    local_usage.set_search_context_observation(search_context);
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(())
}

pub(super) fn search_needs_semantic_evidence(
    request: &SourceSearchRequest,
    requested_backend: SearchBackend,
    semantic_weight: f32,
    availability: SemanticAvailability,
) -> bool {
    availability == SemanticAvailability::Available
        && matches!(
            requested_backend,
            SearchBackend::Semantic | SearchBackend::Hybrid
        )
        && unsupported_semantic_scope(request).is_none()
        && !(requested_backend == SearchBackend::Hybrid && semantic_weight == 0.0)
}

pub(super) const fn compact_search_projection(json_output: bool, verbose: bool) -> bool {
    !json_output && !verbose
}

#[cfg(test)]
pub(super) fn render_search_error<T>(
    result: Result<T>,
    human_output: bool,
    data_root: &Path,
    ui: &mut Ui,
) -> Result<T> {
    render_search_error_observed(result, human_output, data_root, ui, None)
}

fn render_search_error_observed<T>(
    result: Result<T>,
    human_output: bool,
    data_root: &Path,
    ui: &mut Ui,
    mut observation: Option<&mut SearchExecutionObservation>,
) -> Result<T> {
    let rendering_active_generation_race = result.as_ref().is_err_and(is_active_generation_race);
    let result = render_active_generation_race(
        result,
        !human_output,
        ActiveGenerationRaceCommand::Search,
        ui,
    );
    if rendering_active_generation_race
        && result
            .as_ref()
            .is_err_and(|error| !error.is::<crate::RenderedCliError>())
    {
        if let Some(observation) = observation.as_deref_mut() {
            observation.failure_phase = Some(SearchFailurePhase::Output);
        }
        return result;
    }
    match result {
        Ok(value) => Ok(value),
        Err(error) if human_output && search_index_is_not_ready(data_root, &error) => {
            render_not_ready_at_search_boundary(ui, observation)?;
            Err(crate::dispatch::rendered_cli_error())
        }
        Err(error) => Err(error),
    }
}

pub(super) fn render_not_ready_at_search_boundary(
    ui: &mut Ui,
    observation: Option<&mut SearchExecutionObservation>,
) -> Result<()> {
    let document = render_search_not_ready_document(ui.stderr_context());
    if let Err(error) = ui.write_stderr(&document) {
        if let Some(observation) = observation {
            observation.failure_phase = Some(SearchFailurePhase::Output);
        }
        return Err(error.into());
    }
    Ok(())
}

fn search_index_is_not_ready(data_root: &Path, error: &anyhow::Error) -> bool {
    let missing_generation = error.chain().any(|cause| {
        matches!(
            cause.to_string().as_str(),
            MISSING_INDEX_ERROR | QUEUED_WITHOUT_GENERATION_ERROR
        )
    });
    let root = index_root(data_root);
    let active_generation_missing = VerifiedIndex::active_generation_id(&root)
        .ok()
        .flatten()
        .is_none();
    missing_generation
        || (active_generation_missing
            && error
                .downcast_ref::<SourceBackedRefreshDaemonUnavailable>()
                .is_some())
}

pub(super) fn render_semantic_fallback_warning(
    context: &RenderContext,
    fallback: &SemanticFallbackDiagnostics,
) -> Document {
    let (summary, detail, action) = match fallback.reason {
        Some(SemanticReason::PolicyDisabled) => (
            "Semantic search is unavailable",
            "Keyword search was used because semantic search is disabled.",
            "ctx semantic enable",
        ),
        Some(SemanticReason::ContentScopeUnsupported | SemanticReason::EventTypeUnsupported) => (
            "Semantic search does not support this filter",
            "Keyword search was used because this content filter is lexical-only.",
            "ctx search \"<term>\" --backend lexical",
        ),
        _ => (
            "Semantic search is unavailable",
            "Keyword search was used because semantic retrieval did not complete.",
            "ctx doctor",
        ),
    };
    diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Warning,
            summary,
            detail: Some(detail),
            fields: &[],
            action: Some(Action { command: action }),
        },
    )
}

#[cfg(test)]
pub(crate) fn mcp_search(
    request: SourceSearchRequest,
    data_root: &Path,
    config: HistoryCliConfig,
) -> std::result::Result<(Value, SearchContextObservation), McpSearchError> {
    mcp_search_with_compact(request, data_root, config)
        .map(|(value, context, _, _)| (value, context))
        .map_err(|failure| failure.into_parts().0)
}

pub fn mcp_search_with_compact(
    request: SourceSearchRequest,
    data_root: &Path,
    config: HistoryCliConfig,
) -> std::result::Result<
    (
        Value,
        SearchContextObservation,
        Value,
        SearchExecutionObservation,
    ),
    McpSearchExecutionFailure,
> {
    let config = config::AppConfig::from_snapshot(config);
    let semantic_port = crate::semantic::SemanticQueryAdapter::new(data_root);
    let policy = source_search_policy(&config, false);
    let mut observation = initial_search_observation();
    match mcp_search_inner(
        request,
        data_root,
        &config,
        policy,
        &semantic_port,
        &mut observation,
    ) {
        Ok((value, context, compact)) => Ok((value, context, compact, observation)),
        Err(error) => Err(McpSearchExecutionFailure {
            error: error.into_mcp(),
            observation: Box::new(observation),
        }),
    }
}

pub fn normalize_mcp_search_request(
    request: &mut SourceSearchRequest,
) -> std::result::Result<(), McpSearchError> {
    ctx_history_read_application::normalize_search_request(request)
        .map_err(|error| SourceSearchFailure::from(error).into_mcp())
}

fn mcp_search_inner<P: HistorySemanticPort>(
    request: SourceSearchRequest,
    data_root: &Path,
    config: &config::AppConfig,
    policy: ctx_history_read_application::SearchPolicy,
    semantic_port: &P,
    observation: &mut SearchExecutionObservation,
) -> SourceSearchResult<(Value, SearchContextObservation, Value)> {
    let plan = ctx_history_read_application::plan_search(request, policy)?;
    let requested_backend = plan.request().backend.unwrap_or(policy.default_backend);
    observation.backend_requested = Some(requested_backend);
    let refresh =
        observed_refresh_for_search(plan.request(), RefreshArg::Off, data_root, observation)?;
    let result = search_pinned_generation(
        plan,
        data_root,
        RefreshArg::Off,
        refresh,
        true,
        semantic_port,
        None,
        observation,
    );
    let (value, application) = result?;
    let collection = &application.query().collection;
    let context = if config.local_usage.enabled {
        search_context_observation(&value, collection, application.index())
    } else {
        SearchContextObservation::unavailable()
    };
    observation.result_count = value["results"]
        .as_array()
        .map(|results| results.len() as u64);
    observation.citation_count = Some(collection.result_window.hits.len() as u64);
    observation.zero_result = Some(collection.result_window.hits.is_empty());
    observation.has_indexed_content_after = Some(application.index().document_count() > 0);
    observation.failure_phase = Some(SearchFailurePhase::ResultProjection);
    let compact_value = match application.project_read_model(&value) {
        Ok(value) => value,
        Err(error) => return Err(SourceSearchFailure::from(error)),
    };
    observation.failure_phase = None;
    Ok((value, context, compact_value))
}

pub fn validate_explicit_semantic_scope(
    request: &SourceSearchRequest,
) -> std::result::Result<(), McpSearchError> {
    if request.backend == Some(SearchBackend::Semantic) {
        if let Some(not_ready) = unsupported_semantic_scope(request) {
            return Err(SourceSearchFailure::Semantic(not_ready).into_mcp());
        }
    }
    Ok(())
}

pub(super) fn search_context_observation(
    value: &Value,
    collection: &SearchCollection,
    index: &VerifiedIndex,
) -> SearchContextObservation {
    if collection.result_window.hits.is_empty() {
        return SearchContextObservation::unavailable();
    }
    let Some(delivered_context_bytes) =
        value
            .get("results")
            .and_then(Value::as_array)
            .and_then(|results| {
                results.iter().try_fold(0_usize, |total, result| {
                    total.checked_add(result.get("snippet")?.as_str()?.len())
                })
            })
    else {
        return SearchContextObservation::unavailable();
    };
    let session_ids = collection
        .result_window
        .hits
        .iter()
        .map(|hit| hit.event.session_id)
        .collect::<BTreeSet<_>>();
    let mut matched_normalized_session_bytes = 0_usize;
    for session_id in session_ids {
        let Ok(Some(session_bytes)) = index.core_content_bytes_for_session_if_bounded(
            session_id,
            MAX_USAGE_CONTEXT_EVENTS_PER_SESSION,
        ) else {
            return SearchContextObservation::unavailable();
        };
        let Some(total) = matched_normalized_session_bytes.checked_add(session_bytes) else {
            return SearchContextObservation::unavailable();
        };
        matched_normalized_session_bytes = total;
    }
    SearchContextObservation::complete(delivered_context_bytes, matched_normalized_session_bytes)
        .unwrap_or_else(SearchContextObservation::unavailable)
}

pub(super) fn refresh_for_search(
    request: &SourceSearchRequest,
    refresh: RefreshArg,
    data_root: &Path,
) -> SourceSearchResult<RefreshOutcome> {
    refresh_for_search_with(
        request,
        refresh,
        data_root,
        coordinate_source_backed_refresh,
    )
}

pub(super) fn refresh_for_search_with<Coordinate>(
    request: &SourceSearchRequest,
    refresh: RefreshArg,
    data_root: &Path,
    coordinate: Coordinate,
) -> SourceSearchResult<RefreshOutcome>
where
    Coordinate: FnOnce(&Path, SourceBackedRefreshMode) -> Result<SourceBackedRefreshObservation>,
{
    ctx_history_read_application::validate_search_request(request)?;
    let mode = source_backed_refresh_mode(refresh);
    let observation = match coordinate(data_root, mode) {
        Ok(observation) => observation,
        Err(error) if mode == SourceBackedRefreshMode::Background => {
            // Background refresh may report an uncertified empty generation as
            // unavailable. At the query gateway, preserve the stricter typed
            // R1 authority error instead of replacing it with refresh state.
            if let Err(authority_error) = crate::semantic::pin_active_verified_generation(data_root)
            {
                if let Ok(authority_error) =
                    authority_error.downcast::<ctx_history_refresh::GenerationQueryAuthorityError>()
                {
                    return Err(SourceSearchFailure::GenerationAuthority(authority_error));
                }
            }
            return Err(SourceSearchFailure::from(error));
        }
        Err(error) => return Err(SourceSearchFailure::from(error)),
    };
    if observation.mode != mode {
        return Err(anyhow!(
            "source-backed refresh coordinator returned mode {:?} for requested mode {:?}",
            observation.mode,
            mode
        )
        .into());
    }
    let status = match mode {
        SourceBackedRefreshMode::Off => "existing_generation",
        SourceBackedRefreshMode::Background if observation.daemon_available => "daemon_background",
        SourceBackedRefreshMode::Background => "daemon_unavailable",
        SourceBackedRefreshMode::Wait => "completed",
    };
    Ok(RefreshOutcome {
        pin: observation.pin,
        status,
        source_count: observation.source_count,
    })
}

pub(super) fn source_backed_refresh_mode(refresh: RefreshArg) -> SourceBackedRefreshMode {
    match refresh {
        RefreshArg::Off => SourceBackedRefreshMode::Off,
        RefreshArg::Background => SourceBackedRefreshMode::Background,
        RefreshArg::Wait => SourceBackedRefreshMode::Wait,
    }
}

#[allow(clippy::too_many_arguments)]
fn search_pinned_generation<P: HistorySemanticPort>(
    plan: ctx_history_read_application::PlannedSearch,
    data_root: &Path,
    refresh_mode: RefreshArg,
    refresh: RefreshOutcome,
    compact_projection: bool,
    semantic_port: &P,
    active_session: Option<ctx_history_read_application::ActiveSessionExclusion>,
    observation: &mut SearchExecutionObservation,
) -> SourceSearchResult<(Value, ctx_history_read_application::SearchApplicationResult)> {
    let RefreshOutcome {
        pin,
        status,
        source_count,
    } = refresh;
    observation.failure_phase = Some(SearchFailurePhase::QueryPreparation);
    search_existing_generation_with_port(
        plan,
        pin.into_index(),
        data_root,
        SearchRefreshContext {
            mode: refresh_mode,
            status,
            source_count,
        },
        compact_projection,
        semantic_port,
        active_session,
        observation,
    )
}

#[cfg(test)]
pub(super) fn collect_search_hits_with_semantic_availability(
    request: &SourceSearchRequest,
    data_root: &Path,
    semantic: SemanticAvailability,
) -> Result<SearchCollection> {
    collect_search_hits_with_port(
        request,
        data_root,
        request.semantic_weight,
        semantic,
        &crate::semantic::SemanticQueryAdapter::new(data_root),
    )
}
