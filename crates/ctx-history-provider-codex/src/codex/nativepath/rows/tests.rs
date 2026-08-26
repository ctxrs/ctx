use super::*;

const ITEM_COMPLETED_SESSION_ID: &str = "019fb000-0000-7000-8000-0000000000a1";

fn completed_item_record(turn_id: &str, item: Value) -> (CodexDecodedRecord, Vec<u8>) {
    let payload = serde_json::json!({
        "type": "item_completed",
        "thread_id": ITEM_COMPLETED_SESSION_ID,
        "turn_id": turn_id,
        "item": item,
    });
    let raw = serde_json::to_vec(&serde_json::json!({
        "timestamp": "2026-08-26T10:00:00Z",
        "type": "event_msg",
        "payload": payload,
    }))
    .unwrap();
    (
        CodexDecodedRecord {
            occurred_at: DateTime::parse_from_rfc3339("2026-08-26T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            payload,
        },
        raw,
    )
}

#[test]
fn item_completed_plan_preserves_outer_payload_and_qualifies_identity_by_turn() {
    let first_item = serde_json::json!({
        "id": "reused-plan",
        "type": "Plan",
        "text": "first plan",
    });
    let second_item = serde_json::json!({
        "id": "reused-plan",
        "type": "Plan",
        "text": "second plan",
    });
    let (first, first_raw) = completed_item_record("turn-one", first_item);
    let (mut second, _) = completed_item_record("turn-two", second_item);
    second.payload["completed_at_ms"] = serde_json::json!(1787738460000_i64);
    let second_raw = serde_json::to_vec(&serde_json::json!({
        "timestamp": "2026-08-26T10:00:00Z",
        "type": "event_msg",
        "payload": second.payload.clone(),
    }))
    .unwrap();
    let first = build_source_backed_event_row(
        7,
        CodexRetainedKind::ItemCompleted,
        ITEM_COMPLETED_SESSION_ID,
        &first,
        &first_raw,
    )
    .unwrap()
    .unwrap();
    let second = build_source_backed_event_row(
        8,
        CodexRetainedKind::ItemCompleted,
        ITEM_COMPLETED_SESSION_ID,
        &second,
        &second_raw,
    )
    .unwrap()
    .unwrap();

    assert_eq!(first.row.event_type, EventType::Summary);
    assert_eq!(first.row.role, Some(EventRole::Assistant));
    assert_eq!(first.row.lexical_body, "first plan");
    assert_eq!(
        first.row.structured_content,
        Some(serde_json::json!({
            "type": "item_completed",
            "thread_id": ITEM_COMPLETED_SESSION_ID,
            "turn_id": "turn-one",
            "item": {"id": "reused-plan", "type": "Plan", "text": "first plan"},
        }))
    );
    assert_eq!(
        first.row.provider_event_identity.as_ref().unwrap().kind,
        CodexProviderEventIdentityKindV0::CompletedItem
    );
    assert_ne!(
        first.row.provider_event_identity, second.row.provider_event_identity,
        "same item id in another turn must have a distinct canonical key"
    );
    assert_eq!(
        first.row.occurred_at,
        DateTime::parse_from_rfc3339("2026-08-26T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        "legacy Plan safely falls back to the envelope timestamp"
    );
    assert_eq!(
        second.row.occurred_at,
        DateTime::parse_from_rfc3339("2026-08-26T10:01:00Z")
            .unwrap()
            .with_timezone(&Utc)
    );
}

#[test]
fn item_completed_known_unknown_and_malformed_variants_have_distinct_outcomes() {
    let cases = [
        (
            serde_json::json!({"id": "user", "type": "UserMessage", "content": "raw-owned"}),
            CodexRetainedNonMaterialized::KnownNonMaterialized,
        ),
        (
            serde_json::json!({"id": "future", "type": "FutureTurnItem", "content": "future"}),
            CodexRetainedNonMaterialized::Unsupported,
        ),
        (
            serde_json::json!({"id": "lowercase", "type": "plan", "text": "not native"}),
            CodexRetainedNonMaterialized::Unsupported,
        ),
        (
            serde_json::json!({"id": "generic", "type": "ToolCall"}),
            CodexRetainedNonMaterialized::Unsupported,
        ),
        (
            serde_json::json!({"id": "bad-plan", "type": "Plan", "content": []}),
            CodexRetainedNonMaterialized::Malformed,
        ),
        (
            serde_json::json!({"id": "bad-plan-text", "type": "Plan", "text": ["not native"]}),
            CodexRetainedNonMaterialized::Malformed,
        ),
    ];
    for (item, expected) in cases {
        let (record, raw) = completed_item_record("turn-one", item);
        assert_eq!(
            build_source_backed_event_row(
                7,
                CodexRetainedKind::ItemCompleted,
                ITEM_COMPLETED_SESSION_ID,
                &record,
                &raw,
            )
            .unwrap()
            .unwrap_err(),
            expected
        );
    }

    let (record, _) = completed_item_record(
        "turn-one",
        serde_json::json!({"id": "ambiguous", "type": "AgentMessage", "text": "plan"}),
    );
    let raw = br#"{"timestamp":"2026-08-26T10:00:00Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-one","item":{"id":"ambiguous","type":"Plan","type":"AgentMessage","text":"plan"}}}"#;
    assert_eq!(
        build_source_backed_event_row(
            7,
            CodexRetainedKind::ItemCompleted,
            ITEM_COMPLETED_SESSION_ID,
            &record,
            raw,
        )
        .unwrap()
        .unwrap_err(),
        CodexRetainedNonMaterialized::Malformed
    );
}

#[test]
fn item_completed_distinguishes_raw_overlaps_from_lifecycle_only_variants() {
    // These minimal items exercise the closed discriminator table. The narrow
    // Plan projector deliberately leaves their variant-specific fields alone;
    // response_item remains authoritative where it is available.
    for item_type in [
        "UserMessage",
        "HookPrompt",
        "AgentMessage",
        "Reasoning",
        "CommandExecution",
        "DynamicToolCall",
        "CollabAgentToolCall",
        "WebSearch",
        "ImageView",
        "ImageGeneration",
        "FileChange",
        "McpToolCall",
        "ContextCompaction",
    ] {
        let (record, raw) = completed_item_record(
            "turn-one",
            serde_json::json!({"id": "known", "type": item_type}),
        );
        assert_eq!(
            build_source_backed_event_row(
                7,
                CodexRetainedKind::ItemCompleted,
                ITEM_COMPLETED_SESSION_ID,
                &record,
                &raw,
            )
            .unwrap()
            .unwrap_err(),
            CodexRetainedNonMaterialized::KnownNonMaterialized,
            "{item_type} is a current TurnItem variant"
        );
    }

    for item_type in ["SubAgentActivity", "EnteredReviewMode", "ExitedReviewMode"] {
        let (record, raw) = completed_item_record(
            "turn-one",
            serde_json::json!({"id": "known-lifecycle-only", "type": item_type}),
        );
        assert_eq!(
            build_source_backed_event_row(
                7,
                CodexRetainedKind::ItemCompleted,
                ITEM_COMPLETED_SESSION_ID,
                &record,
                &raw,
            )
            .unwrap()
            .unwrap_err(),
            CodexRetainedNonMaterialized::Unsupported,
            "{item_type} has no duplicate raw response record"
        );
    }

    for kind in ["image_gen.generation", "clock.sleep", "web.search"] {
        let (record, raw) = completed_item_record(
            "turn-one",
            serde_json::json!({"id": "known", "type": "Extension", "kind": kind}),
        );
        assert_eq!(
            build_source_backed_event_row(
                7,
                CodexRetainedKind::ItemCompleted,
                ITEM_COMPLETED_SESSION_ID,
                &record,
                &raw,
            )
            .unwrap()
            .unwrap_err(),
            CodexRetainedNonMaterialized::Unsupported,
            "{kind} is lifecycle-only and must remain observably unsupported"
        );
    }

    let (record, raw) = completed_item_record(
        "turn-one",
        serde_json::json!({
            "id": "opaque-mcp",
            "type": "McpToolCall",
            "arguments": {"input": {"left": 1}, "args": {"right": 2}}
        }),
    );
    assert_eq!(
        build_source_backed_event_row(
            7,
            CodexRetainedKind::ItemCompleted,
            ITEM_COMPLETED_SESSION_ID,
            &record,
            &raw,
        )
        .unwrap()
        .unwrap_err(),
        CodexRetainedNonMaterialized::KnownNonMaterialized,
        "opaque tool arguments must not participate in item selector auditing"
    );

    let (record, raw) = completed_item_record(
        "turn-one",
        serde_json::json!({"id": "future", "type": "Extension", "kind": "future.item"}),
    );
    assert_eq!(
        build_source_backed_event_row(
            7,
            CodexRetainedKind::ItemCompleted,
            ITEM_COMPLETED_SESSION_ID,
            &record,
            &raw,
        )
        .unwrap()
        .unwrap_err(),
        CodexRetainedNonMaterialized::Unsupported
    );
}

#[test]
fn item_completed_rejects_missing_or_mismatched_thread_and_malformed_timestamps() {
    for mutation in ["missing-thread", "mismatched-thread", "string-timestamp"] {
        let (mut record, raw) = completed_item_record(
            "turn-one",
            serde_json::json!({"id": "plan", "type": "Plan", "text": "plan"}),
        );
        match mutation {
            "missing-thread" => {
                record.payload.as_object_mut().unwrap().remove("thread_id");
            }
            "mismatched-thread" => {
                record.payload["thread_id"] = serde_json::json!("another-session");
            }
            "string-timestamp" => {
                record.payload["completed_at_ms"] = serde_json::json!("not-a-number");
            }
            _ => unreachable!(),
        }
        assert_eq!(
            build_source_backed_event_row(
                7,
                CodexRetainedKind::ItemCompleted,
                ITEM_COMPLETED_SESSION_ID,
                &record,
                &raw,
            )
            .unwrap()
            .unwrap_err(),
            CodexRetainedNonMaterialized::Malformed,
            "{mutation} must not materialize"
        );
    }
}

#[test]
fn item_completed_rejects_duplicate_keys_for_every_consumed_selector() {
    let (record, _) = completed_item_record(
        "turn-one",
        serde_json::json!({"id": "plan", "type": "Plan", "text": "plan"}),
    );
    let raws = [
        (
            "payload",
            r#"{"type":"event_msg","payload":{},"payload":{"type":"item_completed"}}"#,
        ),
        (
            "timestamp",
            r#"{"timestamp":"2026-08-26T10:00:00Z","timestamp":"2026-08-26T10:00:01Z","type":"event_msg","payload":{"type":"item_completed"}}"#,
        ),
        (
            "thread_id",
            r#"{"type":"event_msg","payload":{"type":"item_completed","thread_id":"first","thread_id":"second"}}"#,
        ),
        (
            "turn_id",
            r#"{"type":"event_msg","payload":{"type":"item_completed","turn_id":"first","turn_id":"second"}}"#,
        ),
        (
            "item",
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{},"item":{"id":"plan","type":"Plan","text":"plan"}}}"#,
        ),
        (
            "item id",
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"id":"first","id":"second","type":"Plan","text":"plan"}}}"#,
        ),
        (
            "Plan text",
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"id":"plan","type":"Plan","text":"first","text":"second"}}}"#,
        ),
        (
            "started_at_ms",
            r#"{"type":"event_msg","payload":{"type":"item_completed","started_at_ms":1,"started_at_ms":2}}"#,
        ),
        (
            "completed_at_ms",
            r#"{"type":"event_msg","payload":{"type":"item_completed","completed_at_ms":1,"completed_at_ms":2}}"#,
        ),
    ];
    for (selector, raw) in raws {
        assert_eq!(
            build_source_backed_event_row(
                7,
                CodexRetainedKind::ItemCompleted,
                ITEM_COMPLETED_SESSION_ID,
                &record,
                raw.as_bytes(),
            )
            .unwrap()
            .unwrap_err(),
            CodexRetainedNonMaterialized::Malformed,
            "duplicate {selector} must not reach durable identity or content"
        );
    }
}

#[test]
fn mcp_terminal_activity_preserves_exact_server_tool_and_linkage() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let payload = serde_json::json!({
        "type": "mcp_tool_call_end",
        "call_id": "call-terminal",
        "invocation": {
            "server": "source-server",
            "tool": "source-tool",
            "arguments": {"path": "A/../B", "items": [1, 1]}
        },
        "status": "provider::failed",
        "result": {"error": "native failure"}
    });
    let raw = serde_json::to_vec(&payload).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_result_activity(
        payload.get("call_id").and_then(Value::as_str),
        payload.get("result"),
        &payload,
        &audit,
        occurred_at,
    )
    .unwrap();

    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::utf8("call-terminal").unwrap())
    );
    let invocation = activity.invocation.unwrap();
    assert_eq!(invocation.protocol.as_deref(), Some("mcp"));
    assert_eq!(invocation.server.as_deref(), Some("source-server"));
    assert_eq!(invocation.tool, "source-tool");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: serde_json::json!({"path": "A/../B", "items": [1, 1]})
        }
    );
    let result = activity.result.unwrap();
    assert_eq!(result.status.as_deref(), Some("provider::failed"));
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present {
            value: serde_json::json!({"error": "native failure"})
        }
    );

    let exact = serde_json::json!({
        "timestamp": "2026-08-16T12:00:00Z",
        "type": "event_msg",
        "payload": {
            "type": "mcp_tool_call_end",
            "call_id": "call-ctx-search",
            "invocation": {
                "server": "ctx",
                "tool": "search",
                "arguments": {"query": "needle"}
            },
            "duration": {"secs": 0, "nanos": 42},
            "result": {
                "Ok": {
                    "content": [{"type": "text", "text": "{\"results\":[]}"}],
                    "isError": false
                }
            }
        }
    });
    let raw = serde_json::to_vec(&exact).unwrap();
    let payload = exact.get("payload").unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_result_activity(
        payload.get("call_id").and_then(Value::as_str),
        payload.get("result"),
        payload,
        &audit,
        occurred_at,
    )
    .unwrap();
    assert_eq!(
        codex_result_discovery_exclusion(&raw, None, true, Some(&activity)),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );

    for mutation in ["ordinary", "error", "diagnostic"] {
        let mut control = exact.clone();
        match mutation {
            "ordinary" => {
                control["payload"]["invocation"]["server"] = Value::String("filesystem".to_owned())
            }
            "error" => control["payload"]["result"]["Ok"]["isError"] = Value::Bool(true),
            "diagnostic" => {
                control["payload"]["result"]["Ok"]["warning"] =
                    Value::String("provider warning".to_owned())
            }
            _ => unreachable!(),
        }
        let raw = serde_json::to_vec(&control).unwrap();
        let payload = control.get("payload").unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            payload.get("call_id").and_then(Value::as_str),
            payload.get("result"),
            payload,
            &audit,
            occurred_at,
        )
        .unwrap();
        assert_eq!(
            codex_result_discovery_exclusion(&raw, None, true, Some(&activity)),
            None,
            "unexpected exclusion for {mutation} MCP terminal"
        );
    }
}

#[test]
fn mcp_retrieval_exclusion_requires_nonempty_text_payload_and_valid_duration() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exact = serde_json::json!({
        "timestamp": "2026-08-16T12:00:00Z",
        "type": "event_msg",
        "payload": {
            "type": "mcp_tool_call_end",
            "call_id": "call-ctx-search-payload",
            "invocation": {
                "server": "ctx",
                "tool": "search",
                "arguments": {"query": "needle"}
            },
            "duration": {"secs": 0, "nanos": 42},
            "result": {
                "Ok": {
                    "content": [{"type": "text", "text": "{\"results\":[]}"}],
                    "isError": false
                }
            }
        }
    });
    let classify = |record: &Value| {
        let raw = serde_json::to_vec(record).unwrap();
        let payload = record.get("payload").unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            payload.get("call_id").and_then(Value::as_str),
            payload.get("result"),
            payload,
            &audit,
            occurred_at,
        )
        .unwrap();
        codex_result_discovery_exclusion(&raw, None, true, Some(&activity))
    };

    assert_eq!(
        classify(&exact),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    let mut no_duration = exact.clone();
    no_duration["payload"]
        .as_object_mut()
        .unwrap()
        .remove("duration");
    assert_eq!(
        classify(&no_duration),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    let mut structured = exact.clone();
    structured["payload"]["result"]["Ok"]["structuredContent"] = serde_json::json!({"results": []});
    assert_eq!(
        classify(&structured),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );

    for (mutation, invalid) in [
        ("empty-content", serde_json::json!([])),
        (
            "empty-text",
            serde_json::json!([{"type": "text", "text": ""}]),
        ),
        (
            "non-text",
            serde_json::json!([{"type": "image", "data": "AA=="}]),
        ),
        (
            "mixed-content",
            serde_json::json!([
                {"type": "text", "text": "payload"},
                {"type": "image", "data": "AA=="}
            ]),
        ),
    ] {
        let mut control = exact.clone();
        control["payload"]["result"]["Ok"]["content"] = invalid;
        assert_eq!(
            classify(&control),
            None,
            "unexpected exclusion for {mutation}"
        );
    }

    let mut structured_only = exact.clone();
    structured_only["payload"]["result"]["Ok"]
        .as_object_mut()
        .unwrap()
        .remove("content");
    structured_only["payload"]["result"]["Ok"]["structuredContent"] =
        serde_json::json!({"results": []});
    assert_eq!(classify(&structured_only), None);

    for (mutation, invalid) in [
        ("null-duration", Value::Null),
        ("non-object-duration", Value::String("fast".to_owned())),
        ("missing-secs", serde_json::json!({"nanos": 42})),
        (
            "unknown-duration-field",
            serde_json::json!({"secs": 0, "nanos": 42, "warning": true}),
        ),
        (
            "out-of-range-nanos",
            serde_json::json!({"secs": 0, "nanos": 1_000_000_000_u64}),
        ),
    ] {
        let mut control = exact.clone();
        control["payload"]["duration"] = invalid;
        assert_eq!(
            classify(&control),
            None,
            "unexpected exclusion for {mutation}"
        );
    }

    let mut metadata = exact;
    metadata["payload"]["result"]["Ok"]["_meta"] =
        serde_json::json!({"warning": "mixed diagnostic"});
    assert_eq!(classify(&metadata), None);
}

#[test]
fn direct_ctx_retrieval_invocation_excludes_without_losing_activity() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for (command, expected) in [
        (
            "ctx search exact-retrieval",
            Some(CoreDiscoveryExclusion::CtxRetrievalDerived),
        ),
        ("ctx status", None),
        ("git status", None),
    ] {
        let payload = serde_json::json!({
            "type": "function_call",
            "call_id": format!("call-{command}"),
            "name": "exec_command",
            "arguments": {"cmd": command}
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_invocation_activity(&payload, &audit, occurred_at).unwrap();

        assert_eq!(
            codex_invocation_discovery_exclusion(&payload, &audit, Some(&activity)),
            expected,
            "unexpected classification for {command}"
        );
        assert_eq!(
            activity.invocation.as_ref().unwrap().arguments,
            ActivityJsonCapture::Present {
                value: serde_json::json!({"cmd": command})
            }
        );
    }
}

#[test]
fn linked_ctx_retrieval_result_requires_exact_success_envelope() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let output = concat!(
        "Script completed\n",
        "Process exited with code 0\n",
        "Final output:\n",
        "{\"results\":[]}"
    );
    let exact = serde_json::json!({
        "timestamp": "2026-08-16T12:00:00Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": "call-linked",
            "status": "success",
            "output": output
        }
    });
    let classify =
        |record: &Value, linked_invocation_discovery_exclusion: Option<CoreDiscoveryExclusion>| {
            let raw = serde_json::to_vec(record).unwrap();
            let payload = record.get("payload").unwrap();
            let audit = audit_codex_record(&raw).unwrap();
            let activity = codex_result_activity(
                payload.get("call_id").and_then(Value::as_str),
                payload.get("output"),
                payload,
                &audit,
                occurred_at,
            )
            .unwrap();
            codex_result_discovery_exclusion(
                &raw,
                linked_invocation_discovery_exclusion,
                true,
                Some(&activity),
            )
        };

    assert_eq!(
        classify(&exact, Some(CoreDiscoveryExclusion::CtxRetrievalDerived)),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    assert_eq!(classify(&exact, None), None);
    let mut legacy = exact.clone();
    legacy["payload"].as_object_mut().unwrap().remove("status");
    legacy["payload"]["output"] = Value::String(
        concat!(
            "Chunk ID: abc123\n",
            "Wall time: 0.1 seconds\n",
            "Process exited with code 0\n",
            "Final output:\n",
            "{\"results\":[]}"
        )
        .to_owned(),
    );
    assert_eq!(
        classify(&legacy, Some(CoreDiscoveryExclusion::CtxRetrievalDerived)),
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    let mut failed = exact.clone();
    failed["payload"]["status"] = Value::String("failed".to_owned());
    assert_eq!(
        classify(&failed, Some(CoreDiscoveryExclusion::CtxRetrievalDerived)),
        None
    );
    let mut diagnostic = exact;
    diagnostic["payload"]["stderr"] = Value::String("diagnostic".to_owned());
    assert_eq!(
        classify(
            &diagnostic,
            Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
        ),
        None
    );
}

#[test]
fn activity_preserves_exact_mcp_invocation_and_result_channels() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let invocation = serde_json::json!({
        "type": "function_call",
        "call_id": "call-exact",
        "name": "mcp__forge__open",
        "arguments": {
            "command": "  git status  ",
            "path": "./exact",
            "url": "https://example.invalid/p?q=a%20b"
        }
    });
    let raw = serde_json::to_vec(&invocation).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_invocation_activity(&invocation, &audit, occurred_at).unwrap();
    assert_eq!(
        codex_invocation_discovery_exclusion(&invocation, &audit, Some(&activity)),
        None
    );
    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::utf8("call-exact").unwrap())
    );
    let invocation = activity.invocation.unwrap();
    assert_eq!(invocation.protocol, None);
    assert_eq!(invocation.server, None);
    assert_eq!(invocation.tool, "mcp__forge__open");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: serde_json::json!({
                "command": "  git status  ",
                "path": "./exact",
                "url": "https://example.invalid/p?q=a%20b"
            })
        }
    );

    let provider_result = serde_json::json!({
        "content": ["first", {"status": "failed"}],
        "command": "  git status  "
    });
    let payload = serde_json::json!({"status":"native", "result":provider_result});
    let raw = serde_json::to_vec(&payload).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_result_activity(
        Some("call-exact"),
        payload.get("result"),
        &payload,
        &audit,
        occurred_at,
    )
    .unwrap();
    let result = activity.result.unwrap();
    assert_eq!(result.status.as_deref(), Some("native"));
    assert_eq!(result.text, ActivityTextCapture::Absent);
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present {
            value: payload.get("result").unwrap().clone()
        }
    );
}

#[test]
fn empty_result_string_is_absent_text_with_exact_structured_capture() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let payload = serde_json::json!({
        "type": "function_call_output",
        "call_id": "call-empty",
        "output": ""
    });
    let raw = serde_json::to_vec(&payload).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_result_activity(
        Some("call-empty"),
        payload.get("output"),
        &payload,
        &audit,
        occurred_at,
    )
    .unwrap();

    let result = activity.result.unwrap();
    assert_eq!(result.text, ActivityTextCapture::Absent);
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present {
            value: Value::String(String::new())
        }
    );
}

#[test]
fn terminal_outcomes_preserve_literal_status_and_complete_result_content() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for (status, expected) in [
        (Some("provider::ok"), Some("provider::ok")),
        (Some("provider::failed"), Some("provider::failed")),
        (None, None),
    ] {
        let mut payload = serde_json::json!({
            "type": "function_call_output",
            "call_id": "call-outcome",
            "output": {"message": "complete provider result"}
        });
        if let Some(status) = status {
            payload["status"] = Value::String(status.to_owned());
        }
        let raw = serde_json::to_vec(&payload).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            payload.get("call_id").and_then(Value::as_str),
            payload.get("output"),
            &payload,
            &audit,
            occurred_at,
        )
        .unwrap();
        let result = activity.result.unwrap();
        assert_eq!(result.status.as_deref(), expected);
        assert_eq!(result.text, ActivityTextCapture::Absent);
        assert_eq!(
            result.structured_content,
            ActivityJsonCapture::Present {
                value: serde_json::json!({"message": "complete provider result"})
            }
        );
    }
}

#[test]
fn malformed_mcp_identity_abstains_without_losing_valid_result_activity() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    for invocation in [
        serde_json::json!({"server": 7, "tool": "read", "arguments": {}}),
        serde_json::json!({"server": "server", "tool": "", "arguments": {}}),
        serde_json::json!({
            "server": "s".repeat(MAX_CODEX_DURABLE_METADATA_BYTES + 1),
            "tool": "read",
            "arguments": {}
        }),
    ] {
        let payload = serde_json::json!({
            "type": "mcp_tool_call_end",
            "call_id": "call-malformed-identity",
            "invocation": invocation,
            "result": "valid result survives"
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            payload.get("call_id").and_then(Value::as_str),
            payload.get("result"),
            &payload,
            &audit,
            occurred_at,
        )
        .unwrap();
        assert!(activity.invocation.is_none());
        assert_eq!(
            activity.result.unwrap().text,
            ActivityTextCapture::Present {
                value: "valid result survives".to_owned()
            }
        );
    }

    let invalid_call = serde_json::json!({
        "type": "function_call_output",
        "call_id": ["not", "a", "string"],
        "output": "unlinked result"
    });
    let raw = serde_json::to_vec(&invalid_call).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    assert!(codex_result_activity(
        invalid_call.get("call_id").and_then(Value::as_str),
        invalid_call.get("output"),
        &invalid_call,
        &audit,
        occurred_at,
    )
    .is_none());
}

#[test]
fn exact_mcp_identity_boundary_is_accepted_and_max_plus_one_abstains() {
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exact_server = "s".repeat(MAX_CODEX_DURABLE_METADATA_BYTES);
    let exact_tool = "t".repeat(MAX_CODEX_DURABLE_METADATA_BYTES);
    let payload = serde_json::json!({
        "type": "mcp_tool_call_end",
        "call_id": "call-boundary",
        "invocation": {
            "server": exact_server,
            "tool": exact_tool,
            "arguments": {}
        },
        "result": "boundary result"
    });
    let raw = serde_json::to_vec(&payload).unwrap();
    let audit = audit_codex_record(&raw).unwrap();
    let activity = codex_result_activity(
        payload.get("call_id").and_then(Value::as_str),
        payload.get("result"),
        &payload,
        &audit,
        occurred_at,
    )
    .unwrap();
    let invocation = activity.invocation.unwrap();
    assert_eq!(invocation.server.as_deref(), Some(exact_server.as_str()));
    assert_eq!(invocation.tool, exact_tool);

    for component in ["server", "tool"] {
        let mut oversized = payload.clone();
        oversized["invocation"][component] =
            Value::String("x".repeat(MAX_CODEX_DURABLE_METADATA_BYTES + 1));
        let raw = serde_json::to_vec(&oversized).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let activity = codex_result_activity(
            oversized.get("call_id").and_then(Value::as_str),
            oversized.get("result"),
            &oversized,
            &audit,
            occurred_at,
        )
        .unwrap();
        assert!(activity.invocation.is_none(), "oversized {component}");
        assert!(activity.result.is_some());
    }
}

#[test]
fn duplicate_selectors_withhold_linkage_and_preserve_raw_fact_order() {
    let raw = br#"{"type":"function_call","call_id":"one","call_id":"two","name":"tool","arguments":{"command":" c ","path":" p ","url":" u "}}"#;
    let payload: Value = serde_json::from_slice(raw).unwrap();
    let audit = audit_codex_record(raw).unwrap();
    let occurred_at = DateTime::parse_from_rfc3339("2026-08-16T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let activity = codex_invocation_activity(&payload, &audit, occurred_at).unwrap();
    assert!(activity.provider_call_id.is_none());
    assert!(activity.invocation.is_none());
    assert_eq!(
        activity
            .facts
            .iter()
            .map(|fact| (fact.kind, fact.value.as_str()))
            .collect::<Vec<_>>(),
        [
            (LiteralFactKind::Command, " c "),
            (LiteralFactKind::File, " p "),
            (LiteralFactKind::Url, " u "),
        ]
    );
}
