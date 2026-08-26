use std::sync::Mutex;

use ctx_history_index::EventSearchCandidate;
use ctx_history_read_application::HistorySemanticQuery;

use super::semantic_port::HistorySemanticBatch;
use super::{
    index_root, initial_search_observation, search_existing_generation_with_port,
    HistorySemanticError, HistorySemanticPort, Path, RefreshArg, Result, SearchBackend,
    SearchCollection, SearchRefreshContext, SemanticAvailability, SourceSearchFailure,
    SourceSearchRequest, Value, VerifiedIndex,
};

pub(in crate::source_index) fn search_existing_generation(
    request: &SourceSearchRequest,
    index: VerifiedIndex,
    data_root: &Path,
    semantic_weight: f32,
    refresh_status: &str,
    refresh_source_count: usize,
) -> Result<(Value, SearchCollection, VerifiedIndex)> {
    let policy = ctx_history_read_application::SearchPolicy {
        default_backend: request.backend.unwrap_or(SearchBackend::Lexical),
        semantic: SemanticAvailability::Available,
    };
    let mut request = request.clone();
    request.semantic_weight = semantic_weight;
    let plan = ctx_history_read_application::plan_search(request, policy)
        .map_err(SourceSearchFailure::from)
        .map_err(SourceSearchFailure::into_anyhow)?;
    let mut observation = initial_search_observation();
    let requested_backend = plan.request().backend.unwrap_or(policy.default_backend);
    observation.backend_requested = Some(requested_backend);
    search_existing_generation_with_port(
        plan,
        index,
        data_root,
        SearchRefreshContext {
            mode: RefreshArg::Off,
            status: refresh_status,
            source_count: refresh_source_count,
        },
        false,
        &crate::semantic::SemanticQueryAdapter::new(data_root),
        None,
        &mut observation,
    )
    .map(|(value, application)| {
        let (query, index) = application.into_parts();
        (value, query.collection, index)
    })
    .map_err(SourceSearchFailure::into_anyhow)
}

pub(in crate::source_index) fn collect_search_hits_with_backend(
    request: &SourceSearchRequest,
    data_root: &Path,
    semantic_weight: f32,
) -> Result<SearchCollection> {
    collect_search_hits_with_port(
        request,
        data_root,
        semantic_weight,
        SemanticAvailability::Available,
        &crate::semantic::SemanticQueryAdapter::new(data_root),
    )
}

pub(super) fn collect_search_hits_with_port<P: HistorySemanticPort>(
    request: &SourceSearchRequest,
    data_root: &Path,
    semantic_weight: f32,
    semantic: SemanticAvailability,
    semantic_port: &P,
) -> Result<SearchCollection> {
    let mut planned = request.clone();
    planned.semantic_weight = semantic_weight;
    let policy = ctx_history_read_application::SearchPolicy {
        default_backend: planned.backend.unwrap_or(SearchBackend::Lexical),
        semantic,
    };
    let plan = ctx_history_read_application::plan_search(planned, policy)
        .map_err(SourceSearchFailure::from)
        .map_err(SourceSearchFailure::into_anyhow)?;
    let index = VerifiedIndex::open_pinned(index_root(data_root))?;
    let mut observation = initial_search_observation();
    let (_, application) = search_existing_generation_with_port(
        plan,
        index,
        data_root,
        SearchRefreshContext {
            mode: RefreshArg::Off,
            status: "existing_generation",
            source_count: 1,
        },
        false,
        semantic_port,
        None,
        &mut observation,
    )
    .map_err(SourceSearchFailure::into_anyhow)?;
    Ok(application.into_parts().0.collection)
}

struct ClosureSemanticPort<'root, SemanticSearch> {
    data_root: &'root Path,
    search: Mutex<SemanticSearch>,
}

struct ClosureSemanticQuery<'query, SemanticSearch> {
    index: &'query VerifiedIndex,
    data_root: &'query Path,
    search: &'query Mutex<SemanticSearch>,
    queries: Vec<String>,
}

impl<SemanticSearch> HistorySemanticPort for ClosureSemanticPort<'_, SemanticSearch>
where
    SemanticSearch: FnMut(
            &VerifiedIndex,
            &Path,
            &[&str],
            &ctx_history_index::CompiledSearchFilter,
            usize,
        ) -> Result<(Vec<EventSearchCandidate>, Value)>
        + Send,
{
    type Query<'a>
        = ClosureSemanticQuery<'a, SemanticSearch>
    where
        Self: 'a;

    fn begin_query<'a>(
        &'a self,
        index: &'a VerifiedIndex,
    ) -> std::result::Result<Self::Query<'a>, HistorySemanticError> {
        Ok(ClosureSemanticQuery {
            index,
            data_root: self.data_root,
            search: &self.search,
            queries: Vec::new(),
        })
    }
}

impl<SemanticSearch> HistorySemanticQuery for ClosureSemanticQuery<'_, SemanticSearch>
where
    SemanticSearch: FnMut(
            &VerifiedIndex,
            &Path,
            &[&str],
            &ctx_history_index::CompiledSearchFilter,
            usize,
        ) -> Result<(Vec<EventSearchCandidate>, Value)>
        + Send,
{
    fn prepare_alternative(
        &mut self,
        query: &str,
    ) -> std::result::Result<Value, HistorySemanticError> {
        self.queries.push(query.to_owned());
        Ok(Value::Null)
    }

    fn candidates(
        &mut self,
        filter: &ctx_history_index::CompiledSearchFilter,
        candidate_limit: usize,
    ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError> {
        let queries = self.queries.iter().map(String::as_str).collect::<Vec<_>>();
        (self.search.lock().unwrap())(
            self.index,
            self.data_root,
            &queries,
            filter,
            candidate_limit,
        )
        .map(|(candidates, diagnostics)| HistorySemanticBatch {
            candidates,
            diagnostics,
        })
        .map_err(|error| HistorySemanticError::failed(format!("{error:#}")))
    }
}

pub(in crate::source_index) fn collect_search_hits_with_backend_using<SemanticSearch>(
    request: &SourceSearchRequest,
    data_root: &Path,
    semantic_weight: f32,
    semantic_search: SemanticSearch,
) -> Result<SearchCollection>
where
    SemanticSearch: FnMut(
            &VerifiedIndex,
            &Path,
            &[&str],
            &ctx_history_index::CompiledSearchFilter,
            usize,
        ) -> Result<(Vec<EventSearchCandidate>, Value)>
        + Send,
{
    collect_search_hits_with_port(
        request,
        data_root,
        semantic_weight,
        SemanticAvailability::Available,
        &ClosureSemanticPort {
            data_root,
            search: Mutex::new(semantic_search),
        },
    )
}
