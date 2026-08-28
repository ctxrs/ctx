use super::*;

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreActivity, CoreRecord,
    EventIdentityInput, LiteralFactKind, NativeItemKey, NativeSessionKey, ProviderDeclaredFact,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation,
    TypedKey, CORE_ACTIVITY_REVISION,
};
use ctx_history_index::EventSearchFilters;
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_semantic_model::semantic_model_contract;

#[test]
fn semantic_query_boundary_rejects_more_than_32_vectors() {
    assert!(validate_semantic_query_vector_count(32).is_ok());
    let error = validate_semantic_query_vector_count(33).unwrap_err();
    assert_eq!(
        error.to_string(),
        "source-backed semantic query vector count must be at most 32"
    );
}

#[test]
fn semantic_query_pin_rejects_a_different_core_generation() {
    let pin = SemanticQueryPin {
        core_generation_id: "generation-a".to_owned(),
        pinned: None,
        filter_projection: None,
    };

    let error = validate_semantic_query_generation("generation-b", &pin)
        .expect_err("a semantic pin must not cross Core generations");
    let not_ready = error
        .downcast_ref::<SemanticNotReady>()
        .expect("generation mismatch must retain the typed unavailable contract");
    assert_eq!(not_ready.code(), "semantic_generation_receipt_mismatch");
    assert!(not_ready.detail().contains("generation-a"));
    assert!(not_ready.detail().contains("generation-b"));
    assert_eq!(
        not_ready.structured(),
        json!({
            "error": not_ready.to_string(),
            "error_code": "semantic_generation_receipt_mismatch",
            "detail": not_ready.detail(),
            "retryable": true,
        })
    );
}

#[test]
fn semantic_disabled_contract_is_stable_and_not_retryable() {
    let not_ready = SemanticNotReady::new("semantic_disabled", "semantic search is disabled");

    assert_eq!(
        not_ready.structured(),
        json!({
            "error": not_ready.to_string(),
            "error_code": "semantic_disabled",
            "detail": "semantic search is disabled",
            "retryable": false,
        })
    );
}

#[test]
fn semantic_query_admission_fails_closed_but_permits_ready_empty() {
    let error =
        semantic_query_pin_from_readiness("generation-a", SourceBackedGenerationPin::NotReady)
            .err()
            .expect("an unacknowledged generation must fail closed");
    let not_ready = error
        .downcast_ref::<SemanticNotReady>()
        .expect("admission failure must retain the typed unavailable contract");
    assert_eq!(not_ready.code(), "semantic_generation_not_acknowledged");

    let pin = semantic_query_pin_from_readiness(
        "generation-empty",
        SourceBackedGenerationPin::ReadyEmpty,
    )
    .expect("an acknowledged empty generation must be admitted");
    assert_eq!(pin.core_generation_id, "generation-empty");
    assert!(pin.pinned.is_none());
}

#[test]
fn semantic_filter_is_applied_before_top_k_across_more_than_4096_candidates() -> Result<()> {
    const UNRELATED_EVENTS: u64 = 4_096;

    let temp = tempfile::tempdir()?;
    let contract = semantic_model_contract();
    let dimensions = contract.dimensions();
    let source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("semantic-filter-adversarial.jsonl")?,
        )?,
    )?;
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("adversarial-session")?)?;
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })?;
    let index_root = temp.path().join("index");
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    writer.begin_source(source.clone())?;
    let mut vector_items = Vec::with_capacity(UNRELATED_EVENTS as usize + 1);
    let mut target_event_id = None;
    for sequence in 1..=UNRELATED_EVENTS + 1 {
        let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence))?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })?;
        let is_target = sequence == UNRELATED_EVENTS + 1;
        let mut record = CoreRecord::new_selected(
            event_id,
            session_id,
            source.clone(),
            sequence,
            "message",
            "semantic-filter-adversarial-v1",
            if is_target { "target" } else { "unrelated" },
        )?;
        record.provider_session_id = Some("adversarial-session".to_owned());
        record.native_event_id = Some(TypedKey::U64(sequence));
        record.role = Some("user".to_owned());
        let workspace = if is_target {
            target_event_id = Some(event_id.as_uuid());
            "only-target".to_owned()
        } else {
            "unrelated".to_owned()
        };
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: None,
            invocation: None,
            result: None,
            facts: vec![ProviderDeclaredFact {
                kind: LiteralFactKind::Workspace,
                value: workspace,
            }],
        });
        record.validate_contract()?;
        writer.add_core_record(record)?;

        let embedding = if is_target {
            normalized_test_embedding(dimensions, 0.5, 0.75_f32.sqrt())
        } else {
            normalized_test_embedding(dimensions, 1.0, 0.0)
        };
        vector_items.push((
            super::super::vector_store::SemanticChunkDocument {
                event_id: event_id.as_uuid(),
                seq: sequence,
                chunk_index: 0,
                source_text_hash: format!("{sequence:064x}"),
                text: String::new(),
                start_char: 0,
                end_char: 1,
            },
            embedding,
        ));
    }
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1_u8])?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "adversarial-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: UNRELATED_EVENTS + 1,
            retained_records: UNRELATED_EVENTS + 1,
            indexed_documents: UNRELATED_EVENTS + 1,
            certified_bytes: UNRELATED_EVENTS + 1,
            ..ScannedSourceCounts::default()
        },
    )?)?;
    writer.commit(|_| true)?;
    let index = VerifiedIndex::open_pinned(&index_root)?;

    let mut store = SemanticVectorStore::open(&temp.path().join("vectors"), contract)?;
    store.publish_chunk_replacements(&vector_items, &[])?;
    let pinned = store
        .flat_pin_generation()?
        .expect("adversarial vectors must publish a flat generation");
    let filters = EventSearchFilters {
        workspace: Some("ONLY-target".to_owned()),
        ..EventSearchFilters::default()
    };
    let filter = CompiledSearchFilter::compile(filters)?;
    let projection = index.semantic_filter_projection(&filter)?;
    assert_eq!(projection.len(), 1);

    let mut pin = semantic_query_pin_from_readiness(
        index.generation_id(),
        SourceBackedGenerationPin::Ready(pinned),
    )?;
    let (candidates, diagnostics) = pin.search(
        &index,
        &filter,
        &[
            normalized_test_embedding(dimensions, 0.0, 1.0),
            normalized_test_embedding(dimensions, 1.0, 0.0),
        ],
        1,
    )?;
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].event.event_id,
        target_event_id.expect("target event ID")
    );
    assert_eq!(diagnostics["iterations"], 1);
    assert_eq!(diagnostics["events_scored"], 1);
    assert_eq!(diagnostics["chunks_scanned"], 1);
    assert_eq!(diagnostics["query_vectors"], 2);
    assert_eq!(diagnostics["vector_passes"], 1);
    assert_eq!(diagnostics["dot_products"], 2);
    assert_eq!(diagnostics["metadata_records_loaded"], 1);
    assert_eq!(diagnostics["core_records_decoded"], 0);
    assert_eq!(
        diagnostics["filtered_candidates"],
        UNRELATED_EVENTS as usize
    );
    Ok(())
}

#[test]
fn semantic_query_scores_only_active_flat_events_that_match_core_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let contract = semantic_model_contract();
    let dimensions = contract.dimensions();
    let source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("semantic-active-intersection.jsonl")?,
        )?,
    )?;
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("active-intersection")?)?;
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })?;
    let index_root = temp.path().join("index");
    let mut writer = GenerationWriter::open(&index_root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    writer.begin_source(source.clone())?;
    let mut active_event_id = None;
    let mut vector_items = Vec::new();
    for (sequence, workspace, has_vector) in [
        (1_u64, "shared", true),
        (2, "shared", false),
        (3, "filtered-only", false),
    ] {
        let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence))?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })?;
        let mut record = CoreRecord::new_selected(
            event_id,
            session_id,
            source.clone(),
            sequence,
            "message",
            "semantic-active-intersection-v1",
            format!("event {sequence}"),
        )?;
        record.provider_session_id = Some("active-intersection".to_owned());
        record.native_event_id = Some(TypedKey::U64(sequence));
        record.role = Some("user".to_owned());
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: None,
            invocation: None,
            result: None,
            facts: vec![ProviderDeclaredFact {
                kind: LiteralFactKind::Workspace,
                value: workspace.to_owned(),
            }],
        });
        record.validate_contract()?;
        writer.add_core_record(record)?;
        if has_vector {
            active_event_id = Some(event_id.as_uuid());
            vector_items.push((
                super::super::vector_store::SemanticChunkDocument {
                    event_id: event_id.as_uuid(),
                    seq: sequence,
                    chunk_index: 0,
                    source_text_hash: format!("{sequence:064x}"),
                    text: String::new(),
                    start_char: 0,
                    end_char: 1,
                },
                normalized_test_embedding(dimensions, 1.0, 0.0),
            ));
        }
    }
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1_u8])?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "active-intersection-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: 3,
            retained_records: 3,
            indexed_documents: 3,
            certified_bytes: 3,
            ..ScannedSourceCounts::default()
        },
    )?)?;
    writer.commit(|_| true)?;
    let index = VerifiedIndex::open_pinned(&index_root)?;

    let mut store = SemanticVectorStore::open(&temp.path().join("vectors"), contract)?;
    store.publish_chunk_replacements(&vector_items, &[])?;
    let pinned = store
        .flat_pin_generation()?
        .expect("the active intersection vector must publish");
    let mut pin = semantic_query_pin_from_readiness(
        index.generation_id(),
        SourceBackedGenerationPin::Ready(pinned),
    )?;

    let shared = CompiledSearchFilter::compile(EventSearchFilters {
        workspace: Some("shared".to_owned()),
        ..EventSearchFilters::default()
    })?;
    let (candidates, diagnostics) = pin.search(
        &index,
        &shared,
        &[normalized_test_embedding(dimensions, 1.0, 0.0)],
        3,
    )?;
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].event.event_id,
        active_event_id.expect("active event ID")
    );
    assert_eq!(diagnostics["events_scored"], 1);

    let filtered_only = CompiledSearchFilter::compile(EventSearchFilters {
        workspace: Some("filtered-only".to_owned()),
        ..EventSearchFilters::default()
    })?;
    let (candidates, diagnostics) = pin.search(
        &index,
        &filtered_only,
        &[normalized_test_embedding(dimensions, 1.0, 0.0)],
        3,
    )?;
    assert!(candidates.is_empty());
    assert_eq!(diagnostics["events_scored"], 0);
    Ok(())
}

fn normalized_test_embedding(dimensions: usize, first: f32, second: f32) -> Vec<f32> {
    let mut embedding = vec![0.0; dimensions];
    let norm = first.mul_add(first, second * second).sqrt();
    embedding[0] = first / norm;
    embedding[1] = second / norm;
    embedding
}
