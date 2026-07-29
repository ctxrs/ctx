use std::{collections::BTreeSet, io::Cursor};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::*;

fn inventory() -> Value {
    serde_json::from_str(include_str!("../testdata/v1/inventory.json")).expect("protocol inventory")
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
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
        HostMessage::BeginSourceManifest(_) => "begin_source_manifest",
        HostMessage::BeginSourceManifestAdmission(_) => "begin_source_manifest_admission",
        HostMessage::AdmitSourceManifestPage(_) => "admit_source_manifest_page",
        HostMessage::FinishSourceManifestAdmission(_) => "finish_source_manifest_admission",
        HostMessage::PrepareSource(_) => "prepare_source",
        HostMessage::MaterializeSourcePage(_) => "materialize_source_page",
        HostMessage::DeleteSource(_) => "delete_source",
        HostMessage::FinishSourceManifest(_) => "finish_source_manifest",
        HostMessage::FinishAdmittedSourceManifest(_) => "finish_admitted_source_manifest",
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
        HelperMessage::SourceManifestBegan(_) => "source_manifest_began",
        HelperMessage::SourceManifestAdmissionBegan(_) => "source_manifest_admission_began",
        HelperMessage::SourceManifestPageAdmitted(_) => "source_manifest_page_admitted",
        HelperMessage::SourceManifestAdmitted(_) => "source_manifest_admitted",
        HelperMessage::SourcePrepared(_) => "source_prepared",
        HelperMessage::SourcePageMaterialized(_) => "source_page_materialized",
        HelperMessage::SourceDeleted(_) => "source_deleted",
        HelperMessage::SourceManifestFinished(_) => "source_manifest_finished",
        HelperMessage::Blame(_) => "blame",
        HelperMessage::Error(_) => "error",
    }
}

fn validate_host(message: &HostMessage) {
    match message {
        HostMessage::BeginSourceManifest(request) => request.validate().unwrap(),
        HostMessage::BeginSourceManifestAdmission(request) => request.validate().unwrap(),
        HostMessage::AdmitSourceManifestPage(request) => request.validate().unwrap(),
        HostMessage::FinishSourceManifestAdmission(request) => request.validate().unwrap(),
        HostMessage::PrepareSource(request) => request.validate().unwrap(),
        HostMessage::MaterializeSourcePage(request) => request.validate().unwrap(),
        HostMessage::DeleteSource(request) => request.validate().unwrap(),
        HostMessage::FinishSourceManifest(request) => request.validate().unwrap(),
        HostMessage::FinishAdmittedSourceManifest(request) => request.validate().unwrap(),
        HostMessage::Blame(request) => request.validate().unwrap(),
        HostMessage::Hello(_)
        | HostMessage::Authorize(_)
        | HostMessage::PrepareGraphKeyDeletion(_)
        | HostMessage::ConfirmGraphKeyDeletion(_)
        | HostMessage::Status(_) => {}
    }
}

fn validate_helper(message: &HelperMessage) {
    match message {
        HelperMessage::Status(result) => result.validate().unwrap(),
        HelperMessage::Blame(result) => result.validate().unwrap(),
        HelperMessage::SourceManifestBegan(result) => result.validate().unwrap(),
        HelperMessage::SourceManifestAdmitted(result) => result.validate().unwrap(),
        HelperMessage::SourcePrepared(result) => result.validate().unwrap(),
        HelperMessage::SourcePageMaterialized(result) => result.validate().unwrap(),
        HelperMessage::SourceDeleted(result) => result.validate().unwrap(),
        HelperMessage::SourceManifestFinished(result) => result.validate().unwrap(),
        HelperMessage::Hello(_)
        | HelperMessage::Authorized(_)
        | HelperMessage::GraphKeyDeletionPrepared(_)
        | HelperMessage::GraphKeyDeleted(_)
        | HelperMessage::SourceManifestAdmissionBegan(_)
        | HelperMessage::SourceManifestPageAdmitted(_)
        | HelperMessage::Error(_) => {}
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
fn inventory_freezes_current_capabilities_and_message_kinds() {
    let value = inventory();
    let canonical = &value["canonical_inventory"];
    let capabilities = canonical["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        capabilities,
        BTreeSet::from([
            "entitlement_authorization",
            "git_read",
            "graph_key_deletion",
            "query",
            "source_materialization",
            "status",
        ])
    );

    let host_kinds = canonical["host_message_kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let helper_kinds = canonical["helper_message_kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        host_kinds,
        BTreeSet::from([
            "admit_source_manifest_page",
            "authorize",
            "begin_source_manifest",
            "begin_source_manifest_admission",
            "blame",
            "confirm_graph_key_deletion",
            "delete_source",
            "finish_admitted_source_manifest",
            "finish_source_manifest",
            "finish_source_manifest_admission",
            "hello",
            "materialize_source_page",
            "prepare_graph_key_deletion",
            "prepare_source",
            "status",
        ])
    );
    assert_eq!(
        helper_kinds,
        BTreeSet::from([
            "authorized",
            "blame",
            "error",
            "graph_key_deleted",
            "graph_key_deletion_prepared",
            "hello",
            "source_deleted",
            "source_manifest_admission_began",
            "source_manifest_admitted",
            "source_manifest_began",
            "source_manifest_finished",
            "source_manifest_page_admitted",
            "source_page_materialized",
            "source_prepared",
            "status",
        ])
    );
}

#[test]
fn every_generated_frame_round_trips_and_validates() {
    let value = inventory();
    for encoded in value["golden_vectors"]["host_frames"]
        .as_object()
        .unwrap()
        .values()
    {
        let bytes = unhex(encoded.as_str().unwrap());
        let envelope = read_frame::<_, HostEnvelope>(&mut Cursor::new(&bytes)).unwrap();
        validate_host(&envelope.message);
        let mut round_trip = Vec::new();
        write_frame(&mut round_trip, &envelope).unwrap();
        assert_eq!(round_trip, bytes);
    }
    for encoded in value["golden_vectors"]["helper_frames"]
        .as_object()
        .unwrap()
        .values()
    {
        let bytes = unhex(encoded.as_str().unwrap());
        let envelope = read_frame::<_, HelperEnvelope>(&mut Cursor::new(&bytes)).unwrap();
        validate_helper(&envelope.message);
        let mut round_trip = Vec::new();
        write_frame(&mut round_trip, &envelope).unwrap();
        assert_eq!(round_trip, bytes);
    }
}

#[test]
fn golden_frame_names_match_typed_message_kinds() {
    let value = inventory();
    for (name, encoded) in value["golden_vectors"]["host_frames"].as_object().unwrap() {
        let envelope =
            read_frame::<_, HostEnvelope>(&mut Cursor::new(unhex(encoded.as_str().unwrap())))
                .unwrap();
        assert_eq!(host_kind(&envelope.message), name);
    }
    for (name, encoded) in value["golden_vectors"]["helper_frames"]
        .as_object()
        .unwrap()
    {
        let envelope =
            read_frame::<_, HelperEnvelope>(&mut Cursor::new(unhex(encoded.as_str().unwrap())))
                .unwrap();
        assert_eq!(helper_kind(&envelope.message), name);
    }
}

#[test]
fn source_manifest_admission_paging_and_transient_records_are_frozen() {
    let value = inventory();
    let host = value["golden_vectors"]["host_frames"].as_object().unwrap();
    for name in [
        "begin_source_manifest_admission",
        "admit_source_manifest_page",
        "finish_source_manifest_admission",
        "finish_admitted_source_manifest",
    ] {
        assert!(host.contains_key(name), "missing {name}");
    }
    let encoded = host["materialize_source_page"].as_str().unwrap();
    let envelope =
        read_frame::<_, HostEnvelope>(&mut Cursor::new(unhex(encoded))).expect("source page");
    let HostMessage::MaterializeSourcePage(request) = envelope.message else {
        panic!("materialize source page fixture kind");
    };
    request.validate().unwrap();
    assert_eq!(request.records.len(), 1);
    assert_eq!(request.records[0].facts.len(), 3);
}

#[test]
fn removed_citation_branch_is_rejected_as_an_unknown_field() {
    let value = serde_json::json!({
        "source_path": "fixture/session.jsonl",
        "fixture_line": 1,
        "provider_legacy": {}
    });
    assert!(serde_json::from_value::<EvidenceCitation>(value).is_err());
}
