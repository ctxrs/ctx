use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    McpToolCallAttribution, NativeItemKey, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};

const SERVER_CANARY: &str = "zzsrvcoresearchcanary4c18vjqx";
const TOOL_CANARY: &str = "zztoolcoresearchcanary6d29wknp";
const BODY_ORACLE: &str = "provider neutral attribution ranking oracle";

fn source() -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl_tree",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("mcp-attribution-search.jsonl").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn record(source: &SourceKey, sequence: u64) -> CoreRecord {
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("mcp-search-session").unwrap())
            .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        sequence,
        "tool_result",
        "primary",
        true,
        "mcp-attribution-search-test-v1",
        BODY_ORACLE,
    )
    .unwrap();
    record.native_event_id = Some(TypedKey::U64(sequence));
    record
}

fn certificate(source: &SourceKey) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "mcp-attribution-search-test-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: 2,
            retained_records: 2,
            indexed_documents: 2,
            certified_bytes: 2,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

#[test]
fn mcp_tool_call_attribution_is_stored_but_never_indexed_or_ranked() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let mut attributed = record(&source, 1);
    attributed.mcp_tool_call = Some(McpToolCallAttribution {
        server: SERVER_CANARY.to_owned(),
        tool: TOOL_CANARY.to_owned(),
    });
    attributed.validate_contract().unwrap();
    let attributed_id = attributed.event_id;

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(attributed).unwrap();
    writer.add_core_record(record(&source, 2)).unwrap();
    writer.certify_source(certificate(&source)).unwrap();
    writer.commit(|_| true).unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    let body_matches = index.search_event_candidates(BODY_ORACLE, 10).unwrap();
    assert_eq!(body_matches.len(), 2);
    assert_eq!(body_matches[0].score, body_matches[1].score);

    let stored = index
        .core_record_by_id(attributed_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.mcp_tool_call,
        Some(McpToolCallAttribution {
            server: SERVER_CANARY.to_owned(),
            tool: TOOL_CANARY.to_owned(),
        })
    );

    for canary in [SERVER_CANARY, TOOL_CANARY] {
        assert!(
            index
                .search_event_candidates(canary, 10)
                .unwrap()
                .is_empty(),
            "provider-neutral MCP attribution became searchable: {canary}"
        );
    }
}
