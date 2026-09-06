//! Supplemental neutral selector cases; strings here are synthetic.
use super::*;

fn invocation(raw: &[u8]) -> CodexCoreRecordDraft {
    let payload: Value = serde_json::from_slice(raw).unwrap();
    let record = CodexDecodedRecord {
        occurred_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
        payload,
    };
    build_source_backed_event_row(0, CodexRetainedKind::ToolCall, "session", &record, raw)
        .unwrap()
        .unwrap()
        .row
}

fn result(raw: &[u8]) -> CodexCoreRecordDraft {
    let payload: Value = serde_json::from_slice(raw).unwrap();
    build_source_backed_sparse_output_row(
        1,
        provider_event_identity(&payload),
        None,
        None,
        true,
        EventType::ToolOutput,
        payload.get("call_id").and_then(Value::as_str),
        DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        payload["output"].as_str().unwrap().to_owned(),
        Some(payload.clone()),
        payload.get("output"),
        raw,
        &payload,
        None,
    )
    .unwrap()
    .unwrap()
}

#[test]
fn distinct_item_ids_preserve_common_call_and_complete_payloads() {
    let call = br#"{"type":"function_call","id":"item-call","call_id":"shared-call","name":"exec_command","arguments":"{\"cmd\":\"echo sample\"}"}"#;
    let output = br#"{"type":"function_call_output","id":"item-output","call_id":"shared-call","output":"  complete result\nlast line\n"}"#;
    for (raw, row, item_id, is_call) in [
        (call.as_slice(), invocation(call), "item-call", true),
        (output.as_slice(), result(output), "item-output", false),
    ] {
        let identity = row.provider_event_identity.unwrap();
        assert_eq!(identity.kind, CodexProviderEventIdentityKindV0::Id);
        assert_eq!(identity.value, item_id);
        let payload: Value = serde_json::from_slice(raw).unwrap();
        assert_eq!(row.structured_content, Some(payload.clone()));
        let activity = row.activity.unwrap();
        assert_eq!(
            activity.provider_call_id,
            Some(TypedKey::utf8("shared-call").unwrap())
        );
        assert_eq!(activity.invocation.is_some(), is_call);
        assert_eq!(activity.result.is_some(), !is_call);
        if let Some(result) = activity.result {
            assert_eq!(row.lexical_body, "  complete result\nlast line\n");
            assert_eq!(
                result.text,
                ActivityTextCapture::Present {
                    value: row.lexical_body
                }
            );
            assert_eq!(
                result.structured_content,
                ActivityJsonCapture::Present {
                    value: payload["output"].clone()
                }
            );
        }
    }
}

#[test]
fn id_only_invocation_fallback_and_call_id_precedence_remain_bounded() {
    for length in [
        MAX_CODEX_DURABLE_METADATA_BYTES,
        MAX_CODEX_DURABLE_METADATA_BYTES + 1,
    ] {
        let id = "i".repeat(length);
        let payload =
            serde_json::json!({"type":"function_call", "id":id, "name":"tool", "arguments":{}});
        let raw = serde_json::to_vec(&payload).unwrap();
        let row = invocation(&raw);
        assert_eq!(row.provider_event_identity.unwrap().value, id);
        assert_eq!(
            row.activity.is_some(),
            length == MAX_CODEX_DURABLE_METADATA_BYTES
        );
        if let Some(activity) = row.activity {
            assert_eq!(
                activity.provider_call_id,
                Some(TypedKey::utf8(&id).unwrap())
            );
        }
    }
    for call_id in [
        Value::Null,
        serde_json::json!(7),
        serde_json::json!(""),
        serde_json::json!("c".repeat(MAX_CODEX_DURABLE_METADATA_BYTES + 1)),
    ] {
        let raw = serde_json::to_vec(&serde_json::json!({"type":"function_call", "id":"fallback", "call_id":call_id, "name":"tool"})).unwrap();
        assert!(
            invocation(&raw).activity.is_none(),
            "present invalid call_id cannot select fallback id"
        );
    }
}

#[test]
fn literal_duplicate_ids_and_call_aliases_still_withhold_linkage() {
    for selectors in [
        r#""id":"one","id":"one""#,
        r#""id":"one","id":"two""#,
        r#""id":"one","\u0069d":"two""#,
        r#""call_id":"one","call_id":"one""#,
        r#""call_id":"one","call_id":"two""#,
        r#""call_id":"one","callId":"two""#,
        r#""call_id":"one","callId":"one""#,
        r#""id":"one","id":"two","call_id":"valid""#,
        r#""callId":"one","callId":"two""#,
        r#""id":"one","callId":"two""#,
        r#""id":"one","callId":"one""#,
        r#""callId":"one","id":"two""#,
        r#""callId":"one","id":"one""#,
        r#""callId":"one","call_id":"two""#,
    ] {
        let call = format!(
            r#"{{"type":"function_call",{selectors},"name":"tool","arguments":{{"command":"literal"}}}}"#
        );
        let output =
            format!(r#"{{"type":"function_call_output",{selectors},"output":"retained result"}}"#);
        for row in [invocation(call.as_bytes()), result(output.as_bytes())] {
            assert!(row.provider_event_identity.is_none(), "{selectors}");
            assert!(row.structured_content.is_none(), "{selectors}");
            assert!(
                row.activity
                    .is_none_or(|activity| activity.provider_call_id.is_none()
                        && activity.invocation.is_none()
                        && activity.result.is_none()),
                "{selectors}"
            );
        }
        assert_eq!(result(output.as_bytes()).lexical_body, "retained result");
    }
}
