use std::{collections::BTreeSet, io::Cursor};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::*;

fn inventory() -> Value {
    serde_json::from_str(include_str!("../testdata/v2/inventory.json")).expect("protocol inventory")
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
        HostMessage::ApplyCoreEventDeltaPages(_) => "apply_core_event_delta_pages",
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
        HelperMessage::CoreEventDeltaPagesApplied(_) => "core_event_delta_pages_applied",
    }
}

const CURRENT_EVENT_DELTA_REQUEST_KIND: &str = "apply_core_event_delta_pages";
const CURRENT_EVENT_DELTA_RESPONSE_KIND: &str = "core_event_delta_pages_applied";
const LEGACY_EVENT_DELTA_REQUEST_KIND: &str = "apply_core_event_delta_page";

fn validate_core_sequence_message_kinds(canonical: &Value) -> Result<(), String> {
    let sequence = canonical["core_materialization"]["sequence"]
        .as_array()
        .ok_or_else(|| "core materialization sequence is not an array".to_owned())?;
    let host_kinds = canonical["host_message_kinds"]
        .as_array()
        .ok_or_else(|| "host message kinds is not an array".to_owned())?;
    let helper_kinds = canonical["helper_message_kinds"]
        .as_array()
        .ok_or_else(|| "helper message kinds is not an array".to_owned())?;

    for operation in sequence {
        let operation = operation
            .as_str()
            .ok_or_else(|| "core materialization operation is not a string".to_owned())?;
        if !host_kinds.iter().any(|kind| kind == operation) {
            return Err(format!(
                "core materialization operation {operation} is not a host message kind"
            ));
        }
    }
    if !sequence
        .iter()
        .any(|kind| kind == CURRENT_EVENT_DELTA_REQUEST_KIND)
    {
        return Err(format!(
            "core materialization sequence does not prescribe {CURRENT_EVENT_DELTA_REQUEST_KIND}"
        ));
    }
    if sequence
        .iter()
        .any(|kind| kind == LEGACY_EVENT_DELTA_REQUEST_KIND)
    {
        return Err(format!(
            "core materialization sequence prescribes legacy {LEGACY_EVENT_DELTA_REQUEST_KIND}"
        ));
    }
    if !helper_kinds
        .iter()
        .any(|kind| kind == CURRENT_EVENT_DELTA_RESPONSE_KIND)
    {
        return Err(format!(
            "helper message kinds omit current {CURRENT_EVENT_DELTA_RESPONSE_KIND}"
        ));
    }
    Ok(())
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
    validate_core_sequence_message_kinds(canonical).unwrap();
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
            "apply_core_event_delta_pages",
            "finish_core_materialization"
        ])
    );
    let encoded = serde_json::to_string(canonical).unwrap();
    assert!(!encoded.contains(&["source", "_manifest"].concat()));
    assert!(!encoded.contains(&["hydra", "tion"].concat()));
    assert!(!encoded.contains("previous_page_sha256"));
    assert!(!encoded.contains("receipt_sha256"));

    assert_eq!(
        canonical["host_message_kinds"],
        serde_json::json!([
            "hello",
            "authorize",
            "prepare_graph_key_deletion",
            "confirm_graph_key_deletion",
            "status",
            "begin_core_materialization",
            "apply_core_source_delta_page",
            "core_event_state_page",
            "apply_core_event_delta_page",
            "finish_core_materialization",
            "blame",
            "apply_core_event_delta_pages"
        ])
    );
    assert_eq!(
        canonical["helper_message_kinds"],
        serde_json::json!([
            "hello",
            "authorized",
            "graph_key_deletion_prepared",
            "graph_key_deleted",
            "status",
            "core_materialization_began",
            "core_source_delta_page_applied",
            "core_event_state_page",
            "core_event_delta_page_applied",
            "core_materialization_finished",
            "blame",
            "error",
            "core_event_delta_pages_applied"
        ])
    );
    assert_eq!(canonical["bounds"]["core_event_delta_pages"], 16);
    assert_eq!(
        canonical["bounds"]["core_event_delta_pages_request_wire_bytes"],
        68 * 1024 * 1024
    );
    assert_eq!(
        canonical["bounds"]["core_event_delta_pages_prepared_output_bytes"],
        128 * 1024 * 1024
    );
    assert_eq!(
        canonical["dto_fields"]["ApplyCoreEventDeltaPageRequest"]["required"],
        serde_json::json!(["page"])
    );
    assert_eq!(
        canonical["dto_fields"]["CoreEventDeltaPageApplied"]["required"],
        serde_json::json!([
            "materialization_id",
            "core_generation_id",
            "source",
            "page_index",
            "additions",
            "replacements",
            "tombstones",
            "terminal",
            "replayed"
        ])
    );
    assert_eq!(
        canonical["dto_fields"]["ApplyCoreEventDeltaPagesRequest"]["required"],
        serde_json::json!(["pages"])
    );
    assert_eq!(
        canonical["dto_fields"]["CoreEventDeltaPagesApplied"]["required"],
        serde_json::json!(["pages"])
    );
}

#[test]
fn core_sequence_message_kind_conformance_rejects_contradictions() {
    let mut singular_sequence = inventory()["canonical_inventory"].clone();
    singular_sequence["core_materialization"]["sequence"][3] =
        serde_json::json!(LEGACY_EVENT_DELTA_REQUEST_KIND);
    let error = validate_core_sequence_message_kinds(&singular_sequence).unwrap_err();
    assert!(error.contains(CURRENT_EVENT_DELTA_REQUEST_KIND));

    let mut unknown_operation = inventory()["canonical_inventory"].clone();
    unknown_operation["core_materialization"]["sequence"][3] =
        serde_json::json!("apply_unlisted_core_event_delta_pages");
    let error = validate_core_sequence_message_kinds(&unknown_operation).unwrap_err();
    assert!(error.contains("not a host message kind"));

    let mut missing_response = inventory()["canonical_inventory"].clone();
    missing_response["helper_message_kinds"]
        .as_array_mut()
        .unwrap()
        .retain(|kind| kind != CURRENT_EVENT_DELTA_RESPONSE_KIND);
    let error = validate_core_sequence_message_kinds(&missing_response).unwrap_err();
    assert!(error.contains(CURRENT_EVENT_DELTA_RESPONSE_KIND));
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
            "fingerprint": "bc73c991e160746fbaaddb641fdce8c7bec24e5ba212a406ec26d197cf0c6a5e",
            "leaf": {
                "helper": "ctx_pro_host_protocol::core_record_leaf_sha256",
                "paired_helper": "ctx_pro_host_protocol::core_record_digests",
                "domain": "ctx-core-record-leaf-v1\0",
                "algorithm": "sha256(domain_then_canonical_event_id_then_u64_be_exact_canonical_core_record_json_length_then_exact_canonical_core_record_json)",
                "added_path": "CoreEventDelta.added.value",
                "replaced_path": "CoreEventDelta.replaced.value.record"
            },
            "accumulator": {
                "identity": "ctx-core-record-event-binding-v1\0",
                "algorithm": "sum_mod_2^256(sha256(identity_then_u64_be_canonical_event_id_length_then_canonical_event_id_then_core_record_leaf))"
            },
            "repository_contract_revision": 8,
            "repository_observation_revision": 4,
            "bounded_shell_subset_revision": 3,
            "repository_association_policy_revision": 6,
            "repository_pull_request_association_capture_revision": 3,
            "repository_outcome_capture_revision": 4,
            "repository_local_root_authorization_fingerprint_revision": 1,
            "mcp_tool_call_attribution_revision": 1,
            "mcp_tool_call_attribution": {
                "wire_path": "CoreRecord.mcp_tool_call",
                "presence": "optional_omitted_when_absent_explicit_null_rejected",
                "shape": "exact_server_and_tool_string_pair",
                "component_bound": "decoded_utf8_bytes",
                "maximum_component_bytes": 65_536
            },
            "repository_candidate_set": "strictly_sorted_unique_kind_and_path_pairs",
            "repository_file_invocation_evidence_set": "strictly_sorted_unique_typed_request_intent_bound_to_repository_and_normalized_body"
        })
    );
    assert_eq!(
        value["golden_vectors"]["core_record_digests"],
        serde_json::json!({
            "core_record_sha256":
                "618d194dea547828014c828028e2b2cf2b06663ae9ca6e7d0a7ea4cba22961a0",
            "core_record_leaf_sha256":
                "1e265db24d2ed62287acfe7224df0315e53e30c52fc802a0e2364e7a73d7dd95"
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
    assert_eq!(evidence["repository_observation_revision"], 4);
    assert_eq!(evidence["association_policy_revision"], 6);
    assert_eq!(evidence["outcome_capture_revision"], 4);
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
fn inventory_freezes_mcp_tool_call_wire_contract() {
    let value = inventory();
    let canonical = &value["canonical_inventory"];
    assert_eq!(
        canonical["bounds"]["mcp_tool_call_attribution_component_bytes"],
        serde_json::json!(65_536)
    );
    assert_eq!(
        canonical["dto_fields"]["CoreRecord"]["optional"],
        serde_json::json!(["mcp_tool_call"])
    );
    assert_eq!(
        canonical["dto_fields"]["McpToolCallAttribution"],
        serde_json::json!({"required": ["server", "tool"], "optional": []})
    );

    let frame = unhex(
        value["golden_vectors"]["host_frames"]["apply_core_event_delta_page"]
            .as_str()
            .unwrap(),
    );
    let envelope: Value = serde_json::from_slice(&frame[FRAME_HEADER_BYTES..]).unwrap();
    let absent = envelope["message"]["body"]["page"]["deltas"][0]["value"].clone();
    assert!(absent.get("mcp_tool_call").is_none());
    let decoded_absent: CoreRecord = serde_json::from_value(absent.clone()).unwrap();
    assert!(decoded_absent.mcp_tool_call.is_none());
    assert!(serde_json::to_value(decoded_absent)
        .unwrap()
        .get("mcp_tool_call")
        .is_none());

    let expected = serde_json::json!({"server": "filesystem", "tool": "read_file"});
    let mut attributed = absent.clone();
    attributed
        .as_object_mut()
        .unwrap()
        .insert("mcp_tool_call".to_owned(), expected.clone());
    assert_inventory_fields_match_actual(canonical, "CoreRecord", &attributed);
    assert_inventory_fields_match_actual(
        canonical,
        "McpToolCallAttribution",
        &attributed["mcp_tool_call"],
    );
    let decoded_attributed: CoreRecord = serde_json::from_value(attributed).unwrap();
    assert_eq!(
        serde_json::to_value(decoded_attributed).unwrap()["mcp_tool_call"],
        expected
    );

    let mut explicit_null = absent;
    explicit_null
        .as_object_mut()
        .unwrap()
        .insert("mcp_tool_call".to_owned(), Value::Null);
    assert!(serde_json::from_value::<CoreRecord>(explicit_null).is_err());
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
fn inventory_freezes_the_blame_result_snapshot_outcome_and_diagnostic_contract() {
    let value = inventory();
    let canonical = &value["canonical_inventory"];
    assert_eq!(canonical["protocol_version"], serde_json::json!(2));
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
    assert_eq!(
        canonical["dto_fields"]["BlameResult"]["required"],
        serde_json::json!([
            "snapshot",
            "target",
            "git_snapshot",
            "outcome",
            "matches",
            "evidence",
            "next"
        ])
    );
    assert_eq!(
        canonical["dto_fields"]["BlameCoverage"]["required"],
        serde_json::json!([
            "unit",
            "evaluated",
            "proven",
            "possible",
            "conflicting",
            "none"
        ])
    );
    assert_eq!(
        canonical["enums"]["blame_attribution"],
        serde_json::json!(["proven", "possible", "conflicting", "none"])
    );
    assert_eq!(
        canonical["bounds"]["blame_diagnostic_candidates"],
        serde_json::json!(5)
    );

    let error = read_frame::<_, HelperEnvelope>(&mut Cursor::new(unhex(
        value["golden_vectors"]["helper_frames"]["error"]
            .as_str()
            .expect("error response frame"),
    )))
    .expect("error response");
    let HelperMessage::Error(error) = error.message else {
        panic!("error response kind");
    };
    let error = serde_json::to_value(error).expect("protocol error JSON");
    assert_inventory_fields_match_actual(canonical, "ProtocolError", &error);
    assert!(error["details"].is_null());

    let details = BlameDiagnosticDetails {
        reason: BlameDiagnosticReason::RepositoryAmbiguous,
        candidates: vec![
            BlameDiagnosticCandidate::Repository {
                selector: "forge:github.com/ctxrs/ctx".to_owned(),
            },
            BlameDiagnosticCandidate::Repository {
                selector: "workspace:ctx".to_owned(),
            },
        ],
        candidates_truncated: false,
    };
    assert_inventory_fields_match_actual(
        canonical,
        "BlameDiagnosticDetails",
        &serde_json::to_value(&details).expect("blame diagnostic details JSON"),
    );
    for (name, candidate) in [
        (
            "BlameDiagnosticCandidate.repository",
            BlameDiagnosticCandidate::Repository {
                selector: "workspace:ctx".to_owned(),
            },
        ),
        (
            "BlameDiagnosticCandidate.commit",
            BlameDiagnosticCandidate::Commit {
                repository: "workspace:ctx".to_owned(),
                oid: "a".repeat(40),
            },
        ),
    ] {
        assert_inventory_fields_match_actual(
            canonical,
            name,
            &serde_json::to_value(candidate).expect("blame diagnostic candidate JSON"),
        );
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
