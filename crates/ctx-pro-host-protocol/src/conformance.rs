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

fn assert_inventory_fields_match_actual(canonical: &Value, name: &str, actual: &Value) {
    let declared = canonical["dto_fields"][name]["required"]
        .as_array()
        .expect("required DTO fields")
        .iter()
        .chain(
            canonical["dto_fields"][name]["optional"]
                .as_array()
                .expect("optional DTO fields"),
        )
        .map(|field| field.as_str().expect("DTO field name"))
        .collect::<BTreeSet<_>>();
    let actual = actual
        .as_object()
        .expect("serialized DTO object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(declared, actual, "inventory field drift for {name}");
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
        HostMessage::CoreEventStatePage(_) => "core_event_state_page",
        HostMessage::ApplyCoreEventDeltaPage(_) => "apply_core_event_delta_page",
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
        HelperMessage::CoreEventStatePage(_) => "core_event_state_page",
        HelperMessage::CoreEventDeltaPageApplied(_) => "core_event_delta_page_applied",
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
            "core_event_state_page",
            "apply_core_event_delta_page",
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
fn inventory_freezes_candidate_sets_and_active_repository_revisions() {
    let value = inventory();
    let canonical = &value["canonical_inventory"];
    assert_eq!(
        canonical["dto_fields"]["RepositoryCandidateEvidence"]["required"],
        serde_json::json!([
            "repository_observation_revision",
            "bounded_shell_subset_revision",
            "association_policy_revision",
            "outcome_capture_revision",
            "candidates"
        ])
    );
    assert_eq!(
        canonical["enums"]["repository_candidate_kind"],
        serde_json::json!([
            "session_cwd",
            "declared_tool_workdir",
            "derived_effective_cwd",
            "command_specific_repository_path",
            "file_activity_path",
            "vcs_activity_path",
            "outcome_operation_repository_path",
            "outcome_output_repository_path"
        ])
    );
    assert_eq!(
        canonical["core_record_contract"],
        serde_json::json!({
            "fingerprint": "931172482d0ee38770bc21b173bfd0c304c2d1d8e0dfa0c51b2654ec2134a7e7",
            "repository_contract_revision": 7,
            "repository_observation_revision": 2,
            "bounded_shell_subset_revision": 1,
            "repository_association_policy_revision": 3,
            "repository_outcome_capture_revision": 2,
            "repository_local_root_authorization_fingerprint_revision": 1,
            "repository_candidate_set": "strictly_sorted_unique_kind_and_path_pairs",
            "repository_file_invocation_evidence_set": "strictly_sorted_unique_typed_request_intent_bound_to_repository_and_normalized_body"
        })
    );

    let frame = unhex(
        value["golden_vectors"]["host_frames"]["apply_core_event_delta_page"]
            .as_str()
            .unwrap(),
    );
    let envelope: Value = serde_json::from_slice(&frame[FRAME_HEADER_BYTES..]).unwrap();
    let evidence =
        &envelope["message"]["body"]["page"]["deltas"][0]["value"]["repository_candidate_evidence"];
    assert_eq!(evidence["repository_observation_revision"], 2);
    assert_eq!(evidence["association_policy_revision"], 3);
    assert_eq!(evidence["outcome_capture_revision"], 2);
    assert_eq!(
        evidence["candidates"],
        serde_json::json!([
            {"kind": "session_cwd", "path": "/golden/repo"},
            {"kind": "file_activity_path", "path": "/golden/repo/src/lib.rs"}
        ])
    );
    assert!(evidence.get("declared_tool_workdir").is_none());
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
fn inventory_freezes_the_blame_result_snapshot_binding() {
    let value = inventory();
    let canonical = &value["canonical_inventory"];
    let envelope = read_frame::<_, HelperEnvelope>(&mut Cursor::new(unhex(
        value["golden_vectors"]["helper_frames"]["blame"]
            .as_str()
            .expect("blame response frame"),
    )))
    .expect("blame response");
    let HelperMessage::Blame(result) = envelope.message else {
        panic!("blame response kind");
    };
    assert_inventory_fields_match_actual(
        canonical,
        "BlameResult",
        &serde_json::to_value(result).expect("blame response JSON"),
    );
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
            "file_evidence_events",
            "exact_commit_evidence_events"
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
    assert!(ack.contains(&serde_json::json!("reconcile_sources")));
    assert!(ack.contains(&serde_json::json!("acknowledgement_page_index")));
    assert!(ack.contains(&serde_json::json!("acknowledgement_terminal")));
    assert_eq!(
        canonical["dto_fields"]["ApplyCoreSourceDeltaPageRequest"]["required"],
        serde_json::json!(["page", "acknowledgement_page_index"])
    );
    assert_eq!(
        canonical["bounds"]["core_source_acknowledgement_page_items"],
        canonical["bounds"]["core_source_delta_page_items"]
    );
    assert!(
        canonical["bounds"]["core_control_wire_bytes"]
            .as_u64()
            .unwrap()
            < canonical["framing"]["maximum_payload_bytes"]
                .as_u64()
                .unwrap()
    );
}

#[test]
fn inventory_source_removal_and_reconciliation_fields_match_actual_dtos() {
    let value = inventory();
    let canonical = &value["canonical_inventory"];
    let envelope = read_frame::<_, HelperEnvelope>(&mut Cursor::new(unhex(
        value["golden_vectors"]["helper_frames"]["core_source_delta_page_applied"]
            .as_str()
            .expect("source acknowledgement frame"),
    )))
    .expect("source acknowledgement");
    let HelperMessage::CoreSourceDeltaPageApplied(response) = envelope.message else {
        panic!("source acknowledgement kind");
    };
    let removal = response
        .reconcile_sources
        .iter()
        .find_map(|reconciliation| match &reconciliation.delta {
            CoreSourceDelta::Removed(removal) => Some((reconciliation, removal)),
            CoreSourceDelta::Present(_) => None,
        })
        .expect("golden removal reconciliation");
    assert_inventory_fields_match_actual(
        canonical,
        "CoreSourceReconciliation",
        &serde_json::to_value(removal.0).expect("reconciliation JSON"),
    );
    assert_inventory_fields_match_actual(
        canonical,
        "CoreSourceRemoval",
        &serde_json::to_value(removal.1).expect("removal JSON"),
    );
}
