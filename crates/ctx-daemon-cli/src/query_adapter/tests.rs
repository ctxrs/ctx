use std::cell::Cell;

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{CoreEventRecord, EventSearchFilters, GenerationWriter, WriterOptions};
use ctx_semantic_index::{
    source_backed_semantic_vector_path,
    test_support::{pinned_flat_generation, publish_chunk_replacements, semantic_chunk_document},
    SemanticBatchEmbedder, SemanticChunkDocument, SemanticDocumentBuilder, SemanticEventDocument,
    SemanticQueryPin, SemanticVectorStore, SourceBackedGenerationPin,
};
use uuid::Uuid;

use super::*;

fn default_compiled_filter() -> CompiledSearchFilter {
    CompiledSearchFilter::compile(EventSearchFilters::default()).unwrap()
}

fn semantic_index(root: &Path) -> Result<(VerifiedIndex, Uuid)> {
    semantic_index_revision(root, 1, true)
}

fn semantic_index_revision(
    root: &Path,
    revision: u64,
    include_record: bool,
) -> Result<(VerifiedIndex, Uuid)> {
    let source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8("query-adapter.jsonl")?)?,
    )?;
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("query-adapter-session")?)?;
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("message", TypedKey::U64(revision))?,
        subrecord_selector: None,
    })?;
    let index_root = root.join("index");
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    writer.begin_source(source.clone())?;
    if include_record {
        let mut record = CoreRecord::new_selected(
            event_id,
            session_id,
            source.clone(),
            revision,
            "message",
            "semantic-query-adapter-v1",
            format!("query adapter fixture {revision}"),
        )?;
        record.provider_session_id = Some("query-adapter-session".to_owned());
        record.native_event_id = Some(TypedKey::U64(revision));
        record.role = Some("user".to_owned());
        record.validate_contract()?;
        writer.add_core_record(record)?;
    }
    let record_count = u64::from(include_record);
    let observation =
        SourceObservation::new(source, "regular-file-v1", revision.to_le_bytes().to_vec())?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "query-adapter-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: record_count,
            retained_records: record_count,
            indexed_documents: record_count,
            certified_bytes: record_count,
            ..ScannedSourceCounts::default()
        },
    )?)?;
    writer.commit(|_| true)?;
    Ok((VerifiedIndex::open_pinned(&index_root)?, event_id.as_uuid()))
}

struct RejectingSemanticPorts;

impl SemanticDocumentBuilder for RejectingSemanticPorts {
    fn build_document(
        &mut self,
        _record: &CoreEventRecord,
    ) -> Result<Option<SemanticEventDocument>> {
        Err(anyhow!(
            "empty semantic fixture unexpectedly requested a document"
        ))
    }
}

impl SemanticBatchEmbedder for RejectingSemanticPorts {
    fn embed_chunks(&mut self, _chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        Err(anyhow!(
            "empty semantic fixture unexpectedly requested embeddings"
        ))
    }
}

fn acknowledge_empty_generation(
    store: &mut SemanticVectorStore,
    index: &VerifiedIndex,
) -> Result<()> {
    let mut builder = RejectingSemanticPorts;
    let mut embedder = RejectingSemanticPorts;
    for _ in 0..32 {
        if store
            .reconcile_source_backed_index(index, &mut builder, &mut embedder)?
            .ready()
        {
            return Ok(());
        }
    }
    Err(anyhow!("empty semantic fixture did not converge"))
}

fn embedding() -> Vec<f32> {
    let mut embedding = vec![0.0; SEMANTIC_DIMENSIONS];
    embedding[0] = 1.0;
    embedding
}

#[test]
fn request_adapter_borrows_the_exact_data_root() {
    let data_root = std::path::PathBuf::from("borrowed-query-root");
    let adapter = SemanticQueryAdapter::new(&data_root);

    assert!(std::ptr::eq(adapter.data_root, data_root.as_path()));
}

fn ready_adapter<'a>(
    index: &'a VerifiedIndex,
    data_root: &'a Path,
    event_id: Uuid,
    vector_root: &Path,
) -> Result<SemanticQuerySession<'a>> {
    let mut store = SemanticVectorStore::open(vector_root)?;
    publish_chunk_replacements(
        &mut store,
        &[(
            semantic_chunk_document(event_id, 1, 0, "1".repeat(64), String::new(), 0, 1),
            embedding(),
        )],
        &[],
    )?;
    let pinned = pinned_flat_generation(&store)?;
    Ok(SemanticQuerySession::from_pin(
        index,
        data_root,
        SemanticQueryPin::from_readiness_for_test(
            index.generation_id(),
            SourceBackedGenerationPin::Ready(pinned),
        )?,
    ))
}

#[test]
fn adapter_never_embeds_before_missing_or_unacknowledged_store_preflight() -> Result<()> {
    for unacknowledged_store in [false, true] {
        let temp = tempfile::tempdir()?;
        let (index, _) = semantic_index(temp.path())?;
        if unacknowledged_store {
            SemanticVectorStore::open(&source_backed_semantic_vector_path(temp.path()))?;
        }
        let error = SemanticQuerySession::begin(&index, temp.path())
            .err()
            .expect("unready semantic state must fail closed");
        assert!(matches!(
            error,
            SemanticQueryError::NotReady {
                code,
                retryable: true,
                ..
            } if code == if unacknowledged_store {
                "semantic_generation_not_acknowledged"
            } else {
                "semantic_store_missing"
            }
        ));
    }
    Ok(())
}

#[test]
fn adapter_never_embeds_before_acknowledged_stale_generation_preflight() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (stale_index, _) = semantic_index_revision(temp.path(), 1, false)?;
    let semantic_path = source_backed_semantic_vector_path(temp.path());
    let mut store = SemanticVectorStore::open(&semantic_path)?;
    acknowledge_empty_generation(&mut store, &stale_index)?;
    assert!(matches!(
        store.source_backed_generation_pin_exact(stale_index.generation_id(), 0)?,
        SourceBackedGenerationPin::ReadyEmpty
    ));
    drop(store);

    let (index, _) = semantic_index_revision(temp.path(), 2, true)?;
    let error = SemanticQuerySession::begin(&index, temp.path())
        .err()
        .expect("an acknowledged stale generation must fail closed");
    assert!(matches!(
        error,
        SemanticQueryError::NotReady {
            code: "semantic_generation_not_acknowledged",
            retryable: true,
            ..
        }
    ));
    Ok(())
}

#[test]
fn adapter_never_embeds_for_mismatched_or_ready_empty_pins() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, _) = semantic_index(temp.path())?;
    for generation in ["different-generation", index.generation_id()] {
        let pin = SemanticQueryPin::from_readiness_for_test(
            generation,
            SourceBackedGenerationPin::ReadyEmpty,
        )?;
        let mut adapter = SemanticQuerySession::from_pin(&index, temp.path(), pin);
        let calls = Cell::new(0_u8);
        let result = adapter.prepare_alternative_with("query", |_, _| {
            calls.set(calls.get() + 1);
            Ok(Some((embedding(), 1)))
        });

        if generation == index.generation_id() {
            assert_eq!(result?, compact_json(json!({"query_embed_ms": null})));
            let (candidates, diagnostics) = adapter.search(&default_compiled_filter(), 1)?;
            assert!(candidates.is_empty());
            assert_eq!(
                diagnostics,
                compact_json(json!({
                    "vector_backend": "flat_f32",
                    "core_generation_id": index.generation_id(),
                    "flat_generation": null,
                    "flat_generation_hash": null,
                    "vector_scan_ms": null,
                    "query_vectors": null,
                    "vector_passes": 0,
                    "chunks_scanned": null,
                    "vector_bytes_read": null,
                    "events_scored": null,
                    "dot_products": null,
                    "initial_k": 1,
                    "final_k": 1,
                    "iterations": 0,
                    "raw_candidates": 0,
                    "eligible_candidates": 0,
                    "filtered_candidates": 0,
                    "non_positive_candidates": 0,
                    "metadata_records_loaded": 0,
                    "core_records_decoded": 0,
                    "exhausted": true,
                    "cap_reached": false,
                }))
            );
        } else {
            let error = result.expect_err("a mismatched pin must fail closed");
            assert!(matches!(
                error,
                SemanticQueryError::NotReady {
                    code: "semantic_generation_receipt_mismatch",
                    retryable: true,
                    ..
                }
            ));
        }
        assert_eq!(calls.get(), 0);
    }
    Ok(())
}

#[test]
fn adapter_embeds_ordered_queries_then_runs_one_scan_with_one_filter_projection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, event_id) = semantic_index(temp.path())?;
    let mut adapter = ready_adapter(&index, temp.path(), event_id, &temp.path().join("vectors"))?;
    let calls = Cell::new(0_u8);
    let filters = default_compiled_filter();

    let first_diagnostics =
        adapter.prepare_alternative_with("first normalized query", |_, _| {
            calls.set(calls.get() + 1);
            Ok(Some((embedding(), 17)))
        })?;
    assert_eq!(first_diagnostics["query_embed_ms"], 17);
    let second_diagnostics =
        adapter.prepare_alternative_with("second normalized query", |_, _| {
            calls.set(calls.get() + 1);
            Ok(Some((embedding(), 17)))
        })?;
    assert_eq!(second_diagnostics["query_embed_ms"], 17);
    assert_eq!(adapter.pin.filter_projection_identity_for_test(), None);
    let (candidates, scan_diagnostics) = adapter.search(&filters, 1)?;
    assert_eq!(candidates.len(), 1);
    assert_eq!(scan_diagnostics["query_vectors"], 2);
    assert_eq!(scan_diagnostics["vector_passes"], 1);
    assert_eq!(calls.get(), 2, "each ready query must embed exactly once");
    assert!(adapter.pin.filter_projection_identity_for_test().is_some());
    Ok(())
}

#[test]
fn adapter_preserves_daemon_query_service_unavailable_contract() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, event_id) = semantic_index(temp.path())?;
    let mut adapter = ready_adapter(&index, temp.path(), event_id, &temp.path().join("vectors"))?;
    let calls = Cell::new(0_u8);

    let error = adapter
        .prepare_alternative_with("query", |_, _| {
            calls.set(calls.get() + 1);
            Ok(None)
        })
        .expect_err("a ready pin still requires the daemon embedding service");
    assert_eq!(calls.get(), 1);
    assert!(matches!(
        error,
        SemanticQueryError::NotReady {
            code: "semantic_query_service_unavailable",
            detail,
            retryable: true,
        } if detail == "the daemon query embedding service is unavailable"
    ));
    Ok(())
}

#[test]
fn adapter_scores_only_the_active_flat_core_intersection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, _) = semantic_index(temp.path())?;
    let mut adapter = ready_adapter(
        &index,
        temp.path(),
        Uuid::new_v4(),
        &temp.path().join("vectors"),
    )?;

    adapter.prepare_alternative_with("query", |_, _| Ok(Some((embedding(), 1))))?;
    let (candidates, diagnostics) = adapter.search(&default_compiled_filter(), 1)?;

    assert!(candidates.is_empty());
    assert_eq!(diagnostics["events_scored"], 0);
    assert_eq!(diagnostics["filtered_candidates"], 1);
    Ok(())
}

#[test]
fn adapter_downcasts_engine_not_ready_without_parsing_display_text() {
    let error = anyhow::Error::new(SemanticNotReady::new(
        "semantic_projection_event_mismatch",
        "typed engine detail",
    ));
    let classified = SemanticQueryError::from(error);

    assert!(matches!(
        classified,
        SemanticQueryError::NotReady {
            code: "semantic_projection_event_mismatch",
            detail,
            retryable: true,
        } if detail == "typed engine detail"
    ));
}

#[test]
fn adapter_maps_non_engine_failures_to_failed() {
    let classified = SemanticQueryError::from(anyhow!("transport failed"));
    assert!(matches!(
        classified,
        SemanticQueryError::Failed { detail } if detail == "transport failed"
    ));
}
