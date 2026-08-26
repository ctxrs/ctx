use std::{path::Path, time::Instant};

use ctx_history_index::VerifiedIndex;
use ctx_history_read_application::{
    SearchCollection, SearchFailurePhase as ApplicationFailurePhase,
};
use serde_json::Value;

use super::super::{compact_presentation::generation_read, render::search_json_document};
use super::{
    application_search_failure, index_root, refresh_for_search, HistorySemanticPort, RefreshArg,
    RefreshOutcome, SearchRefreshContext, SourceSearchRequest, SourceSearchResult,
};
use crate::{SearchExecutionObservation, SearchFailurePhase, SearchRefreshStatus};

pub(super) fn initial_search_observation() -> SearchExecutionObservation {
    SearchExecutionObservation {
        failure_phase: Some(SearchFailurePhase::Preparation),
        ..SearchExecutionObservation::default()
    }
}

fn search_refresh_status(status: &str) -> SearchRefreshStatus {
    match status {
        "existing_generation" => SearchRefreshStatus::ExistingGeneration,
        "daemon_background" => SearchRefreshStatus::DaemonBackground,
        "daemon_unavailable" => SearchRefreshStatus::DaemonUnavailable,
        _ => SearchRefreshStatus::Completed,
    }
}

pub(super) fn observed_refresh_for_search(
    request: &SourceSearchRequest,
    mode: RefreshArg,
    data_root: &Path,
    observation: &mut SearchExecutionObservation,
) -> SourceSearchResult<RefreshOutcome> {
    observation.failure_phase = Some(SearchFailurePhase::Refresh);
    let started = Instant::now();
    let result = refresh_for_search(request, mode, data_root);
    observation.refresh_duration = Some(started.elapsed());
    match result {
        Ok(refresh) => {
            observation.refresh_status = Some(search_refresh_status(refresh.status));
            observation.refresh_source_count = Some(refresh.source_count as u64);
            Ok(refresh)
        }
        Err(error) => {
            observation.refresh_status = Some(SearchRefreshStatus::Failed);
            Err(error)
        }
    }
}

pub(super) const fn observed_failure_phase(value: ApplicationFailurePhase) -> SearchFailurePhase {
    match value {
        ApplicationFailurePhase::GenerationOpen => SearchFailurePhase::GenerationOpen,
        ApplicationFailurePhase::QueryPreparation => SearchFailurePhase::QueryPreparation,
        ApplicationFailurePhase::SemanticRetrieval => SearchFailurePhase::SemanticRetrieval,
        ApplicationFailurePhase::IndexQueryDecode => SearchFailurePhase::IndexQueryDecode,
        ApplicationFailurePhase::ResultProjection => SearchFailurePhase::ResultProjection,
    }
}

pub(super) fn observe_search_collection(
    observation: &mut SearchExecutionObservation,
    collection: &SearchCollection,
) {
    observation.backend_requested = Some(collection.requested_backend);
    observation.backend_effective = Some(collection.effective_backend);
    observation.work = collection.work;
    observation.final_candidate_pool = u64::try_from(collection.candidate_pool).ok();
    observation.candidate_pool_truncated = Some(collection.candidate_pool_truncated);
    observation.concentration = Some(collection.concentration);
    observation.diversification = Some(collection.diversification);
    observation.stop_reason = collection.stop_reason;
    observation.failure_phase = None;
}

#[allow(clippy::too_many_arguments)]
pub(super) fn search_existing_generation_with_port<P: HistorySemanticPort>(
    plan: ctx_history_read_application::PlannedSearch,
    index: VerifiedIndex,
    data_root: &Path,
    refresh: SearchRefreshContext<'_>,
    compact_projection: bool,
    semantic_port: &P,
    active_session: Option<ctx_history_read_application::ActiveSessionExclusion>,
    observation: &mut SearchExecutionObservation,
) -> SourceSearchResult<(Value, ctx_history_read_application::SearchApplicationResult)> {
    let mut index = Some(index);
    let mut generation_port = |request: &ctx_history_read_application::GenerationReadRequest| {
        generation_read(
            index.take().expect("generation port is invoked once"),
            &index_root(data_root),
            request,
        )
    };
    let result = ctx_history_read_application::execute_search_observed(
        ctx_history_read_application::SearchApplicationRequest {
            plan,
            generation_target: ctx_history_read_application::GenerationReadTarget::Active,
            compact_projection,
            active_session,
        },
        &mut generation_port,
        semantic_port,
    )
    .map_err(|error| {
        observation.work = error.work();
        observation.query_duration = error.query_duration();
        observation.failure_phase = Some(observed_failure_phase(error.failure_phase()));
        application_search_failure(error)
    })?;
    observation.query_duration = Some(result.query_duration());
    let query = result.query();
    observe_search_collection(observation, &query.collection);
    observation.failure_phase = Some(SearchFailurePhase::ResultProjection);
    let value = search_json_document(
        &query.request,
        data_root,
        result.index(),
        &query.collection,
        &query.filters,
        &query.presentations,
        refresh.mode,
        refresh.status,
        refresh.source_count,
        result.query_duration(),
    )?;
    observation.failure_phase = None;
    Ok((value, result))
}
