use super::*;

fn patch_row(name: &str, input: Value) -> CodexCoreRecordDraft {
    let payload = serde_json::json!({
        "type": "custom_tool_call", "id": "patch-item", "call_id": "patch-call",
        "name": name, "input": input,
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
    assert_eq!(row.lexical_body, payload.to_string());
    assert_eq!(row.structured_content, Some(payload));
    assert_eq!(
        row.provider_event_identity.as_ref().unwrap().value,
        "patch-item"
    );
    assert_eq!(
        row.activity
            .as_ref()
            .unwrap()
            .invocation
            .as_ref()
            .unwrap()
            .arguments,
        ActivityJsonCapture::Present { value: input },
    );
    row
}

fn patch_paths(row: &CodexCoreRecordDraft) -> Vec<&str> {
    row.activity
        .as_ref()
        .unwrap()
        .facts
        .iter()
        .filter(|fact| fact.kind == LiteralFactKind::File)
        .map(|fact| fact.value.as_str())
        .collect()
}

#[test]
fn direct_patch_headers_are_literal_file_references() {
    let patch = "*** Begin Patch\n*** Add File: new.rs\n+new\n*** Update File: ./old.rs\n*** Move to: /workspace/moved.rs\n@@\n-old\n+updated\n*** Delete File: deleted.rs\n*** End Patch";
    let row = patch_row("apply_patch", Value::String(patch.to_owned()));
    assert_eq!(
        patch_paths(&row),
        ["new.rs", "./old.rs", "/workspace/moved.rs", "deleted.rs"]
    );
}

#[test]
fn patch_declarations_in_tool_input_do_not_require_running_the_wrapper() {
    let patch =
        "*** Begin Patch\n*** Update File: src/example.rs\n@@\n-before\n+after\n*** End Patch";
    for code in [
        format!("const patch = `{patch}`; await tools.apply_patch(patch);"),
        format!(
            "await tools.apply_patch({});",
            serde_json::to_string(patch).unwrap()
        ),
    ] {
        let row = patch_row("exec", Value::String(code));
        assert_eq!(patch_paths(&row), ["src/example.rs"]);
    }
}

#[test]
fn patch_strings_in_object_and_json_encoded_arguments_have_the_same_references() {
    let patch = "*** Begin Patch\n*** Add File: dir/雪 and space.rs\n+new\n*** End Patch";
    let literal = serde_json::json!({"patch": patch});
    for arguments in [literal.clone(), Value::String(literal.to_string())] {
        let raw = serde_json::json!({"type":"function_call", "arguments":arguments}).to_string();
        let audit = audit_codex_record(raw.as_bytes()).unwrap();
        assert_eq!(audit.facts().len(), 1);
        assert_eq!(audit.facts()[0].value, "dir/雪 and space.rs");
    }
}

#[test]
fn patch_fact_capture_does_not_scan_messages_results_or_ambiguous_envelopes() {
    let patch = "*** Begin Patch\n*** Delete File: ignored.rs\n*** End Patch";
    for value in [
        serde_json::json!({"type":"message", "content":patch}),
        serde_json::json!({"type":"function_call_output", "output":patch}),
        serde_json::json!({"type":"custom_tool_call_output", "output":patch}),
        serde_json::json!({"type":"event_msg", "payload":{"type":"custom_tool_call", "input":patch}}),
        serde_json::json!({"type":"custom_tool_call", "input":patch, "arguments":patch}),
    ] {
        assert!(audit_codex_record(value.to_string().as_bytes())
            .unwrap()
            .facts()
            .is_empty());
    }
    let encoded = serde_json::to_string(patch).unwrap();
    for raw in [
        format!(r#"{{"type":"custom_tool_call","input":{encoded},"input":{encoded}}}"#),
        format!(r#"{{"type":"custom_tool_call","type":"message","input":{encoded}}}"#),
        format!(
            r#"{{"type":"custom_tool_call","name":"apply_patch","name":"other","input":{encoded}}}"#
        ),
    ] {
        assert!(audit_codex_record(raw.as_bytes())
            .unwrap()
            .facts()
            .is_empty());
    }
}

#[test]
fn incomplete_or_malformed_patch_blocks_do_not_add_partial_file_references() {
    for patch in [
        "*** Begin Patch\n*** Add File: ignored.rs\n+x",
        "*** Begin Patch\n*** Move to: ignored.rs\n*** End Patch",
        "*** Begin Patch\n*** Add File: \n+x\n*** End Patch",
        "*** Begin Patch\n*** Add File: ignored.rs\nnot an added line\n*** End Patch",
        "*** Begin Patch\n*** Delete File: ignored.rs\n*** Move to: also-ignored.rs\n*** End Patch",
        "*** Add File: ignored.rs\n+x\n*** End Patch",
    ] {
        let row = patch_row("apply_patch", Value::String(patch.to_owned()));
        assert!(patch_paths(&row).is_empty(), "{patch}");
    }
}

#[test]
fn patch_body_header_lookalikes_are_not_declarations_and_crlf_is_supported() {
    let patch = "*** Begin Patch\r\n*** Add File: actual.rs\r\n+*** Delete File: not-a-declaration.rs\r\n+*** Move to: not-a-move.rs\r\n*** End Patch\r\n";
    let row = patch_row("apply_patch", Value::String(patch.to_owned()));
    assert_eq!(patch_paths(&row), ["actual.rs"]);
}

#[test]
fn repeated_and_multiple_patch_declarations_keep_the_first_literal_path_order() {
    let patch = "*** Begin Patch\n*** Delete File: a.rs\n*** Delete File: a.rs\n*** End Patch";
    let second = "*** Begin Patch\n*** Delete File: b.rs\n*** End Patch";
    let row = patch_row(
        "exec",
        Value::String(format!("const p = `{patch}`; const q = `{second}`;")),
    );
    assert_eq!(patch_paths(&row), ["a.rs", "b.rs"]);
}

#[test]
fn json_escaped_patch_markers_and_discriminators_use_normal_json_decoding() {
    let raw = br#"{"type":"custom_tool_c\u0061ll","input":"\u002a\u002a\u002a Begin Patch\n*** Delete File: escaped.rs\n*** End Patch"}"#;
    let audit = audit_codex_record(raw).unwrap();
    assert_eq!(audit.facts().len(), 1);
    assert_eq!(audit.facts()[0].value, "escaped.rs");
}

#[test]
fn escaped_patch_lines_do_not_require_json_compatible_program_bodies() {
    for code in [
        r#"tools.apply_patch("*** Begin Patch\n*** Add File: literal.rs\n+let x = '\u{61}';\n*** End Patch")"#,
        r#"tools.apply_patch("*** Begin Patch\n*** Add File: literal.rs\n+let x = '\$';\n*** End Patch")"#,
        r#"tools.apply_patch('*** Begin Patch\n*** Add File: literal.rs\n+x\n*** End Patch')"#,
        r#"const patch = `*** Begin Patch\n*** Add File: literal.rs\n+${body}\n*** End Patch`;"#,
        r#"const command = "shell <<'PATCH'\n*** Begin Patch\n*** Add File: literal.rs\n+x\n*** End Patch\nPATCH";"#,
        r#"const command = "/* example\n*** Begin Patch\n*** Add File: literal.rs\n+x\n*** End Patch\n*/";"#,
    ] {
        let row = patch_row("exec", Value::String(code.to_owned()));
        assert_eq!(patch_paths(&row), ["literal.rs"], "{code}");
    }
}

#[test]
fn nested_quoting_decodes_header_values_without_confusing_escaped_backslashes() {
    for line_ending in ["\n", "\r\n"] {
        let path = r#"C:\new\root\quote"雪.rs"#;
        let mut patch = format!(
            "*** Begin Patch{line_ending}*** Delete File: {path}{line_ending}*** End Patch"
        );
        for _ in 0..4 {
            patch = serde_json::to_string(&patch).unwrap();
            let row = patch_row("exec", Value::String(format!("opaque text {patch};")));
            assert_eq!(patch_paths(&row), [path]);
        }
    }
}

#[test]
fn literal_template_spellings_are_not_resolved_to_computed_filenames() {
    let row = patch_row("exec", Value::String(
        r#"const name = 'resolved.rs'; const patch = `*** Begin Patch\n*** Delete File: ${name}\n*** End Patch`;"#.to_owned()
    ));
    assert_eq!(patch_paths(&row), ["${name}"]);
    assert!(!patch_paths(&row).contains(&"resolved.rs"));
}

#[test]
fn closing_marker_prefix_is_not_a_complete_patch() {
    for suffix in ["Suffix", "_extra", " extra", "\\extra"] {
        let patch = format!("*** Begin Patch\n*** Delete File: ghost.rs\n*** End Patch{suffix}");
        let row = patch_row("apply_patch", Value::String(patch));
        assert!(patch_paths(&row).is_empty());
    }
}

#[test]
fn escaped_non_json_quoted_headers_keep_literal_double_quotes() {
    for code in [
        r#"const patch = `*** Begin Patch\n*** Delete File: quote"name.rs\n*** End Patch`;"#,
        r#"const patch = '*** Begin Patch\n*** Delete File: quote"name.rs\n*** End Patch';"#,
    ] {
        let row = patch_row("exec", Value::String(code.to_owned()));
        assert_eq!(patch_paths(&row), [r#"quote"name.rs"#], "{code}");
    }
}

#[test]
fn repeated_begin_markers_without_an_end_do_not_restart_suffix_scans() {
    let row = patch_row(
        "apply_patch",
        Value::String("*** Begin Patch\n".repeat(10_000)),
    );
    assert!(patch_paths(&row).is_empty());
}

#[test]
fn an_incomplete_constructed_patch_does_not_hide_a_separate_literal_patch() {
    let code = r#"let patch = "*** Begin Patch\n";
patch += generatedBody;
patch += "*** End Patch";
await tools.apply_patch(patch);
await tools.apply_patch("*** Begin Patch\n*** Delete File: literal.rs\n*** End Patch");
const another = "*** Begin Patch\n" + generatedBody + "*** End Patch";"#;
    let row = patch_row("exec", Value::String(code.to_owned()));
    assert_eq!(patch_paths(&row), ["literal.rs"]);
}

#[test]
fn malformed_opening_separator_does_not_hide_a_later_complete_patch() {
    let code = r#"const example = "*** Begin Patch\r";
const patch = "*** Begin Patch\n*** Delete File: kept.rs\n*** End Patch";"#;
    let row = patch_row("exec", Value::String(code.to_owned()));
    assert_eq!(patch_paths(&row), ["kept.rs"]);
}
