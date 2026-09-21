use sha2::{Digest as _, Sha256};

use ctx_attribution_index::{
    FLAT_SERVING_ROLE, FlatSegmentWriter, FlatStore, MANIFEST_SCHEMA_VERSION, SegmentManifest,
    SegmentRef, SegmentStore, SegmentWriter, random_generation_id, segment_file_name,
};

use crate::graph::segment_graph::{
    SEGMENT_EVIDENCE_IDENTITY, SEGMENT_ORDERING_IDENTITY, SEGMENT_SCHEMA_IDENTITY, SegmentGraph,
};
use crate::protocol::{CoreMaterializationReceipt, core_record_contract_fingerprint};

pub(crate) const TEST_MATERIALIZER_REVISION: &str = "2026.09.20.1+core-attribution";

pub(crate) fn empty_graph() -> (tempfile::TempDir, SegmentGraph) {
    let directory = tempfile::tempdir().expect("work-graph test directory");
    let root = directory.path().join("graph");
    ctx_history_platform::platform_security::establish_private_data_root(&root)
        .expect("private work-graph test root");

    let generation = [0x51; 32];
    let generation_id = hex::encode(generation);
    let file_name = segment_file_name(&generation_id, FLAT_SERVING_ROLE);
    let path = root.join(&file_name);
    let writer = SegmentWriter::create(
        &path,
        generation,
        FLAT_SERVING_ROLE,
        ctx_attribution_index::SEGMENT_CHUNK_BYTES,
    )
    .expect("checked test segment");
    let stats =
        FlatSegmentWriter::write(writer, Vec::new(), Vec::new()).expect("empty Flat test segment");
    let segment = SegmentRef {
        ordinal: 0,
        publication_generation: 1,
        generation_id,
        role: FLAT_SERVING_ROLE,
        file_name,
        plaintext_bytes: stats.plaintext_bytes,
        file_sha256: hex::encode(Sha256::digest(
            std::fs::read(&path).expect("checked test bytes"),
        )),
    };
    let receipt = CoreMaterializationReceipt {
        core_generation_id: "a".repeat(64),
        core_record_contract_fingerprint: core_record_contract_fingerprint(),
        source_snapshot_sha256: "b".repeat(64),
        materializer_revision: TEST_MATERIALIZER_REVISION.to_owned(),
        source_count: 0,
        event_count: 0,
    };
    let manifest = SegmentManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        generation_id: random_generation_id().expect("manifest generation"),
        prior_generation_id: None,
        graph_generation: 1,
        materializer_identity: receipt.materializer_revision.clone(),
        core_receipt: receipt,
        schema_identity: SEGMENT_SCHEMA_IDENTITY.to_owned(),
        evidence_identity: SEGMENT_EVIDENCE_IDENTITY.to_owned(),
        ordering_identity: SEGMENT_ORDERING_IDENTITY.to_owned(),
        segments: vec![segment],
        predecessor_segments: Vec::new(),
    };
    let store = SegmentStore::new(&root);
    let candidate = store
        .stage_manifest(&manifest)
        .expect("stage work-graph test manifest");
    store
        .publish_candidate(candidate)
        .expect("publish work-graph test manifest");
    let pinned = FlatStore::new(&root)
        .open_active(SegmentGraph::flat_open_policy())
        .expect("pin work-graph test generation");
    (directory, SegmentGraph::from_pinned(pinned, None))
}

/// Entirely authored record shared by graph and projection tests.
pub(crate) fn core_record() -> crate::protocol::CoreRecord {
    use ctx_history_core::*;
    let source = SourceKey::derive(
        "zed",
        "zed_threads_sqlite",
        "zed-nativepath-sqlite-v0",
        1,
        SourceAnchor::provider_native(
            "thread-database",
            TypedKey::utf8("attribution-fixture.db").unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let native_session =
        NativeSessionKey::native_id("session", TypedKey::utf8("authored-session").unwrap())
            .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &native_session,
    })
    .unwrap();
    let native_item = NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source,
        1,
        "message",
        "authored-attribution-fixture-v1",
        "Authored attribution test message".to_owned(),
    )
    .unwrap();
    record.root_session_id = Some(session_id);
    record.validate_contract().unwrap();
    record
}
