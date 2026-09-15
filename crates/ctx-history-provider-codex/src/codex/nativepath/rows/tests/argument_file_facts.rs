use super::*;

fn assert_literal_argument_encoding_parity(key: &str) {
    // Synthetic diagnostic: only the provider argument envelope changes.
    // It does not infer an operation or a file effect from text.
    let literal = serde_json::json!({key: "/workspace/example.rs"});
    for arguments in [literal.clone(), Value::String(literal.to_string())] {
        let payload = serde_json::json!({
            "type": "function_call",
            "name": "example",
            "call_id": "literal-argument",
            "arguments": arguments,
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let audit = audit_codex_record(&raw).unwrap();
        let occurred_at = DateTime::parse_from_rfc3339("2026-09-10T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let activity = codex_invocation_activity(&payload, &audit, occurred_at).unwrap();
        assert_eq!(
            activity.invocation.unwrap().arguments,
            ActivityJsonCapture::Present {
                value: arguments.clone()
            },
        );
        assert_eq!(
            activity.facts,
            vec![ctx_history_core::ProviderDeclaredFact {
                kind: LiteralFactKind::File,
                value: "/workspace/example.rs".to_owned(),
            }],
            "literal {key} is missing for arguments={arguments}",
        );
    }
}

#[test]
fn serialized_path_retains_literal_file_fact() {
    assert_literal_argument_encoding_parity("path");
}

#[test]
fn serialized_file_path_retains_literal_file_fact() {
    assert_literal_argument_encoding_parity("file_path");
}

fn argument_row(arguments: &str) -> CodexCoreRecordDraft {
    let payload = serde_json::json!({
        "type": "function_call", "id": "item", "call_id": "call",
        "name": "example", "arguments": arguments,
    });
    let raw = serde_json::to_vec(&serde_json::json!({
        "type": "response_item", "payload": payload,
    }))
    .unwrap();
    let retained = CodexDecodedRecord {
        occurred_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
        payload: payload.clone(),
    };
    let row =
        build_source_backed_event_row(0, CodexRetainedKind::ToolCall, "session", &retained, &raw)
            .unwrap()
            .unwrap()
            .row;
    assert_eq!(row.structured_content, Some(payload.clone()));
    assert_eq!(row.lexical_body, payload.to_string());
    assert_eq!(row.provider_event_identity.as_ref().unwrap().value, "item");
    let activity = row.activity.as_ref().unwrap();
    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::utf8("call").unwrap())
    );
    assert_eq!(activity.invocation.as_ref().unwrap().tool, "example");
    assert_eq!(
        activity.invocation.as_ref().unwrap().arguments,
        ActivityJsonCapture::Present {
            value: Value::String(arguments.to_owned())
        },
    );
    row
}

fn literal_values(audit: &RawJsonAudit) -> Vec<(LiteralFactKind, &str)> {
    audit
        .facts()
        .iter()
        .map(|fact| (fact.kind, fact.value.as_str()))
        .collect()
}

#[test]
fn serialized_arrays_nesting_and_exact_path_bytes_follow_literal_order() {
    let arguments = r#"[
        {"paths":["./relative.rs","../parent.rs","/absolute.rs","./relative.rs"]},
        {"nested":{"file_path":"dir/quote\"slash\\tab\tline\n.rs"}},
        {"path":"\u96ea/\ud83d\ude80.rs"},
        {"path":" e\u0301/é.rs "},
        {"file":"one","filepath":"two","old_path":"old","new_path":"new"},
        {"path":[null,7,false,"",["nested-array.rs"],{"file_path":"inside.rs"}]}
    ]"#;
    let row = argument_row(arguments);
    let facts = &row.activity.unwrap().facts;
    assert!(facts.iter().all(|fact| fact.kind == LiteralFactKind::File));
    assert_eq!(
        facts
            .iter()
            .map(|fact| fact.value.as_str())
            .collect::<Vec<_>>(),
        [
            "./relative.rs",
            "../parent.rs",
            "/absolute.rs",
            "./relative.rs",
            "dir/quote\"slash\\tab\tline\n.rs",
            "雪/🚀.rs",
            " e\u{301}/é.rs ",
            "one",
            "two",
            "old",
            "new",
            "nested-array.rs",
            "inside.rs",
        ]
    );
}

#[test]
fn decoded_facts_stay_at_the_argument_field_position() {
    let argument_json = r#"{"command":" c ","file_path":" p ","url":" u "}"#;
    for arguments in [
        argument_json.to_owned(),
        serde_json::to_string(argument_json).unwrap(),
    ] {
        // Discriminators may follow arguments; JSON map sorting is not the contract.
        let payload = format!(
            r#"{{"path":"before","arguments":{arguments},"branch":"after","type":"function_call"}}"#
        );
        for raw in [
            payload.clone(),
            format!(r#"{{"payload":{payload},"type":"response_item"}}"#),
        ] {
            let audit = audit_codex_record(raw.as_bytes()).unwrap();
            assert_eq!(
                literal_values(&audit),
                [
                    (LiteralFactKind::File, "before"),
                    (LiteralFactKind::Command, " c "),
                    (LiteralFactKind::File, " p "),
                    (LiteralFactKind::Url, " u "),
                    (LiteralFactKind::Branch, "after"),
                ]
            );
        }
    }
}

#[test]
fn duplicate_literal_keys_discard_facts_without_losing_arguments() {
    for arguments in [
        r#"{"path":"one","path":"one"}"#,
        r#"{"path":"one","path":"two"}"#,
        r#"{"path":"one","\u0070ath":"two"}"#,
        r#"{"path":null,"path":"two"}"#,
        r#"{"path":"before","nested":{"file_path":"one","file_path":"two"}}"#,
        r#"{"cmd":"one","cmd":"two","path":"file"}"#,
    ] {
        assert!(
            argument_row(arguments).activity.unwrap().facts.is_empty(),
            "{arguments}"
        );
        for encoded in [
            arguments.to_owned(),
            serde_json::to_string(arguments).unwrap(),
        ] {
            let raw =
                format!(r#"{{"path":"outside","type":"function_call","arguments":{encoded}}}"#);
            assert!(audit_codex_record(raw.as_bytes())
                .unwrap()
                .facts()
                .is_empty());
        }
    }
}

#[test]
fn decoded_selector_names_do_not_change_native_identity_or_redecode_strings() {
    let arguments = r#"{
        "type":"function_call","type":"other","id":"one","id":"two",
        "call_id":"one","callId":"two","name":"one","name":"two",
        "input":"{\"path\":\"input.rs\"}","args":"{\"path\":\"args.rs\"}",
        "arguments":"{\"path\":\"arguments.rs\"}",
        "payload":{"type":"function_call","arguments":"{\"path\":\"payload.rs\"}"},
        "path":"literal.rs"
    }"#;
    let activity = argument_row(arguments).activity.unwrap();
    assert_eq!(activity.facts.len(), 1);
    assert_eq!(activity.facts[0].value, "literal.rs");
}

#[test]
fn ambiguous_native_argument_envelopes_are_not_decoded() {
    let arguments = serde_json::to_string(r#"{"path":"inside.rs"}"#).unwrap();
    for fields in [
        format!(r#""type":"function_call","arguments":{arguments},"arguments":{arguments}"#),
        format!(r#""type":"function_call","arguments":null,"arguments":{arguments}"#),
        format!(r#""type":"function_call","arguments":{arguments},"\u0061rguments":{arguments}"#),
        format!(r#""type":"function_call","arguments":{arguments},"input":null"#),
        format!(r#""type":"function_call","args":null,"arguments":{arguments}"#),
        format!(r#""type":"other","type":"function_call","arguments":{arguments}"#),
        format!(r#""type":"function_call","type":"function_call","arguments":{arguments}"#),
        format!(r#""type":null,"type":"function_call","arguments":{arguments}"#),
    ] {
        let payload = format!(r#"{{"path":"outside.rs",{fields}}}"#);
        for raw in [
            payload.clone(),
            format!(r#"{{"type":"response_item","payload":{payload}}}"#),
        ] {
            let audit = audit_codex_record(raw.as_bytes()).unwrap();
            assert_eq!(
                literal_values(&audit),
                [(LiteralFactKind::File, "outside.rs")],
                "{fields}"
            );
            assert!(audit.any_selector_ambiguous());
        }
    }
    let payload = format!(r#"{{"type":"function_call","arguments":{arguments}}}"#);
    for raw in [
        format!(r#"{{"type":"response_item","payload":{payload},"payload":{payload}}}"#),
        format!(r#"{{"type":"response_item","payload":null,"payload":{payload}}}"#),
        format!(r#"{{"type":"other","type":"response_item","payload":{payload}}}"#),
    ] {
        assert!(audit_codex_record(raw.as_bytes())
            .unwrap()
            .facts()
            .is_empty());
    }
}

#[test]
fn escaped_native_keys_are_selected_without_normalizing_path_values() {
    let raw = r#"{"\u0070ayload":{"\u0061rguments":"{\"\u0070ath\":\"./雪.rs\"}","\u0074ype":"function_call"},"type":"response_item"}"#;
    let audit = audit_codex_record(raw.as_bytes()).unwrap();
    assert_eq!(literal_values(&audit), [(LiteralFactKind::File, "./雪.rs")]);
}

#[test]
fn malformed_or_noncontainer_argument_json_adds_no_partial_facts() {
    for arguments in [
        "",
        "not json",
        "null",
        "true",
        "7",
        r#""a string""#,
        r#""{\"path\":\"double-encoded.rs\"}""#,
        r#"{"path":"partial.rs","later":}"#,
        r#"[{"path":"partial.rs"},"unterminated]"#,
        r#"{"path":"partial.rs"} trailing"#,
        r#"{"path":"partial.rs"}{"path":"second.rs"}"#,
        r#"{"path":"\ud800"}"#,
    ] {
        assert!(
            argument_row(arguments).activity.unwrap().facts.is_empty(),
            "{arguments}"
        );
        let encoded = serde_json::to_string(arguments).unwrap();
        let raw =
            format!(r#"{{"path":"outside.rs","type":"function_call","arguments":{encoded}}}"#);
        let audit = audit_codex_record(raw.as_bytes()).unwrap();
        assert_eq!(
            literal_values(&audit),
            [(LiteralFactKind::File, "outside.rs")]
        );
    }
}

#[test]
fn unrelated_native_strings_and_nested_calls_are_not_argument_envelopes() {
    let arguments = r#"{"path":"not-a-fact.rs"}"#;
    let call = serde_json::json!({"type":"function_call","arguments":arguments});
    for value in [
        serde_json::json!({"type":"custom_tool_call","input":arguments}),
        serde_json::json!({"type":"custom_tool_call","arguments":arguments}),
        serde_json::json!({"type":"function_call_output","output":arguments}),
        serde_json::json!({"type":"function_call_output","arguments":arguments}),
        serde_json::json!({"type":"message","content":arguments}),
        serde_json::json!({"type":"function_call","input":arguments}),
        serde_json::json!({"type":"function_call","args":arguments}),
        serde_json::json!({"type":"function_call","arguments":[arguments]}),
        serde_json::json!({"type":"function_call","arguments":{"text":arguments}}),
        serde_json::json!({"type":"event_msg","payload":call}),
        serde_json::json!({"type":"response_item","payload":[call]}),
        serde_json::json!({"type":"response_item","payload":{"type":"message","content":call}}),
        serde_json::json!([call]),
        serde_json::json!({"unrelated":call}),
    ] {
        let raw = value.to_string();
        assert!(
            audit_codex_record(raw.as_bytes())
                .unwrap()
                .facts()
                .is_empty(),
            "{raw}"
        );
    }
}

#[test]
fn command_patch_and_result_prose_remain_literal_content_not_file_effects() {
    let arguments = serde_json::json!({
        "cmd": "apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: shell.rs\n+x\n*** End Patch\nPATCH",
        "code": "await tools.apply_patch('*** Begin Patch\\n*** Add File: nested.rs\\n+x\\n*** End Patch')",
        "patch": "*** Begin Patch\n*** Update File: direct.rs\n@@\n-a\n+b\n*** End Patch",
        "result": "{\"path\":\"result.rs\"}",
        "text": "{\"file_path\":\"text.rs\"}",
    });
    let row = argument_row(&arguments.to_string());
    let facts = row.activity.unwrap().facts;
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].kind, LiteralFactKind::Command);
    assert_eq!(facts[0].value, arguments["cmd"].as_str().unwrap());
}

#[test]
fn decoded_fact_count_shares_the_outer_record_limit() {
    use ctx_history_core::MAX_PROVIDER_DECLARED_FACTS;

    for count in [
        MAX_PROVIDER_DECLARED_FACTS - 1,
        MAX_PROVIDER_DECLARED_FACTS,
        MAX_PROVIDER_DECLARED_FACTS + 1,
    ] {
        let arguments = serde_json::json!({"paths": vec!["same.rs"; count]}).to_string();
        let encoded = serde_json::to_string(&arguments).unwrap();
        for outside in ["", r#", "path":"outside.rs""#] {
            let raw = format!(r#"{{"type":"function_call","arguments":{encoded}{outside}}}"#);
            let audit = audit_codex_record(raw.as_bytes()).unwrap();
            let total = count + usize::from(!outside.is_empty());
            assert_eq!(
                audit.facts().len(),
                if total <= MAX_PROVIDER_DECLARED_FACTS {
                    total
                } else {
                    0
                }
            );
        }
    }
}

#[test]
fn argument_json_keeps_the_existing_deserializer_depth_limit() {
    for depth in [126, 127] {
        let arguments = format!(
            "{}{}{}",
            "[".repeat(depth),
            r#"{"path":"deep.rs"}"#,
            "]".repeat(depth)
        );
        let row = argument_row(&arguments);
        assert_eq!(row.activity.unwrap().facts.len(), usize::from(depth == 126));
    }
    let arguments = format!(
        r#"[{{"path":"partial.rs"}},{}0{}]"#,
        "[".repeat(128),
        "]".repeat(128)
    );
    assert!(argument_row(&arguments).activity.unwrap().facts.is_empty());
}

#[test]
fn decoded_argument_envelope_byte_limit_is_not_a_prefix_capture() {
    use ctx_history_core::MAX_CORE_CONTENT_BYTES;

    let document = r#"{"path":"small.rs"}"#;
    for bytes in [MAX_CORE_CONTENT_BYTES, MAX_CORE_CONTENT_BYTES + 1] {
        let arguments = format!("{document}{}", " ".repeat(bytes - document.len()));
        let encoded = serde_json::to_string(&arguments).unwrap();
        let raw = format!(r#"{{"type":"function_call","arguments":{encoded}}}"#);
        let audit = audit_codex_record(raw.as_bytes()).unwrap();
        assert_eq!(
            audit.facts().len(),
            usize::from(bytes == MAX_CORE_CONTENT_BYTES)
        );
    }
}

#[test]
fn existing_literal_auditor_byte_and_recognized_key_bounds_are_unchanged() {
    use ctx_history_core::MAX_CORE_CONTENT_BYTES;

    for bytes in [MAX_CORE_CONTENT_BYTES, MAX_CORE_CONTENT_BYTES + 1] {
        let raw = serde_json::json!({"path": "x".repeat(bytes)}).to_string();
        let audit = audit_codex_record(raw.as_bytes()).unwrap();
        assert_eq!(
            audit.facts().len(),
            usize::from(bytes == MAX_CORE_CONTENT_BYTES)
        );
    }
    // The Codex allowlist has fewer than 64 keys; exercise the auditor's
    // independent per-object ceiling with a synthetic recognizing callback.
    for count in [64, 65] {
        let fields = (0..count)
            .map(|i| format!(r#""key{i}":"literal""#))
            .collect::<Vec<_>>()
            .join(",");
        let raw = format!("{{{fields}}}");
        let audit = audit_json(raw.as_bytes(), |_| None, |_| Some(LiteralFactKind::File)).unwrap();
        assert_eq!(audit.facts().len(), if count == 64 { 64 } else { 0 });
    }
}
