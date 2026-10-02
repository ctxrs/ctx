use super::*;

#[test]
fn ambiguous_terminals_keep_output_identity_and_facts_without_join_or_embedded_invocation() {
    for mcp in [false, true] {
        let payload = if mcp {
            serde_json::json!({
                "type":"mcp_tool_call_end", "call_id":"terminal",
                "invocation":{"server":"ctx", "tool":"search", "arguments":{"query":"proof"}},
                "result":{"Ok":{"content":[{"type":"text","text":"complete output"}],"isError":false}},
                "file_path":"src/witness.rs"
            })
        } else {
            serde_json::json!({
                "type":"function_call_output", "call_id":"terminal", "status":"success",
                "output":"Process exited with code 0\nFinal output:\ncomplete output",
                "file_path":"src/witness.rs"
            })
        };
        let raw = serde_json::to_vec(&serde_json::json!({
            "type":if mcp { "event_msg" } else { "response_item" },
            "payload":payload
        }))
        .unwrap();
        let build = |unique| {
            build_source_backed_sparse_output_row(
                7,
                provider_event_identity(&payload),
                None,
                Some(CoreDiscoveryExclusion::CtxRetrievalDerived),
                unique,
                EventType::ToolOutput,
                Some("terminal"),
                DateTime::parse_from_rfc3339("2026-08-09T12:00:05Z")
                    .unwrap()
                    .with_timezone(&Utc),
                "complete output".to_owned(),
                Some(payload.clone()),
                payload.get(if mcp { "result" } else { "output" }),
                &raw,
                &payload,
                None,
            )
            .unwrap()
            .unwrap()
        };
        let unique = build(true);
        let ambiguous = build(false);
        assert!(unique.provider_event_identity.is_some());
        assert_eq!(
            ambiguous.provider_event_identity,
            unique.provider_event_identity
        );
        assert_eq!(ambiguous.lexical_body, "complete output");
        assert_eq!(ambiguous.structured_content, Some(payload));
        assert_eq!(ambiguous.discovery_exclusion, None);
        let original = unique.activity.unwrap();
        let unlinked = ambiguous.activity.unwrap();
        assert_eq!(
            original.provider_call_id,
            Some(TypedKey::Utf8("terminal".to_owned()))
        );
        assert_eq!(original.invocation.is_some(), mcp);
        assert_eq!(unlinked.provider_call_id, None);
        assert_eq!(unlinked.invocation, None);
        assert!(original.result.is_some());
        assert_eq!(unlinked.result, None);
        assert!(!unlinked.facts.is_empty());
        assert_eq!(unlinked.facts, original.facts);
    }
}
