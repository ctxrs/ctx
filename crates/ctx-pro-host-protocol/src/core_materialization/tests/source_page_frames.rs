use super::*;

#[test]
fn source_acknowledgement_sizing_is_the_exact_complete_frame_at_decimal_boundaries() {
    let response = CoreSourceDeltaPageApplied {
        materialization_id: "d".repeat(64),
        core_generation_id: "a".repeat(64),
        page_index: 0,
        acknowledgement_page_index: 9,
        acknowledgement_terminal: false,
        changed_sources: 0,
        removed_sources: 1,
        reconcile_sources: vec![CoreSourceReconciliation {
            materialize_index: 9,
            delta: CoreSourceDelta::Removed(CoreSourceRemoval { source: source(9) }),
        }],
        replayed: false,
    };
    for sequence in [0, 9, 10, 99, 100, u64::MAX] {
        let mut frame = Vec::new();
        write_frame(
            &mut frame,
            &HelperEnvelope {
                sequence,
                request_id: Uuid::from_u128(1),
                message: HelperMessage::CoreSourceDeltaPageApplied(response.clone()),
            },
        )
        .unwrap();
        assert_eq!(
            core_source_delta_page_applied_frame_wire_bytes(sequence, &response).unwrap(),
            frame.len()
        );
    }
    assert_eq!(
        core_source_delta_page_applied_frame_wire_bytes(10, &response).unwrap(),
        core_source_delta_page_applied_frame_wire_bytes(9, &response).unwrap() + 1
    );
    assert_eq!(
        core_source_delta_page_applied_frame_wire_bytes(100, &response).unwrap(),
        core_source_delta_page_applied_frame_wire_bytes(99, &response).unwrap() + 1
    );

    let mut cursor_ten = response.clone();
    cursor_ten.acknowledgement_page_index = 10;
    assert_eq!(
        core_source_delta_page_applied_frame_wire_bytes(u64::MAX, &cursor_ten).unwrap(),
        core_source_delta_page_applied_frame_wire_bytes(u64::MAX, &response).unwrap() + 1
    );
    let mut cursor_ninety_nine = response;
    cursor_ninety_nine.acknowledgement_page_index = 99;
    let ninety_nine =
        core_source_delta_page_applied_frame_wire_bytes(u64::MAX, &cursor_ninety_nine).unwrap();
    cursor_ninety_nine.acknowledgement_page_index = 100;
    assert_eq!(
        core_source_delta_page_applied_frame_wire_bytes(u64::MAX, &cursor_ninety_nine).unwrap(),
        ninety_nine + 1
    );
}

#[test]
fn source_delta_request_sizing_is_the_exact_complete_host_frame() {
    let request = ApplyCoreSourceDeltaPageRequest {
        page: CoreSourceDeltaPage::new(
            "d".repeat(64),
            "a".repeat(64),
            0,
            true,
            vec![CoreSourceDelta::Present(state(escaped_source(), 1, 0))],
        )
        .unwrap(),
        acknowledgement_page_index: 99,
    };
    for sequence in [0, 9, 10, 99, 100, u64::MAX] {
        let mut frame = Vec::new();
        write_frame(
            &mut frame,
            &HostEnvelope {
                sequence,
                request_id: Uuid::from_u128(1),
                message: HostMessage::ApplyCoreSourceDeltaPage(request.clone()),
            },
        )
        .unwrap();
        assert_eq!(
            apply_core_source_delta_page_request_frame_wire_bytes(sequence, &request).unwrap(),
            frame.len()
        );
    }
}

#[test]
fn source_delta_request_frame_bound_rejects_before_consumer_mutation() {
    let request = ApplyCoreSourceDeltaPageRequest {
        page: CoreSourceDeltaPage::new(
            "d".repeat(64),
            "a".repeat(64),
            0,
            true,
            vec![CoreSourceDelta::Present(state(escaped_source(), 1, 0))],
        )
        .unwrap(),
        acknowledgement_page_index: 0,
    };
    request.page.validate().unwrap();
    let request_bytes = serde_json::to_vec(&request).unwrap().len();
    let frame_bytes =
        apply_core_source_delta_page_request_frame_wire_bytes(u64::MAX, &request).unwrap();
    assert!(request_bytes < frame_bytes);
    request
        .validate_with_control_frame_wire_bound(frame_bytes)
        .unwrap();
    let mut consumer_mutated = false;
    assert_eq!(
        request
            .validate_with_control_frame_wire_bound(frame_bytes - 1)
            .map(|()| consumer_mutated = true)
            .unwrap_err()
            .class,
        ErrorClass::Bounds
    );
    assert!(!consumer_mutated);
    assert_eq!(
        crate::message::apply_core_source_delta_page_request_frame_wire_bytes_from_request_bytes(
            u64::MAX,
            usize::MAX,
        )
        .unwrap_err()
        .class,
        ErrorClass::Bounds
    );
}

#[test]
fn generation_begin_and_finish_fail_closed_on_mismatched_cas() {
    let sources = vec![state(source(1), 1, 0)];
    let request = BeginCoreMaterializationRequest {
        head: head(&sources),
        expected_prior_receipt: None,
    };
    let revision = "materializer-v2";
    let identity = request.acknowledgement_identity().unwrap();
    let expected_materialization_id = canonical_sha256(
        &(&request, revision),
        "Core materialization ID encoding failed",
    )
    .unwrap();
    let mut began = CoreMaterializationBegan {
        materialization_id: core_materialization_id(&request, revision).unwrap(),
        core_generation_id: request.head.core_generation_id.clone(),
        materializer_revision: revision.to_owned(),
        expected_prior_receipt: None,
        replayed: false,
    };
    assert_eq!(began.materialization_id, expected_materialization_id);
    drop(request);
    began.validate_for_identity(&identity).unwrap();
    began.core_generation_id = "f".repeat(64);
    assert!(began.validate_for_identity(&identity).is_err());
}

#[test]
fn receipt_identity_binds_generation_sources_and_materializer_revision() {
    let sources = vec![state(source(1), 1, 4), state(source(2), 2, 5)];
    let head = head(&sources);
    let receipt = CoreMaterializationReceipt {
        core_generation_id: head.core_generation_id.clone(),
        core_record_contract_fingerprint: head.core_record_contract_fingerprint.clone(),
        source_snapshot_sha256: head.source_snapshot_sha256.clone(),
        materializer_revision: "materializer-v1".to_owned(),
        source_count: 2,
        event_count: 9,
    };
    receipt.validate_for_head(&head).unwrap();
    let first = CoreMaterializationReceiptIdentity::from_receipt(&receipt).unwrap();
    let mut revised = receipt.clone();
    revised.materializer_revision = "materializer-v2".to_owned();
    let second = CoreMaterializationReceiptIdentity::from_receipt(&revised).unwrap();
    assert_ne!(first.materializer_revision, second.materializer_revision);
}
