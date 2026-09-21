use super::*;
use ctx_history_core::{
    EventIdentityInput, NativeItemKey, NativeSessionKey, SessionIdentityInput, SourceAnchor,
    TypedKey, derive_event_id, derive_session_id,
};
use serde_json::json;

fn source(anchor: u8) -> SourceKey {
    SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage([anchor; 32]),
    )
    .unwrap()
}

fn citation() -> EvidenceCitation {
    let source = source(3);
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &NativeSessionKey::native_id("session", TypedKey::U64(1)).unwrap(),
    })
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(2)).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    EvidenceCitation {
        core_generation_id: "a".repeat(64),
        source,
        session_id,
        event_id,
        event_sequence: 7,
        byte_range: Some(ByteRange {
            start: 10,
            end_exclusive: 20,
        }),
        evidence_sha256: Some("b".repeat(64)),
    }
}

#[test]
fn citations_keep_core_identity_and_reject_wrong_sources_ranges_and_digests() {
    let valid = citation();
    assert!(valid.is_usable());
    let encoded = serde_json::to_value(&valid).unwrap();
    assert_eq!(encoded["event_sequence"], 7);
    assert_eq!(
        encoded["byte_range"],
        json!({"start": 10, "end_exclusive": 20})
    );
    assert_eq!(
        serde_json::from_value::<EvidenceCitation>(encoded).unwrap(),
        valid
    );

    let mut invalid = valid.clone();
    invalid.source = source(4);
    assert!(!invalid.is_usable());
    invalid = valid.clone();
    invalid.session_id = valid.event_id;
    assert!(!invalid.is_usable());
    invalid = valid.clone();
    invalid.byte_range = Some(ByteRange {
        start: 21,
        end_exclusive: 20,
    });
    assert!(!invalid.is_usable());
    invalid = valid.clone();
    invalid.core_generation_id = "A".repeat(64);
    assert!(!invalid.is_usable());
    invalid = valid;
    invalid.evidence_sha256 = Some("b".repeat(63));
    assert!(!invalid.is_usable());
}

fn receipt() -> CoreMaterializationReceipt {
    CoreMaterializationReceipt {
        core_generation_id: "a".repeat(64),
        core_record_contract_fingerprint: "b".repeat(64),
        source_snapshot_sha256: "c".repeat(64),
        materializer_revision: "materializer-v1".to_owned(),
        source_count: 1,
        event_count: 3,
    }
}

#[test]
fn generation_receipts_bind_the_exact_source_frontier_and_counts() {
    let sources = [CoreSourceState {
        source: source(3),
        core_record_accumulator: "d".repeat(64),
        event_count: 3,
    }];
    let head = CoreGenerationHead::new(
        "a".repeat(64),
        1,
        1,
        "b".repeat(64),
        1,
        1,
        "e".repeat(64),
        &sources,
    )
    .unwrap();
    head.validate_sources(&sources).unwrap();
    let mut completed = receipt();
    completed.source_snapshot_sha256 = head.source_snapshot_sha256.clone();
    completed.validate_for_head(&head).unwrap();
    let identity = CoreMaterializationReceiptIdentity::from_receipt(&completed).unwrap();
    assert_eq!(
        serde_json::to_value(identity).unwrap(),
        json!({
            "core_generation_id": "a".repeat(64),
            "materializer_revision": "materializer-v1",
        })
    );

    completed.event_count += 1;
    assert_eq!(
        completed.validate_for_head(&head).unwrap_err().class,
        ErrorClass::Sequence
    );
    let mut changed_sources = sources.clone();
    changed_sources[0].core_record_accumulator = "f".repeat(64);
    assert!(head.validate_sources(&changed_sources).is_err());
    assert!(core_source_snapshot_sha256(&[sources[0].clone(), sources[0].clone()]).is_err());

    completed = receipt();
    completed.materializer_revision = "a".repeat(MAX_CORE_MATERIALIZER_REVISION_BYTES);
    completed.validate().unwrap();
    completed.materializer_revision.push('a');
    assert_eq!(completed.validate().unwrap_err().class, ErrorClass::Bounds);
}

#[test]
fn empty_source_snapshot_keeps_its_canonical_digest() {
    assert_eq!(
        core_source_snapshot_sha256(&[]).unwrap(),
        "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
    );
}

#[test]
fn repository_coverage_cannot_claim_unbound_or_unprocessed_events() {
    RepositoryCoverage::default()
        .validate_for_receipt(None)
        .unwrap();
    let mut coverage = RepositoryCoverage {
        repository_candidate_events: 3,
        logical_binding_events: 2,
        certified_live_root_access_events: 1,
        file_evidence_events: 2,
        exact_commit_evidence_events: 1,
        exact_pull_request_evidence_events: 0,
    };
    let completed = receipt();
    coverage.validate_for_receipt(Some(&completed)).unwrap();
    assert!(coverage.validate_for_receipt(None).is_err());
    coverage.file_evidence_events = 3;
    assert!(coverage.validate_for_receipt(Some(&completed)).is_err());
    coverage.file_evidence_events = 2;
    coverage.repository_candidate_events = 4;
    assert!(coverage.validate_for_receipt(Some(&completed)).is_err());
}

#[test]
fn preview_data_preserves_invocation_intent_and_optional_field_omission() {
    let operation: evidence_preview::RepositoryFileInvocationKind =
        RepositoryFileInvocationKind::Modify;
    let preview = EvidencePreviewModel {
        previews: vec![EvidencePreview {
            citation_numbers: vec![1, 2],
            operation,
            path: "src/lib.rs".to_owned(),
            prior_path: None,
            tool_name: "apply_patch".to_owned(),
            event_occurred_at_ms: None,
            excerpt: "+synthetic change".to_owned(),
        }],
    };
    assert_eq!(
        serde_json::to_value(preview).unwrap(),
        json!({
            "previews": [{
                "citation_numbers": [1, 2], "operation": "modify", "path": "src/lib.rs",
                "tool_name": "apply_patch", "excerpt": "+synthetic change"
            }]
        })
    );
    assert_eq!(
        serde_json::to_value(EvidencePreviewModel { previews: vec![] }).unwrap(),
        json!({"previews": []})
    );
}

#[test]
fn diagnostics_are_pure_data_with_a_concrete_import_action() {
    let reason: diagnostic::BlameDiagnosticReason = BlameDiagnosticReason::ProjectionStale;
    let diagnostic = BlameDiagnostic {
        error: "projection_stale",
        error_code: "projection_stale",
        reason,
        message: "Attribution is behind the retained history.",
        retryable: true,
        freshness: Some(BlameDiagnosticFreshness {
            state: BlameFreshnessState::StaleCommitted,
        }),
        next_action: Some(BlameNextAction {
            kind: BlameNextActionKind::ImportAll,
            argv: vec!["ctx".to_owned(), "import".to_owned(), "--all".to_owned()],
        }),
        candidates: vec![],
        candidates_truncated: false,
    };
    assert_eq!(diagnostic.to_string(), "projection_stale");
    assert!(std::error::Error::source(&diagnostic).is_none());
    assert_eq!(
        serde_json::to_value(&diagnostic).unwrap(),
        json!({
            "error": "projection_stale", "error_code": "projection_stale",
            "reason": "projection_stale", "message": "Attribution is behind the retained history.",
            "retryable": true, "freshness": {"state": "stale_committed"},
            "next_action": {"kind": "import_all", "argv": ["ctx", "import", "--all"]}
        })
    );
    assert_eq!(
        serde_json::to_value(BlameResultFreshness::Current).unwrap(),
        "current"
    );
    for reason in [
        "entitlement_required",
        "secure_storage_locked",
        "helper_failed",
    ] {
        assert!(serde_json::from_value::<BlameDiagnosticReason>(json!(reason)).is_err());
    }
    for class in [
        "entitlement_expired",
        "key_store_locked",
        "key_store_unavailable",
    ] {
        assert!(serde_json::from_value::<ErrorClass>(json!(class)).is_err());
    }
}

#[test]
fn shared_presentation_reasons_do_not_weaken_typed_error_details() {
    let error = ProtocolError::new(ErrorClass::ResourceNotFound, "no indexed target")
        .with_blame_details(BlameDiagnosticDetails {
            reason: BlameDiagnosticReason::TargetNotIndexed,
            candidates: vec![],
            candidates_truncated: false,
        });
    let encoded = serde_json::to_value(&error).unwrap();
    assert_eq!(
        serde_json::from_value::<ProtocolError>(encoded.clone()).unwrap(),
        error
    );
    let mut invalid = encoded;
    invalid["details"]["reason"] = json!("projection_stale");
    assert!(serde_json::from_value::<ProtocolError>(invalid).is_err());
}
