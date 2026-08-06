use super::*;

fn finish_for(index: &VerifiedIndex, revision: &str) -> FinishCoreMaterializationRequest {
    let sources = core_source_states(index.manifest()).unwrap();
    let head = core_generation_head(index, &sources).unwrap();
    let begin = BeginCoreMaterializationRequest {
        head: head.clone(),
        expected_prior_receipt: None,
    };
    FinishCoreMaterializationRequest {
        materialization_id: ctx_pro_host_protocol::core_materialization_id(&begin, revision)
            .unwrap(),
        head,
        expected_prior_receipt: None,
        source_delta_pages: 1,
        changed_sources: 1,
        removed_sources: 0,
        event_delta_pages: 1,
        event_mutations: 1,
    }
}

fn progress_for(
    index: &VerifiedIndex,
    phase: CoreMaterializationFinalizationPhase,
    cursor: char,
    revision: &str,
) -> CoreMaterializationFinalizationProgress {
    let finish = finish_for(index, revision);
    CoreMaterializationFinalizationProgress {
        materialization_id: finish.materialization_id.clone(),
        core_generation_id: index.generation_id().to_owned(),
        finish_request_digest: finish.canonical_digest().unwrap(),
        materializer_revision: revision.to_owned(),
        phase,
        cursor_sha256: cursor.to_string().repeat(64),
    }
}

#[test]
fn continuation_completion_rejects_changed_finish_digest_and_revision() {
    let (_temp, index) =
        single_source_index("finalization-terminal-cas.jsonl", vec!["body".to_owned()]);
    let expected = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::ReadyToActivate,
        '9',
        "test-core-materializer-v1",
    );

    for changed_revision in [false, true] {
        let mut consumer = Consumer::new();
        consumer.finalization_progress = Some(expected.clone());
        consumer.finish = Some(finish_for(&index, "test-core-materializer-v1"));
        if changed_revision {
            consumer.revision = "test-core-materializer-v2".to_owned();
        } else {
            consumer.terminal_finish_digest_override = Some("f".repeat(64));
        }
        let status = consumer
            .status(StatusRequest {
                requested_core_generation_id: Some(index.generation_id().to_owned()),
            })
            .unwrap();
        let error = continue_core_finalization(
            &index,
            &status,
            &mut consumer,
            CoreWorkerLaunchSelection::explicit_test(1),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("continuation CAS"),
            "unexpected terminal CAS error: {error:#}"
        );
    }
}

fn pending(
    progress: CoreMaterializationFinalizationProgress,
    replayed: bool,
) -> CoreMaterializationFinalizationPending {
    CoreMaterializationFinalizationPending { progress, replayed }
}

#[test]
fn finish_and_each_restart_continuation_advance_at_most_one_quantum() {
    let (_temp, index) = single_source_index("finalization.jsonl", vec!["body".to_owned()]);
    let mut consumer = Consumer::new();
    let first = pending(
        progress_for(
            &index,
            CoreMaterializationFinalizationPhase::SealingInputs,
            '1',
            &consumer.revision,
        ),
        false,
    );
    consumer.finish_pending = Some(first.clone());

    let CoreMaterializationSyncProgress::FinalizationPending(observed) =
        sync_core_feed_progress(&index, None, &mut consumer).unwrap()
    else {
        panic!("Finish should yield durable finalization progress");
    };
    assert_eq!(observed, first);
    assert_eq!(consumer.finish_requests.len(), 1);
    assert!(consumer.continue_requests.is_empty());
    let source_exchanges = consumer.source_exchanges;
    let state_exchanges = consumer.state_exchanges;
    let event_exchanges = consumer.event_exchanges;

    let second = pending(
        CoreMaterializationFinalizationProgress {
            phase: CoreMaterializationFinalizationPhase::EmitReplay,
            cursor_sha256: "2".repeat(64),
            ..first.progress.clone()
        },
        true,
    );
    consumer.continue_pending = Some(second.clone());
    let status = consumer
        .status(StatusRequest {
            requested_core_generation_id: Some(index.generation_id().to_owned()),
        })
        .unwrap();
    status.validate().unwrap();
    let CoreMaterializationSyncProgress::FinalizationPending(observed) =
        continue_core_finalization(
            &index,
            &status,
            &mut consumer,
            CoreWorkerLaunchSelection::explicit_test(1),
        )
        .unwrap()
    else {
        panic!("Continue should yield after one quantum");
    };
    assert_eq!(observed, second);
    assert_eq!(consumer.continue_requests.len(), 1);
    assert_eq!(
        consumer.continue_requests[0].expected_progress,
        first.progress
    );
    assert_eq!(consumer.source_exchanges, source_exchanges);
    assert_eq!(consumer.state_exchanges, state_exchanges);
    assert_eq!(consumer.event_exchanges, event_exchanges);

    let status = consumer
        .status(StatusRequest {
            requested_core_generation_id: Some(index.generation_id().to_owned()),
        })
        .unwrap();
    let CoreMaterializationSyncProgress::Finished(report) = continue_core_finalization(
        &index,
        &status,
        &mut consumer,
        CoreWorkerLaunchSelection::explicit_test(1),
    )
    .unwrap() else {
        panic!("final continuation should expose the terminal receipt");
    };
    assert_eq!(report.receipt.core_generation_id, index.generation_id());
    assert_eq!(consumer.continue_requests.len(), 2);
    assert_eq!(
        consumer.continue_requests[1].expected_progress,
        second.progress
    );
    assert_eq!(consumer.source_exchanges, source_exchanges);
    assert_eq!(consumer.state_exchanges, state_exchanges);
    assert_eq!(consumer.event_exchanges, event_exchanges);
}

#[test]
fn continuation_rejects_unchanged_and_conflicting_pending_progress() {
    let (_temp, index) =
        single_source_index("finalization-conflict.jsonl", vec!["body".to_owned()]);
    let expected = progress_for(
        &index,
        CoreMaterializationFinalizationPhase::EmitFlat,
        '3',
        "test-core-materializer-v1",
    );

    for response in [
        pending(expected.clone(), true),
        pending(
            CoreMaterializationFinalizationProgress {
                materialization_id: "f".repeat(64),
                phase: CoreMaterializationFinalizationPhase::EmitEventIndex,
                cursor_sha256: "4".repeat(64),
                ..expected.clone()
            },
            false,
        ),
    ] {
        let mut consumer = Consumer::new();
        consumer.finalization_progress = Some(expected.clone());
        consumer.continue_pending = Some(response);
        let status = consumer
            .status(StatusRequest {
                requested_core_generation_id: Some(index.generation_id().to_owned()),
            })
            .unwrap();
        let error = continue_core_finalization(
            &index,
            &status,
            &mut consumer,
            CoreWorkerLaunchSelection::explicit_test(1),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("did not advance"),
            "unexpected conflict error: {error:#}"
        );
        assert_eq!(consumer.continue_requests.len(), 1);
    }
}

#[test]
fn protocol_dispatch_maps_both_typed_finalization_responses() {
    let pending = pending(
        CoreMaterializationFinalizationProgress {
            materialization_id: "a".repeat(64),
            core_generation_id: "b".repeat(64),
            finish_request_digest: "d".repeat(64),
            materializer_revision: "materializer-v1".to_owned(),
            phase: CoreMaterializationFinalizationPhase::ReadyToActivate,
            cursor_sha256: "c".repeat(64),
        },
        true,
    );
    assert!(matches!(
        map_core_finalization_response(HelperMessage::CoreMaterializationFinalizationPending(
            pending
        ))
        .unwrap(),
        CoreMaterializationFinalizationStep::Pending(_)
    ));

    let finished = CoreMaterializationFinished {
        materialization_id: "a".repeat(64),
        finish_request_digest: "d".repeat(64),
        receipt: CoreMaterializationReceipt {
            core_generation_id: "b".repeat(64),
            core_record_contract_fingerprint: "c".repeat(64),
            source_snapshot_sha256: "d".repeat(64),
            materializer_revision: "materializer-v1".to_owned(),
            source_count: 0,
            event_count: 0,
        },
        replayed: false,
    };
    assert!(matches!(
        map_core_finalization_response(HelperMessage::CoreMaterializationFinished(finished))
            .unwrap(),
        CoreMaterializationFinalizationStep::Finished(_)
    ));
    assert!(
        map_core_finalization_response(HelperMessage::Status(status::result(
            StatusRequest {
                requested_core_generation_id: None,
            },
            CoreProjectionCurrentness::NotMaterialized,
            None,
            0,
            JournalFinishActivity::default(),
        )))
        .unwrap_err()
        .to_string()
        .contains("non-Core-finalization")
    );
}
