use std::cell::Cell;

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{CoreEventRecord, GenerationWriter, WriterOptions};
use uuid::Uuid;

use super::*;
use crate::semantic::{
    query_index::SemanticQueryPin,
    vector_store::{
        source_backed_semantic_vector_path, SemanticBatchEmbedder, SemanticChunkDocument,
        SemanticDocumentBuilder, SemanticVectorStore, SourceBackedGenerationPin,
    },
    SemanticEventDocument,
};

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
        .map_err(crate::semantic::committed_generation_recovery_error)?;
    writer.begin_source(source.clone())?;
    if include_record {
        let mut record = CoreRecord::new_selected(
            event_id,
            session_id,
            session_id,
            source.clone(),
            revision,
            "message",
            "primary",
            true,
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
            .ready
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

fn ready_adapter(
    index: &VerifiedIndex,
    event_id: Uuid,
    vector_root: &Path,
) -> Result<SemanticQueryAdapter> {
    let mut store = SemanticVectorStore::open(vector_root)?;
    store.publish_chunk_replacements(
        &[(
            SemanticChunkDocument {
                event_id,
                seq: 1,
                chunk_index: 0,
                source_text_hash: "1".repeat(64),
                text: String::new(),
                start_char: 0,
                end_char: 1,
            },
            embedding(),
        )],
        &[],
    )?;
    let pinned = store
        .flat_pin_generation()?
        .expect("ready adapter fixture must publish one flat generation");
    Ok(SemanticQueryAdapter::from_pin(
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
        let calls = Cell::new(0_u8);
        let mut adapter = SemanticQueryAdapter::default();

        let error = adapter
            .search_with(
                &index,
                temp.path(),
                "query",
                &EventSearchFilters::default(),
                1,
                |_, _| {
                    calls.set(calls.get() + 1);
                    Ok(Some((embedding(), 1)))
                },
            )
            .expect_err("unready semantic state must fail closed");

        let not_ready = error
            .downcast_ref::<SemanticNotReady>()
            .expect("preflight must retain the typed not-ready contract");
        assert_eq!(
            not_ready.code(),
            if unacknowledged_store {
                "semantic_generation_not_acknowledged"
            } else {
                "semantic_store_missing"
            }
        );
        assert_eq!(calls.get(), 0);
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
    let calls = Cell::new(0_u8);
    let error = SemanticQueryAdapter::default()
        .search_with(
            &index,
            temp.path(),
            "query",
            &EventSearchFilters::default(),
            1,
            |_, _| {
                calls.set(calls.get() + 1);
                Ok(Some((embedding(), 1)))
            },
        )
        .expect_err("an acknowledged stale generation must fail closed");

    assert_eq!(
        error
            .downcast_ref::<SemanticNotReady>()
            .map(SemanticNotReady::code),
        Some("semantic_generation_not_acknowledged")
    );
    assert_eq!(calls.get(), 0);
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
        let mut adapter = SemanticQueryAdapter::from_pin(pin);
        let calls = Cell::new(0_u8);
        let result = adapter.search_with(
            &index,
            temp.path(),
            "query",
            &EventSearchFilters::default(),
            1,
            |_, _| {
                calls.set(calls.get() + 1);
                Ok(Some((embedding(), 1)))
            },
        );

        if generation == index.generation_id() {
            let (candidates, diagnostics) = result?;
            assert!(candidates.is_empty());
            assert_eq!(
                diagnostics,
                compact_json(json!({
                    "vector_backend": "flat_f32",
                    "core_generation_id": index.generation_id(),
                    "flat_generation": null,
                    "flat_generation_hash": null,
                    "query_embed_ms": null,
                    "vector_scan_ms": null,
                    "chunks_scanned": null,
                    "vector_bytes_read": null,
                    "events_scored": null,
                    "initial_k": 1,
                    "final_k": 1,
                    "iterations": 0,
                    "raw_candidates": 0,
                    "eligible_candidates": 0,
                    "filtered_candidates": 0,
                    "non_positive_candidates": 0,
                    "exhausted": true,
                    "cap_reached": false,
                }))
            );
        } else {
            let error = result.expect_err("a mismatched pin must fail closed");
            assert_eq!(
                error
                    .downcast_ref::<SemanticNotReady>()
                    .map(SemanticNotReady::code),
                Some("semantic_generation_receipt_mismatch")
            );
        }
        assert_eq!(calls.get(), 0);
    }
    Ok(())
}

#[test]
fn adapter_embeds_ready_queries_once_and_reuses_one_pin_filter_cache() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, event_id) = semantic_index(temp.path())?;
    let mut adapter = ready_adapter(&index, event_id, &temp.path().join("vectors"))?;
    let calls = Cell::new(0_u8);
    let filters = EventSearchFilters::default();

    let (first, first_diagnostics) = adapter.search_with(
        &index,
        temp.path(),
        "first normalized query",
        &filters,
        1,
        |_, _| {
            calls.set(calls.get() + 1);
            Ok(Some((embedding(), 17)))
        },
    )?;
    assert_eq!(first.len(), 1);
    assert_eq!(first_diagnostics["query_embed_ms"], 17);
    let first_projection = adapter
        .pin
        .as_ref()
        .and_then(SemanticQueryPin::filter_projection_identity_for_test)
        .expect("first query must cache its filter projection");
    let (second, _) = adapter.search_with(
        &index,
        temp.path(),
        "second normalized query",
        &filters,
        1,
        |_, _| {
            calls.set(calls.get() + 1);
            Ok(Some((embedding(), 17)))
        },
    )?;
    assert_eq!(second.len(), 1);
    assert_eq!(calls.get(), 2, "each ready query must embed exactly once");
    assert_eq!(
        adapter
            .pin
            .as_ref()
            .and_then(SemanticQueryPin::filter_projection_identity_for_test),
        Some(first_projection),
        "normalized queries must reuse one pin and filter cache"
    );
    Ok(())
}

#[test]
fn adapter_preserves_daemon_query_service_unavailable_contract() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (index, event_id) = semantic_index(temp.path())?;
    let mut adapter = ready_adapter(&index, event_id, &temp.path().join("vectors"))?;
    let calls = Cell::new(0_u8);

    let error = adapter
        .search_with(
            &index,
            temp.path(),
            "query",
            &EventSearchFilters::default(),
            1,
            |_, _| {
                calls.set(calls.get() + 1);
                Ok(None)
            },
        )
        .expect_err("a ready pin still requires the daemon embedding service");
    let not_ready = error
        .downcast_ref::<SemanticNotReady>()
        .expect("daemon unavailability must retain the typed contract");

    assert_eq!(calls.get(), 1);
    assert_eq!(not_ready.code(), "semantic_query_service_unavailable");
    assert_eq!(
        not_ready.detail(),
        "the daemon query embedding service is unavailable"
    );
    assert!(not_ready.retryable());
    Ok(())
}
