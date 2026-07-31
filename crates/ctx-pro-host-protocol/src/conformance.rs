use std::{collections::BTreeSet, io::Cursor};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::*;

fn inventory() -> Value {
    serde_json::from_str(include_str!("../testdata/v1/inventory.json")).expect("protocol inventory")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(value: &str) -> Vec<u8> {
    fn nibble(byte: u8) -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => panic!("invalid lowercase hex"),
        }
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| (nibble(pair[0]) << 4) | nibble(pair[1]))
        .collect()
}

fn host_kind(message: &HostMessage) -> &'static str {
    match message {
        HostMessage::Hello(_) => "hello",
        HostMessage::Authorize(_) => "authorize",
        HostMessage::PrepareGraphKeyDeletion(_) => "prepare_graph_key_deletion",
        HostMessage::ConfirmGraphKeyDeletion(_) => "confirm_graph_key_deletion",
        HostMessage::Status(_) => "status",
        HostMessage::BeginCoreMaterialization(_) => "begin_core_materialization",
        HostMessage::ApplyCoreSourceDeltaPage(_) => "apply_core_source_delta_page",
        HostMessage::MaterializeCoreRecordPage(_) => "materialize_core_record_page",
        HostMessage::FinishCoreMaterialization(_) => "finish_core_materialization",
        HostMessage::Blame(_) => "blame",
    }
}

fn helper_kind(message: &HelperMessage) -> &'static str {
    match message {
        HelperMessage::Hello(_) => "hello",
        HelperMessage::Authorized(_) => "authorized",
        HelperMessage::GraphKeyDeletionPrepared(_) => "graph_key_deletion_prepared",
        HelperMessage::GraphKeyDeleted(_) => "graph_key_deleted",
        HelperMessage::Status(_) => "status",
        HelperMessage::CoreMaterializationBegan(_) => "core_materialization_began",
        HelperMessage::CoreSourceDeltaPageApplied(_) => "core_source_delta_page_applied",
        HelperMessage::CoreRecordPageMaterialized(_) => "core_record_page_materialized",
        HelperMessage::CoreMaterializationFinished(_) => "core_materialization_finished",
        HelperMessage::Blame(_) => "blame",
        HelperMessage::Error(_) => "error",
    }
}

#[test]
fn canonical_inventory_and_exported_fingerprint_are_exact() {
    let value = inventory();
    let bytes = serde_json::to_vec(&value["canonical_inventory"]).unwrap();
    let digest = hex(&Sha256::digest(&bytes));
    assert_eq!(value["canonical_sha256"], digest);
    assert_eq!(PROTOCOL_FINGERPRINT, digest);
    assert_eq!(
        value["canonical_inventory"]["fingerprint"]["runtime_value"],
        "<sha256-of-this-canonical-inventory>"
    );
}

#[test]
fn inventory_freezes_core_capability_and_exact_message_sequence() {
    let canonical = &inventory()["canonical_inventory"];
    let capabilities = canonical["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert!(capabilities.contains("core_materialization"));
    let retired_capability = ["source", "_materialization"].concat();
    assert!(!capabilities
        .iter()
        .any(|name| name.contains(&retired_capability)));
    assert_eq!(
        canonical["core_materialization"]["sequence"],
        serde_json::json!([
            "begin_core_materialization",
            "apply_core_source_delta_page",
            "materialize_core_record_page",
            "finish_core_materialization"
        ])
    );
    let encoded = serde_json::to_string(canonical).unwrap();
    assert!(!encoded.contains(&["source", "_manifest"].concat()));
    assert!(!encoded.contains(&["hydra", "tion"].concat()));
    assert!(!encoded.contains("previous_page_sha256"));
    assert!(!encoded.contains("receipt_sha256"));
}

#[test]
fn every_generated_frame_round_trips_and_names_match_typed_kinds() {
    let value = inventory();
    for (name, encoded) in value["golden_vectors"]["host_frames"].as_object().unwrap() {
        let bytes = unhex(encoded.as_str().unwrap());
        let envelope = read_frame::<_, HostEnvelope>(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(host_kind(&envelope.message), name);
        let mut round_trip = Vec::new();
        write_frame(&mut round_trip, &envelope).unwrap();
        assert_eq!(round_trip, bytes);
    }
    for (name, encoded) in value["golden_vectors"]["helper_frames"]
        .as_object()
        .unwrap()
    {
        let bytes = unhex(encoded.as_str().unwrap());
        let envelope = read_frame::<_, HelperEnvelope>(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(helper_kind(&envelope.message), name);
        let mut round_trip = Vec::new();
        write_frame(&mut round_trip, &envelope).unwrap();
        assert_eq!(round_trip, bytes);
    }
}

#[test]
fn inventory_freezes_reviewed_status_axes_and_incremental_ack_subset() {
    let canonical = &inventory()["canonical_inventory"];
    assert_eq!(
        canonical["status_contract"]["axes"],
        serde_json::json!([
            "core_projection_currentness",
            "materialized_target_coverage",
            "repository_coverage",
            "access",
            "supported_operations",
            "available_operations"
        ])
    );
    assert_eq!(
        canonical["status_contract"]["repository_coverage_axes"],
        serde_json::json!([
            "repository_candidate_events",
            "logical_binding_events",
            "certified_live_root_access_events",
            "file_evidence_events",
            "exact_commit_evidence_events",
            "exact_pull_request_evidence_events"
        ])
    );
    assert_eq!(
        canonical["status_contract"]["operation_prerequisites"]["global"],
        serde_json::json!(["current_complete_projection", "entitlement", "graph_key"])
    );
    assert_eq!(
        canonical["status_contract"]["operation_prerequisites"]["file_blame"],
        serde_json::json!([
            "local_repository",
            "certified_live_root_access_events",
            "file_evidence_events"
        ])
    );
    assert_eq!(
        canonical["status_contract"]["operation_prerequisites"]["commit_blame"],
        serde_json::json!(["exact_commit_evidence_events"])
    );
    assert_eq!(
        canonical["status_contract"]["operation_prerequisites"]["pull_request_blame"],
        serde_json::json!(["exact_pull_request_evidence_events"])
    );
    let ack = canonical["dto_fields"]["CoreSourceDeltaPageApplied"]["required"]
        .as_array()
        .unwrap();
    assert!(ack.contains(&serde_json::json!("materialize_sources")));
}
