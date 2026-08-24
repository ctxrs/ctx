use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Result};
use ctx_history_core::{
    CertifiedSource, ScannedSourceCounts, SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_history_index_query::{EventSearchFilters, SearchContentScope, VerifiedIndex};
use serde_json::json;

use super::*;
use crate::{
    collect_search_hits, resolve_search_backend, SearchBackend, SearchExecutionError, SearchPolicy,
    SearchRequest,
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
}

impl FakeSemanticPort {
    fn ready(calls: CallLog) -> Self {
        Self {
            calls,
            begin_error: None,
        }
    }

    fn failing(calls: CallLog, error: HistorySemanticError) -> Self {
        Self {
            calls,
            begin_error: Some(error),
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
        })
    }
}

struct FakeSemanticQuery {
    calls: CallLog,
}

impl HistorySemanticQuery for FakeSemanticQuery {
    fn candidates(
        &mut self,
        query: &str,
        _filters: &EventSearchFilters,
        candidate_limit: usize,
    ) -> std::result::Result<HistorySemanticBatch, HistorySemanticError> {
        self.calls
            .push(format!("candidates:{query}:{candidate_limit}"));
        Ok(HistorySemanticBatch {
            candidates: Vec::new(),
            diagnostics: json!({"fake_query": query}),
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

#[test]
fn fake_port_begins_once_before_ordered_query_calls() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index = empty_index(temp.path())?;
    let calls = CallLog::default();
    let port = FakeSemanticPort::ready(calls.clone());
    let request = semantic_request(SearchBackend::Hybrid);

    let collection = collect_search_hits(
        &request,
        &index,
        &EventSearchFilters::default(),
        SemanticAvailability::Available,
        &port,
    )?;

    assert_eq!(collection.semantic_status, "ready");
    assert_eq!(collection.semantic_diagnostics.unwrap()["query_count"], 2);
    assert_eq!(
        calls.values(),
        vec![
            "begin_query",
            "candidates:first query:1600",
            "candidates:second query:1600",
        ]
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

    let error = collect_search_hits(
        &request,
        &index,
        &EventSearchFilters::default(),
        SemanticAvailability::Available,
        &port,
    )
    .unwrap_err();
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

    let collection = collect_search_hits(
        &request,
        &index,
        &EventSearchFilters::default(),
        SemanticAvailability::Available,
        &port,
    )?;

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

    let collection = collect_search_hits(
        &request,
        &index,
        &EventSearchFilters::default(),
        SemanticAvailability::Available,
        &port,
    )?;

    assert_eq!(collection.semantic_status, "skipped");
    assert!(calls.values().is_empty());
    Ok(())
}
