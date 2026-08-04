use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    McpExchangeContent, McpInvocationContent, McpJsonCapture, McpTerminalResponseContent,
    McpTerminalStatus, McpTextCapture, McpToolCallAttribution, NativeItemKey, NativeSessionKey,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation,
    TypedKey,
};
use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};

const SERVER_CANARY: &str = "zzsrvcoresearchcanary4c18vjqx";
const TOOL_CANARY: &str = "zztoolcoresearchcanary6d29wknp";
const CALL_ID_CANARY: &str = "zzcallidcoresearchcanary7g52skmp";
const ARGUMENT_CANARY: &str = "zzargumentcoresearchcanary8h63tlmq";
const RESPONSE_CANARY: &str = "zzresponsecoresearchcanary9j74vmnr";
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
fn mcp_attribution_and_exchange_are_stored_but_never_indexed_or_ranked() {
    let temp = tempfile::tempdir().unwrap();
    let source = source();
    let mut attributed = record(&source, 1);
    attributed.mcp_tool_call = Some(McpToolCallAttribution {
        server: SERVER_CANARY.to_owned(),
        tool: TOOL_CANARY.to_owned(),
    });
    let exchange = McpExchangeContent {
        provider_call_id: CALL_ID_CANARY.to_owned(),
        invocation: Some(McpInvocationContent {
            server: SERVER_CANARY.to_owned(),
            tool: TOOL_CANARY.to_owned(),
            arguments: McpJsonCapture::Present {
                value: serde_json::json!({"only_in_mcp_exchange": ARGUMENT_CANARY}),
            },
        }),
        response: Some(McpTerminalResponseContent {
            status: McpTerminalStatus::Succeeded,
            failure_kind: None,
            duration_ns: Some(42),
            text: McpTextCapture::NormalizedBody,
            payload: McpJsonCapture::Present {
                value: serde_json::json!({"only_in_mcp_exchange": RESPONSE_CANARY}),
            },
        }),
    };
    attributed.content.mcp_exchange = Some(exchange.clone());
    attributed.validate_contract().unwrap();
    let attributed_id = attributed.event_id;
    let attributed_content_bytes = attributed.content.encoded_content_bytes().unwrap();
    let plain = record(&source, 2);
    let plain_content_bytes = plain.content.encoded_content_bytes().unwrap();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(attributed).unwrap();
    writer.add_core_record(plain).unwrap();
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
    assert_eq!(stored.content.mcp_exchange, Some(exchange.clone()));

    let page = index.core_source_event_page(&source, None, 10).unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(
        page.content_bytes,
        attributed_content_bytes + plain_content_bytes
    );
    assert_eq!(
        page.items
            .iter()
            .find(|item| item.core_record.event_id == attributed_id)
            .unwrap()
            .core_record
            .content
            .mcp_exchange,
        Some(exchange)
    );

    for canary in [
        SERVER_CANARY,
        TOOL_CANARY,
        CALL_ID_CANARY,
        ARGUMENT_CANARY,
        RESPONSE_CANARY,
    ] {
        assert!(
            index
                .search_event_candidates(canary, 10)
                .unwrap()
                .is_empty(),
            "provider-neutral MCP data became searchable: {canary}"
        );
    }
}
