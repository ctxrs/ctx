use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Result};
use ctx_history_core::{
    CertifiedSource, ScannedSourceCounts, SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_history_index_query::{CompiledSearchFilter, SearchContentScope, VerifiedIndex};
use serde_json::json;

use super::*;
use crate::{
    plan_search, resolve_search_backend, PinnedHistoryQuery, SearchBackend, SearchCollection,
    SearchExecutionError, SearchPolicy, SearchRequest,
};

#[derive(Clone, Default)]
struct CallLog(Arc<Mutex<Vec<String>>>);

impl CallLog {
    fn push(&self, value: impl Into<String>) {
        self.0.lock().unwrap().push(value.into());
    }

    fn values(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

struct FakeSemanticPort {
    calls: CallLog,
    begin_error: Option<HistorySemanticError>,
    prepare_error: Option<(String, HistorySemanticError)>,
}

impl FakeSemanticPort {
    fn ready(calls: CallLog) -> Self {
        Self {
            calls,
            begin_error: None,
            prepare_error: None,
        }
    }

    fn failing(calls: CallLog, error: HistorySemanticError) -> Self {
        Self {
            calls,
            begin_error: Some(error),
            prepare_error: None,
        }
    }

    fn failing_alternative(
        calls: CallLog,
        query: impl Into<String>,
        error: HistorySemanticError,
    ) -> Self {
        Self {
            calls,
            begin_error: None,
            prepare_error: Some((query.into(), error)),
        }
    }
}

impl HistorySemanticPort for FakeSemanticPort {
    type Query<'a> = FakeSemanticQuery;

    fn begin_query<'a>(
        &'a self,
        _index: &'a VerifiedIndex,
    ) -> std::result::Result<Self::Query<'a>, HistorySemanticError> {
        self.calls.push("begin_query");
        if let Some(error) = self.begin_error.as_ref() {
            return Err(error.clone());
        }
        Ok(FakeSemanticQuery {
            calls: self.calls.clone(),
            prepare_error: self.prepare_error.clone(),
        })
    }
}

struct FakeSemanticQuery {
    calls: CallLog,
    prepare_error: Option<(String, HistorySemanticError)>,
}

impl HistorySemanticQuery for FakeSemanticQuery {
    fn prepare_alternative(
        &mut self,
        query: &str,
    ) -> std::result::Result<Value, HistorySemanticError> {
        self.calls.push(format!("prepare:{query}"));
        if let Some((failed_query, error)) = self.prepare_error.as_ref() {
            if query == failed_query {
                return Err(error.clone());
            }
        }
        Ok(json!({"fake_query": query}))
    }

    fn candidates(
        &mut self,
        _filter: &CompiledSearchFilter,
        candidate_limit: usize,
    ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError> {
        self.calls.push(format!("candidates:{candidate_limit}"));
        Ok(HistorySemanticBatch {
            candidates: Vec::new(),
            diagnostics: json!({"fake_scan": true}),
        })
    }
}

fn empty_index(root: &Path) -> Result<VerifiedIndex> {
    let source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("semantic-port-empty.jsonl")?,
        )?,
    )?;
    let index_root = root.join("index");
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default())?
        .into_writer()
        .map_err(|recovery| {
            anyhow!(
                "semantic-port test index requires recovery for generation {}: {}",
                recovery.generation_id(),
                recovery.detail()
            )
        })?;
    writer.begin_source(source.clone())?;
    let observation = SourceObservation::new(source, "regular-file-v1", vec![1])?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "semantic-port-test-parser-v1",
        [1; 32],
        ScannedSourceCounts::default(),
    )?)?;
    writer.commit(|_| true)?;
    Ok(VerifiedIndex::open_pinned(&index_root)?)
}

fn semantic_request(backend: SearchBackend) -> SearchRequest {
    SearchRequest {
        query: "first query".to_owned(),
        terms: vec!["second query".to_owned()],
        limit: 10,
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
        content_scope: SearchContentScope::All,
        event_type: None,
        file: None,
        session: None,
        exclude_sessions: Vec::new(),
        events: false,
        include_current_session: true,
        backend: Some(backend),
        semantic_weight: 0.35,
    }
}

fn execute_semantic_search<P: HistorySemanticPort>(
    request: &SearchRequest,
    index: &VerifiedIndex,
    semantic_port: &P,
) -> std::result::Result<SearchCollection, SearchExecutionError> {
    let policy = SearchPolicy {
        default_backend: request.backend.unwrap_or(SearchBackend::Lexical),
        semantic: SemanticAvailability::Available,
    };
    let plan = plan_search(request.clone(), policy)?;
    PinnedHistoryQuery::new(index, None)
        .search(plan, None, semantic_port)
        .map(|query| query.collection)
        .map_err(|failure| *failure.error)
}

#[test]
fn fake_port_begins_once_before_ordered_query_calls() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index = empty_index(temp.path())?;
    let calls = CallLog::default();
    let port = FakeSemanticPort::ready(calls.clone());
    let request = semantic_request(SearchBackend::Hybrid);

    let collection = execute_semantic_search(&request, &index, &port)?;

    assert_eq!(collection.semantic_status, "ready");
    assert_eq!(collection.work.retrieval_rounds, Some(2));
    assert_eq!(collection.candidate_pool, 0);
    assert!(!collection.candidate_pool_truncated);
    assert_eq!(
        collection.stop_reason,
        Some(crate::SearchStopReason::FixedPool)
    );
    assert_eq!(collection.semantic_diagnostics.unwrap()["query_count"], 2);
    assert_eq!(
        calls.values(),
        vec![
            "begin_query",
            "prepare:first query",
            "prepare:second query",
            "candidates:1600",
        ]
    );
    Ok(())
}

#[test]
fn ordered_prepare_failure_preserves_prefix_and_never_starts_vector_scan() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index = empty_index(temp.path())?;
    let calls = CallLog::default();
    let port = FakeSemanticPort::failing_alternative(
        calls.clone(),
        "second query",
        HistorySemanticError::not_ready(
            SemanticReason::QueryServiceUnavailable,
            "second embedding failed",
            true,
        ),
    );
    let request = semantic_request(SearchBackend::Hybrid);

    let collection = execute_semantic_search(&request, &index, &port)?;

    assert_eq!(collection.effective_backend, SearchBackend::Lexical);
    assert_eq!(collection.semantic_status, "unavailable");
    assert_eq!(collection.work.retrieval_rounds, Some(1));
    let diagnostics = collection
        .semantic_diagnostics
        .expect("hybrid fallback diagnostics");
    assert_eq!(diagnostics["query_count"], 2);
    assert_eq!(diagnostics["queries"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        diagnostics["queries"][0]["diagnostics"]["fake_query"],
        "first query"
    );
    assert_eq!(
        calls.values(),
        vec!["begin_query", "prepare:first query", "prepare:second query",]
    );
    Ok(())
}

#[test]
fn backend_resolution_uses_caller_supplied_capability_policy() -> Result<()> {
    let calls = CallLog::default();
    let request = semantic_request(SearchBackend::Semantic);

    assert_eq!(
        resolve_search_backend(&request, SearchPolicy::semantic_available())?,
        SearchBackend::Semantic
    );
    assert!(calls.values().is_empty());
    Ok(())
}

#[test]
fn semantic_only_preserves_the_typed_port_error() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index = empty_index(temp.path())?;
    let calls = CallLog::default();
    let port = FakeSemanticPort::failing(
        calls.clone(),
        HistorySemanticError::not_ready(
            SemanticReason::Adapter("semantic_fixture_not_ready"),
            "fixture unavailable",
            true,
        ),
    );
    let request = semantic_request(SearchBackend::Semantic);

    let error = execute_semantic_search(&request, &index, &port).unwrap_err();
    let SearchExecutionError::Semantic(typed) = error else {
        panic!("semantic-only search must preserve the typed port failure");
    };
    assert_eq!(
        typed.reason(),
        Some(SemanticReason::Adapter("semantic_fixture_not_ready"))
    );
    assert_eq!(typed.detail(), "fixture unavailable");
    assert!(typed.retryable());
    assert_eq!(calls.values(), vec!["begin_query"]);
    Ok(())
}

#[test]
fn hybrid_maps_typed_port_failure_to_lexical_fallback() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index = empty_index(temp.path())?;
    let calls = CallLog::default();
    let port = FakeSemanticPort::failing(
        calls.clone(),
        HistorySemanticError::failed("fixture transport failed"),
    );
    let request = semantic_request(SearchBackend::Hybrid);

    let collection = execute_semantic_search(&request, &index, &port)?;

    assert_eq!(collection.effective_backend, SearchBackend::Lexical);
    assert_eq!(collection.semantic_status, "unavailable");
    let fallback = collection.semantic_fallback.unwrap();
    assert_eq!(fallback.reason, None);
    assert_eq!(fallback.detail, "fixture transport failed");
    assert_eq!(calls.values(), vec!["begin_query"]);
    Ok(())
}

#[test]
fn zero_weight_hybrid_never_opens_the_semantic_port() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index = empty_index(temp.path())?;
    let calls = CallLog::default();
    let port = FakeSemanticPort::ready(calls.clone());
    let mut request = semantic_request(SearchBackend::Hybrid);
    request.semantic_weight = 0.0;

    let collection = execute_semantic_search(&request, &index, &port)?;

    assert_eq!(collection.semantic_status, "skipped");
    assert!(calls.values().is_empty());
    Ok(())
}
