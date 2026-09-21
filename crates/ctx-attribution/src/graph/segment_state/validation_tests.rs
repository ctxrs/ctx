use super::*;

fn replacement_page() -> SegmentCorePageOutput {
    let record: crate::protocol::CoreRecord = crate::test_support::core_record();
    let owner = SegmentEventOwner {
        source_id: record.source.identity().to_string(),
        event_id: record.event_id.to_string(),
        direct_session_id: record.session_id.to_string(),
        root_session_id: record.root_session_id.map(|id| id.to_string()),
        event_sequence: 7,
    };
    SegmentCorePageOutput {
        materialization_id: "a".repeat(64),
        graph_generation: 2,
        request_sha256: "b".repeat(64),
        effect: crate::protocol::CoreEventDeltaPageApplied {
            materialization_id: "a".repeat(64),
            core_generation_id: "c".repeat(64),
            source: record.source,
            page_index: 0,
            additions: 0,
            replacements: 1,
            tombstones: 0,
            terminal: true,
            replayed: false,
        },
        mutations: vec![SegmentPublicationMutation::Replaced {
            tombstone: SegmentPublicationTombstone {
                owner: owner.clone(),
                prior_core_record_sha256: "d".repeat(64),
                prior_event_state_sha256: "e".repeat(64),
            },
            replacement: SegmentPreparedEvent {
                owner,
                event_identity: record.event_id,
                core_record_sha256: "d".repeat(64),
                core_record_leaf_sha256: "f".repeat(64),
                prepared: SegmentPreparedUnit {
                    origin_event_id: record.event_id.to_string(),
                    producer_authority_disposition: ProducerAuthorityDisposition::AbstainUnknown,
                    stable_entities: vec![record.event_id],
                    facts: Vec::new(),
                    evidence: None,
                    coverage: SegmentCoreCoverage::default(),
                },
            },
        }],
    }
}

#[test]
fn replacement_encoding_accepts_equal_hash_rebuild() {
    assert!(replacement_page().encode().is_ok());
}

#[test]
fn replacement_encoding_preserves_distinct_prior_and_current_sequence() {
    let mut page = replacement_page();
    let SegmentPublicationMutation::Replaced { replacement, .. } = &mut page.mutations[0] else {
        unreachable!()
    };
    replacement.owner.event_sequence = 3;
    replacement.core_record_sha256 = "1".repeat(64);
    let encoded: serde_json::Value = serde_json::from_slice(&page.encode().unwrap()).unwrap();
    assert_eq!(
        encoded["mutations"][0]["value"]["tombstone"]["owner"]["event_sequence"],
        7
    );
    assert_eq!(
        encoded["mutations"][0]["value"]["replacement"]["owner"]["event_sequence"],
        3
    );
}

#[test]
fn replacement_encoding_keeps_identity_hash_and_prepared_output_checks() {
    for corrupt in 0..5 {
        let mut page = replacement_page();
        let SegmentPublicationMutation::Replaced {
            tombstone,
            replacement,
        } = &mut page.mutations[0]
        else {
            unreachable!()
        };
        // Use unequal hashes so these controls also reach their intended
        // rejection before the equal-hash repair.
        replacement.core_record_sha256 = "1".repeat(64);
        match corrupt {
            0 => tombstone.owner.source_id = "another-source".to_owned(),
            1 => tombstone.prior_core_record_sha256 = "invalid".to_owned(),
            2 => replacement.core_record_leaf_sha256 = "invalid".to_owned(),
            3 => replacement.owner.event_id = "another-event".to_owned(),
            4 => replacement.prepared.origin_event_id = "another-event".to_owned(),
            _ => unreachable!(),
        }
        assert!(page.encode().is_err(), "invalid replacement case {corrupt}");
    }
}

#[test]
fn replacement_encoding_keeps_prepared_entity_and_page_item_bounds() {
    let mut page = replacement_page();
    let SegmentPublicationMutation::Replaced { replacement, .. } = &mut page.mutations[0] else {
        unreachable!()
    };
    replacement.prepared.stable_entities =
        vec![replacement.event_identity; MAX_SEGMENT_PREPARED_ENTITIES_PER_EVENT + 1];
    assert!(matches!(page.encode(), Err(CoreStoreError::Bounds)));

    let mut page = replacement_page();
    page.mutations =
        vec![page.mutations[0].clone(); crate::protocol::MAX_CORE_EVENT_DELTA_PAGE_ITEMS + 1];
    assert!(matches!(page.encode(), Err(CoreStoreError::Bounds)));
}

const COMPLETED_CONTROL_V2_FIXTURE: &str = r#"{
        "graph_generation":0,
        "event_count":0,
        "receipt":null,
        "materialization_id":null,
        "head":null,
        "expected_prior_receipt":null,
        "finish_request_sha256":null,
        "materializer_revision":"materializer-v1",
        "schema_contract":"segment-schema-v1",
        "semantics_contract":"segment-semantics-v1",
        "evidence_contract":"segment-evidence-v1",
        "core_record_contract":"core-record-v1",
        "coverage":{
            "repository_candidate_events":0,
            "logical_binding_events":0,
            "certified_live_root_access_events":0,
            "file_evidence_events":0,
            "exact_commit_evidence_events":0,
            "exact_pull_request_evidence_events":0
        },
        "replay_count":42,
        "replay_accumulator_sha256":"legacy-replay-state-is-not-current-authority",
        "publication_semantics_sha256":"0000000000000000000000000000000000000000000000000000000000000000"
    }"#;

#[test]
fn event_owner_accepts_absent_root_but_rejects_invalid_present_root() {
    let mut owner = SegmentEventOwner {
        source_id: "source:fixture".to_owned(),
        event_id: "event:fixture".to_owned(),
        direct_session_id: "session:fixture".to_owned(),
        root_session_id: None,
        event_sequence: 1,
    };
    assert!(validate_owner(&owner).is_ok());

    owner.root_session_id = Some(String::new());
    assert!(validate_owner(&owner).is_err());
}

#[test]
fn legacy_replay_control_is_behaviorally_inert_and_never_reserializes() {
    let control: SegmentCompletedControl =
        serde_json::from_str(COMPLETED_CONTROL_V2_FIXTURE).unwrap();
    control.validate().unwrap();
    let serialized = serde_json::to_value(&control).unwrap();
    assert!(serialized.get("replay_count").is_none());
    assert!(serialized.get("replay_accumulator_sha256").is_none());

    let mut different_legacy: serde_json::Value =
        serde_json::from_str(COMPLETED_CONTROL_V2_FIXTURE).unwrap();
    different_legacy["replay_count"] = serde_json::Value::String("ignored".to_owned());
    different_legacy["replay_accumulator_sha256"] = serde_json::Value::Bool(true);
    let different_legacy: SegmentCompletedControl =
        serde_json::from_value(different_legacy).unwrap();
    different_legacy.validate().unwrap();
    assert_eq!(control, different_legacy);
    assert_eq!(serialized, serde_json::to_value(&different_legacy).unwrap());

    let mut tampered: serde_json::Value =
        serde_json::from_str(COMPLETED_CONTROL_V2_FIXTURE).unwrap();
    tampered["finish_request_sha256"] = serde_json::Value::String("f".repeat(64));
    let tampered: SegmentCompletedControl = serde_json::from_value(tampered).unwrap();
    assert!(tampered.validate().is_err());
}

#[test]
fn direct_event_delta_page_semantics_are_deterministic() {
    use crate::protocol::{CoreEventDeltaPageApplied, SourceAnchor, SourceKey};

    let output = SegmentCorePageOutput {
        materialization_id: "a".repeat(64),
        graph_generation: 1,
        request_sha256: "b".repeat(64),
        effect: CoreEventDeltaPageApplied {
            materialization_id: "a".repeat(64),
            core_generation_id: "c".repeat(64),
            source: SourceKey::derive(
                "segment-state-test",
                "direct-page",
                "test",
                1,
                SourceAnchor::CatalogLineage([0x2a; 32]),
            )
            .unwrap(),
            page_index: 0,
            additions: 0,
            replacements: 0,
            tombstones: 0,
            terminal: true,
            replayed: false,
        },
        mutations: Vec::new(),
    };
    let first = output.encode().unwrap();
    assert_eq!(first, output.encode().unwrap());
    let encoded: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(encoded["effect"]["materialization_id"], "a".repeat(64));
    assert!(encoded["effect"].get("kind").is_none());
}
