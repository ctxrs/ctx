use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    McpExchangeContent, McpFailureKind, McpInvocationContent, McpJsonCapture,
    McpPayloadOmissionReason, McpTerminalResponseContent, McpTerminalStatus, McpTextCapture,
    McpToolCallAttribution, NativeItemKey, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, StableEntityId, TypedKey,
};
use ctx_history_index::{
    EventSearchFilters, GenerationWriter, SearchContentScope, VerifiedIndex, WriterOptions,
};

const BODY_ORACLE: &str = "provider neutral attribution ranking oracle";
const SERVER_CANARY: &str = "zzsrvcoresearchcanary4c18vjqx";
const TOOL_CANARY: &str = "zztoolcoresearchcanary6d29wknp";
const ARGUMENT_KEY_CANARY: &str = "zzargumentkeycanary8h63tlmq";
const ARGUMENT_VALUE_CANARY: &str = "zzargumentvaluecanary5g42rknp";
const CONTROL_VALUE_CANARY: &str = "zzcontrolvaluecanary2f31qjmo";
const CALL_ID_CANARY: &str = "zzcallidcoresearchcanary7g52skmp";
const SECOND_CALL_ID_CANARY: &str = "zzcallidcoresearchcanary3b14plqx";
const RESPONSE_KEY_CANARY: &str = "zzresponsekeycanary1a03okpw";
const RESPONSE_VALUE_CANARY: &str = "zzresponsevaluecanary9j74vmnr";
const ATTRIBUTION_SERVER_CANARY: &str = "zzattributionservercanary6d18vlrx";
const ATTRIBUTION_TOOL_CANARY: &str = "zzattributiontoolcanary7e29wmsy";
const STRUCTURED_CANARY: &str = "zzstructuredcontentcanary8f30xntz";
const OMITTED_ARGUMENT_BYTES: u64 = 998_877_665_544;
const RESPONSE_DURATION_NS: u64 = 424_242_424_242;

const SCOPE_CANARY: &str = "zzmcpscopecanary4c27vlqy";
const SCOPE_SERVER: &str = "zzmcpscopeserver5d38wmrz";
const SCOPE_TOOL: &str = "zzmcpscopetool6e49xnas";

fn source(name: &str) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl_tree",
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8(name).unwrap()).unwrap(),
    )
    .unwrap()
}

fn record(
    source: &SourceKey,
    sequence: u64,
    event_type: &str,
    normalized_body: &str,
) -> CoreRecord {
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
        event_type,
        "primary",
        true,
        "mcp-attribution-search-test-v2",
        normalized_body,
    )
    .unwrap();
    record.native_event_id = Some(TypedKey::U64(sequence));
    record
}

fn invocation(server: &str, tool: &str, arguments: McpJsonCapture) -> McpInvocationContent {
    McpInvocationContent {
        server: server.to_owned(),
        tool: tool.to_owned(),
        arguments,
    }
}

fn certificate(source: &SourceKey, count: u64) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "mcp-attribution-search-test-v2",
        [1; 32],
        ScannedSourceCounts {
            complete_records: count,
            retained_records: count,
            indexed_documents: count,
            certified_bytes: count,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn publish(source: &SourceKey, records: Vec<CoreRecord>) -> (tempfile::TempDir, VerifiedIndex) {
    let count = u64::try_from(records.len()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(certificate(source, count)).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();
    (temp, index)
}

fn matching_ids(index: &VerifiedIndex, query: &str) -> Vec<StableEntityId> {
    let mut ids = index
        .search_event_candidates(query, 20)
        .unwrap()
        .into_iter()
        .map(|candidate| candidate.event.event_id)
        .collect::<Vec<_>>();
    ids.sort_unstable_by_key(|id| id.as_uuid());
    ids
}

fn sorted_ids(ids: impl IntoIterator<Item = StableEntityId>) -> Vec<StableEntityId> {
    let mut ids = ids.into_iter().collect::<Vec<_>>();
    ids.sort_unstable_by_key(|id| id.as_uuid());
    ids
}

fn score_for(index: &VerifiedIndex, query: &str, event_id: StableEntityId) -> f32 {
    index
        .search_event_candidates(query, 20)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.event.event_id == event_id)
        .unwrap()
        .score
}

#[test]
fn mcp_invocation_projection_is_searchable_narrow_and_response_neutral() {
    let source = source("mcp-invocation-projection.jsonl");
    let arguments = serde_json::json!({
        ARGUMENT_KEY_CANARY: {
            "empty": {},
            "nested": [
                ARGUMENT_VALUE_CANARY,
                {"control": format!("line\n{CONTROL_VALUE_CANARY}\t\"quote\"\\slash")},
                [3, 1, 2]
            ],
            "unicode": ["Grüße", "東京"]
        }
    });

    let first_exchange = McpExchangeContent {
        provider_call_id: CALL_ID_CANARY.to_owned(),
        invocation: Some(invocation(
            SERVER_CANARY,
            TOOL_CANARY,
            McpJsonCapture::Present {
                value: arguments.clone(),
            },
        )),
        response: Some(McpTerminalResponseContent {
            status: McpTerminalStatus::Failed,
            failure_kind: Some(McpFailureKind::ToolReported),
            duration_ns: Some(RESPONSE_DURATION_NS),
            text: McpTextCapture::NormalizedBody,
            payload: McpJsonCapture::Present {
                value: serde_json::json!({RESPONSE_KEY_CANARY: RESPONSE_VALUE_CANARY}),
            },
        }),
    };
    let mut first = record(&source, 1, "tool_output", BODY_ORACLE);
    first.content.structured_content = Some(serde_json::json!({
        "structured_only": STRUCTURED_CANARY
    }));
    first.content.mcp_exchange = Some(first_exchange.clone());
    first.validate_contract().unwrap();
    let first_id = first.event_id;

    let mut second = record(&source, 2, "tool_output", BODY_ORACLE);
    second.content.mcp_exchange = Some(McpExchangeContent {
        provider_call_id: SECOND_CALL_ID_CANARY.to_owned(),
        invocation: Some(invocation(
            SERVER_CANARY,
            TOOL_CANARY,
            McpJsonCapture::Present { value: arguments },
        )),
        response: Some(McpTerminalResponseContent {
            status: McpTerminalStatus::Succeeded,
            failure_kind: None,
            duration_ns: None,
            text: McpTextCapture::Absent,
            payload: McpJsonCapture::Absent,
        }),
    });
    second.validate_contract().unwrap();
    let second_id = second.event_id;

    let captures = [
        McpJsonCapture::Absent,
        McpJsonCapture::Unavailable,
        McpJsonCapture::Omitted {
            reason: McpPayloadOmissionReason::SizeLimit,
            observed_encoded_bytes: Some(OMITTED_ARGUMENT_BYTES),
        },
    ];
    let mut records = vec![first, second];
    for (offset, arguments) in captures.into_iter().enumerate() {
        let sequence = u64::try_from(offset).unwrap() + 3;
        let mut state = record(
            &source,
            sequence,
            "tool_call",
            &format!("zzargumentstatebodyoracle{sequence}"),
        );
        state.content.mcp_exchange = Some(McpExchangeContent {
            provider_call_id: format!("zzstatecallidcanary{sequence}"),
            invocation: Some(invocation(
                "zzargumentstateservercanary",
                "zzargumentstatetoolcanary",
                arguments,
            )),
            response: None,
        });
        state.validate_contract().unwrap();
        records.push(state);
    }
    let mut empty_object = record(&source, 6, "tool_call", "zzemptyobjectbodyoracle");
    empty_object.content.mcp_exchange = Some(McpExchangeContent {
        provider_call_id: "zzemptyobjectcallidcanary".to_owned(),
        invocation: Some(invocation(
            "zzemptyobjectservercanary",
            "zzemptyobjecttoolcanary",
            McpJsonCapture::Present {
                value: serde_json::json!({}),
            },
        )),
        response: None,
    });
    empty_object.validate_contract().unwrap();
    records.push(empty_object);
    let mut attribution_only = record(&source, 7, "tool_call", "zzattributiononlybodyoracle");
    attribution_only.mcp_tool_call = Some(McpToolCallAttribution {
        server: ATTRIBUTION_SERVER_CANARY.to_owned(),
        tool: ATTRIBUTION_TOOL_CANARY.to_owned(),
    });
    attribution_only.validate_contract().unwrap();
    let attribution_only_id = attribution_only.event_id;
    records.push(attribution_only);

    let (_temp, index) = publish(&source, records);
    let expected_invocation_ids = sorted_ids([first_id, second_id]);
    for query in [
        SERVER_CANARY,
        TOOL_CANARY,
        ARGUMENT_KEY_CANARY,
        ARGUMENT_VALUE_CANARY,
        "quote",
        "Grüße",
        "東京",
    ] {
        assert_eq!(
            matching_ids(&index, query),
            expected_invocation_ids,
            "missing searchable invocation component: {query}"
        );
    }

    let body_matches = index.search_event_candidates(BODY_ORACLE, 20).unwrap();
    assert_eq!(body_matches.len(), 2);
    assert_eq!(
        score_for(&index, BODY_ORACLE, first_id),
        score_for(&index, BODY_ORACLE, second_id),
        "NormalizedBody response disposition duplicated the normalized body"
    );

    let stored = index
        .core_record_by_id(first_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(stored.content.mcp_exchange, Some(first_exchange));
    let stored_attribution = index
        .core_record_by_id(attribution_only_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        stored_attribution.mcp_tool_call,
        Some(McpToolCallAttribution {
            server: ATTRIBUTION_SERVER_CANARY.to_owned(),
            tool: ATTRIBUTION_TOOL_CANARY.to_owned(),
        })
    );

    for excluded in [
        CALL_ID_CANARY,
        SECOND_CALL_ID_CANARY,
        RESPONSE_KEY_CANARY,
        RESPONSE_VALUE_CANARY,
        ATTRIBUTION_SERVER_CANARY,
        ATTRIBUTION_TOOL_CANARY,
        STRUCTURED_CANARY,
        "failed",
        "succeeded",
        "tool_reported",
        "duration_ns",
        "normalized_body",
        "payload",
        "capture_status",
        "present",
        "absent",
        "unavailable",
        "omitted",
        "size_limit",
        "observed_encoded_bytes",
        "424242424242",
        "998877665544",
    ] {
        assert!(
            matching_ids(&index, excluded).is_empty(),
            "excluded MCP data became searchable: {excluded}"
        );
    }
}

#[test]
fn separate_calls_and_mixed_outputs_keep_their_existing_scope_and_weight() {
    let source = source("mcp-invocation-scope.jsonl");
    let scope_arguments = McpJsonCapture::Present {
        value: serde_json::json!({"scope_argument": SCOPE_CANARY}),
    };
    let mut call = record(&source, 1, "tool_call", "zzmcpscopebodyoracle");
    call.content.mcp_exchange = Some(McpExchangeContent {
        provider_call_id: "zzscopecallidone".to_owned(),
        invocation: Some(invocation(
            SCOPE_SERVER,
            SCOPE_TOOL,
            scope_arguments.clone(),
        )),
        response: None,
    });
    call.validate_contract().unwrap();
    let call_id = call.event_id;

    let mut mixed = record(&source, 2, "tool_output", "zzmcpscopebodyoracle");
    mixed.content.mcp_exchange = Some(McpExchangeContent {
        provider_call_id: "zzscopecallidtwo".to_owned(),
        invocation: Some(invocation(SCOPE_SERVER, SCOPE_TOOL, scope_arguments)),
        response: Some(McpTerminalResponseContent {
            status: McpTerminalStatus::Succeeded,
            failure_kind: None,
            duration_ns: Some(17),
            text: McpTextCapture::NormalizedBody,
            payload: McpJsonCapture::Absent,
        }),
    });
    mixed.validate_contract().unwrap();
    let mixed_id = mixed.event_id;

    let (_temp, index) = publish(&source, vec![call, mixed]);
    let all = index.search_event_candidates(SCOPE_CANARY, 20).unwrap();
    let explicit_all = index
        .search_event_candidates_with_filters(
            SCOPE_CANARY,
            &EventSearchFilters {
                content_scope: SearchContentScope::All,
                ..EventSearchFilters::default()
            },
            20,
        )
        .unwrap();
    assert_eq!(all, explicit_all);
    assert_eq!(
        matching_ids(&index, SCOPE_CANARY),
        sorted_ids([call_id, mixed_id])
    );

    let calls = index
        .search_event_candidates_with_filters(
            SCOPE_CANARY,
            &EventSearchFilters {
                content_scope: SearchContentScope::Calls,
                ..EventSearchFilters::default()
            },
            20,
        )
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].event.event_id, call_id);

    let outputs = index
        .search_event_candidates_with_filters(
            SCOPE_CANARY,
            &EventSearchFilters {
                content_scope: SearchContentScope::Outputs,
                ..EventSearchFilters::default()
            },
            20,
        )
        .unwrap();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].event.event_id, mixed_id);

    let all_call_score = all
        .iter()
        .find(|candidate| candidate.event.event_id == call_id)
        .unwrap()
        .score;
    let all_output_score = all
        .iter()
        .find(|candidate| candidate.event.event_id == mixed_id)
        .unwrap()
        .score;
    assert!((all_call_score / calls[0].score - 0.8).abs() < 0.000_01);
    assert!((all_output_score / outputs[0].score - 0.6).abs() < 0.000_01);
    assert!(all_call_score > all_output_score);
}
