use super::*;
use ctx_history_capture_runtime::CorePreparationPort;
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecordError, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};

#[test]
fn index_preparation_classifies_only_source_contract_failures_as_invalid() {
    let invalid_source = [
        IndexError::ProjectionContract(ProjectionContractError::SourceChanged),
        IndexError::CoreRecord(CoreRecordError::UnsupportedVersion(0)),
        IndexError::CoreRecordPolicyRevisionMismatch {
            normalization: 0,
            expected_normalization: 1,
            content: 0,
            expected_content: 1,
        },
        IndexError::EmptyDocumentField { field: "body" },
        IndexError::DocumentFieldTooLarge {
            field: "body",
            actual: 2,
            maximum: 1,
        },
    ];
    for failure in invalid_source {
        assert_eq!(
            index_preparation_failure_kind(&failure),
            CorePreparationFailureKind::InvalidSource
        );
    }
    assert_eq!(
        index_preparation_failure_kind(&IndexError::ConcurrentGenerationChange),
        CorePreparationFailureKind::Internal
    );
    assert_eq!(
        index_preparation_failure_kind(&IndexError::ActiveGenerationNeedsRebuild {
            generation_id: "adapter-test-generation".to_owned(),
            detail: "fixture rebuild required".to_owned(),
        }),
        CorePreparationFailureKind::Internal
    );
}

#[test]
fn index_preparation_delegates_exact_size_and_capacity_without_reencoding() {
    let temporary = crate::test_support_paths::tempdir().unwrap();
    let writer = GenerationWriter::open(
        temporary.path(),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    let preparation = IndexCorePreparation::from(writer.core_record_preparer());

    let direct = preparation.0.prepare(adapter_test_record()).unwrap();
    let prepared = preparation.prepare(adapter_test_record()).unwrap();
    assert_eq!(
        preparation.encoded_bytes(&prepared),
        direct.encoded_core_bytes(),
        "the runtime envelope must account for the preparer's exact final bytes"
    );

    let exact_bytes = preparation.encoded_bytes(&prepared);
    let draft = preparation.prepare_draft(adapter_test_record()).unwrap();
    assert!(matches!(
        preparation.materialize_draft(draft, exact_bytes.saturating_sub(1)),
        Ok(CoreMaterialization::CapacityExceeded(_))
    ));

    let draft = preparation.prepare_draft(adapter_test_record()).unwrap();
    let CoreMaterialization::Prepared(materialized) =
        preparation.materialize_draft(draft, exact_bytes).unwrap()
    else {
        panic!("the exact prepared size must admit materialization");
    };
    assert_eq!(preparation.encoded_bytes(&materialized), exact_bytes);
}

#[test]
fn index_capture_lifecycle_runs_complete_neutral_exchange() {
    let temporary = crate::test_support_paths::tempdir().unwrap();
    let mut lifecycle = match IndexCaptureLifecycle::open(
        temporary.path(),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap()
    {
        CaptureLifecycleOpenOutcome::Ready(lifecycle) => lifecycle,
        CaptureLifecycleOpenOutcome::RecoveryRequired { recovery } => panic!(
            "fresh lifecycle unexpectedly requires recovery for {}: {}",
            recovery.generation_id(),
            recovery.detail()
        ),
    };
    assert!(lifecycle.base_snapshot().is_none());

    let route_identity =
        ctx_history_capture_model::SourceRouteIdentity::from_sha256("42".repeat(32)).unwrap();
    lifecycle
        .set_route_plan(BTreeSet::from([route_identity.clone()]), BTreeSet::new())
        .unwrap();
    lifecycle.begin_route_stage(route_identity.clone()).unwrap();

    let (record, source) = adapter_test_record_and_source();
    let source_identity = source.identity();
    lifecycle.begin_source_replace(source.clone()).unwrap();
    let preparation = lifecycle.core_preparation();
    let prepared = preparation.prepare(record).unwrap();
    assert!(preparation
        .prepared_source(&prepared)
        .exact_descriptor_eq(&source));
    lifecycle.add_prepared(prepared).unwrap();

    let certificate = adapter_test_certificate(&source);
    lifecycle.certify_source(certificate.clone()).unwrap();

    let mut visited_targets = 0;
    lifecycle
        .visit_revalidation_targets(|target| {
            let CaptureRevalidationTarget::Source(target) = target else {
                panic!("replace stage unexpectedly exposed a deletion target");
            };
            assert_eq!(target, &certificate);
            assert_eq!(target.counts().indexed_documents, 1);
            assert_eq!(target.observation().source().identity(), source_identity);
            visited_targets += 1;
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap()
        .unwrap();
    assert_eq!(visited_targets, 1);

    lifecycle.finish_route_stage(&route_identity).unwrap();
    lifecycle
        .set_present_routes(std::iter::once(PresentCaptureRoute::new(
            route_identity.clone(),
            vec![source.clone()],
        )))
        .unwrap();

    let receipt = lifecycle
        .commit(
            |target| match target {
                CaptureRevalidationTarget::Source(target) => target == &certificate,
                CaptureRevalidationTarget::Deletion(_) => false,
            },
            |_| true,
        )
        .unwrap();
    assert!(!receipt.generation_id.is_empty());
    assert_eq!(receipt.indexed_documents, 1);
    assert_eq!(receipt.certified_sources, 1);
    assert_eq!(receipt.certified_source_bytes, 1);

    let snapshot = receipt.snapshot();
    assert_eq!(snapshot.sources(), std::slice::from_ref(&certificate));
    assert_eq!(
        snapshot.sources()[0].observation().source().identity(),
        source_identity
    );
    let route = snapshot.source_route(&route_identity).unwrap();
    assert_eq!(route.route_identity(), &route_identity);
    assert!(!route.is_missing());
    assert_eq!(route.sources().len(), 1);
    assert!(route.sources()[0].exact_descriptor_eq(&source));
    let mut aggregates = snapshot.source_aggregates();
    assert_eq!(aggregates.len(), 1);
    let aggregate = aggregates.next().unwrap();
    assert_eq!(aggregate.indexed_documents(), 1);
    assert!(aggregates.next().is_none());
    let aggregate_identity = aggregate.source_identity_digest();
    assert_eq!(aggregate_identity.len(), 64);
    for (encoded, expected) in aggregate_identity
        .as_bytes()
        .chunks_exact(2)
        .zip(source_identity.digest())
    {
        assert_eq!(
            u8::from_str_radix(std::str::from_utf8(encoded).unwrap(), 16).unwrap(),
            expected
        );
    }
}

fn adapter_test_record() -> CoreRecord {
    adapter_test_record_and_source().0
}

fn adapter_test_record_and_source() -> (CoreRecord, SourceKey) {
    let source = SourceKey::derive(
        "runtime-adapter-test",
        "runtime_adapter_fixture",
        "runtime-adapter-fixture-v1",
        1,
        SourceAnchor::CatalogLineage([7; 32]),
    )
    .unwrap();
    let native_session_key =
        NativeSessionKey::native_id("runtime-adapter.session", TypedKey::U64(1)).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "runtime-adapter-session",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key =
        NativeItemKey::native_id("runtime-adapter.event", TypedKey::U64(1)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "runtime-adapter-event",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        1,
        "message",
        "runtime-adapter-parser-v1".to_owned(),
        "runtime adapter Core record".to_owned(),
    )
    .map(|mut record| {
        record.agent_scope = Some(ctx_history_core::AgentScope::Primary);
        record
    })
    .unwrap();
    (record, source)
}

fn adapter_test_certificate(source: &SourceKey) -> CertifiedSource {
    let observation =
        SourceObservation::new(source.clone(), "runtime-adapter-observation-v1", vec![1]).unwrap();
    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "runtime-adapter-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            rejected_records: 0,
            ignored_records: 0,
            indexed_documents: 1,
            certified_bytes: 1,
        },
        None,
    )
    .unwrap()
}
