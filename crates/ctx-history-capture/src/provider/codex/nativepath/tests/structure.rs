use super::*;

#[test]
fn structural_output_visitor_matches_decoded_payload_and_ignores_envelope_distractors() {
    let lines = [
        r#"{"timed_out":true,"status":"failed","output":"TIMED OUT","timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"unknown","output":"plain"}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"t\u0079pe":"function_call_output","call_id":"timeout","details":[{"timedOut":true,"durationMs":17}],"output":"prefix \u0054IMED OUT"}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"failure","details":{"nested":[{"exitCode":7},{"status":"f\u0061iled"}]},"duration_ms":19,"output":"failed"}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"success","details":[{"ok":true}],"\u006futput":"A\u00e9"}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"sorted","details":{"z":"Process exited with code 7","a":"Process exited with code 0"},"output":"ordered"}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"unicode-trim","details":{"status":"\u00a0FAILED\u00a0","error":"\u00a0"},"output":"trimmed"}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"duplicate-status","details":{"status":"failed","status":"success"},"output":"last status wins"}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"duplicate-arbitrary","details":{"shadow":{"exit_code":7},"shadow":{"ok":true}},"output":"last object wins"}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"duplicate-escaped","details":{"sta\u0074us":"failed","status":"success"},"output":"escaped key aliases last win"}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"duplicate-reverse","details":{"shadow":{"ok":true},"shadow":{"exit_code":9}},"output":"last failure wins"}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"duplicate-output","output":"first secret-bearing body","output":"last"}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"array-output","output":[{"text":"first"},{"ignored":"x"},{"content":"second"}]}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"array-precedence","output":[{"text":{"ignored":"nested"},"input_text":"not selected","content":"fallback"}]}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"object-output","output":{"content":{"text":"nested"}}}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"json-output","output":{"z":1e2,"a":false}}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"null-output","output":null}}"#,
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"duration-internal-minus","output":"Process exited with code 7\nWall time: 1-2 seconds"}}"#,
    ];

    for line in lines {
        let decoded = serde_json::from_str::<Value>(line).unwrap();
        let expected =
            crate::provider::codex::events::codex_tool_output_outcome(&decoded["payload"]);
        let probe = super::record::classify_codex_record(line.as_bytes()).unwrap();
        let structural = probe.output.unwrap();
        assert_eq!(structural.outcome, expected, "{line}");
        assert!(structural.has_exact_display_field, "{line}");
    }

    let escaped_output = super::record::classify_codex_record(lines[3].as_bytes())
        .unwrap()
        .output
        .unwrap();
    assert_eq!(escaped_output.output_bytes, Some("Aé".len()));
    let duplicate_output = super::record::classify_codex_record(lines[10].as_bytes())
        .unwrap()
        .output
        .unwrap();
    assert_eq!(duplicate_output.output_bytes, Some("last".len()));
}

#[test]
fn structural_output_marks_metadata_only_failures_as_non_display() {
    let line = r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"metadata-only","status":"failed"}}"#;
    let structural = super::record::classify_codex_record(line.as_bytes())
        .unwrap()
        .output
        .unwrap();
    assert_eq!(structural.outcome.outcome, crate::OutputOutcome::Failure);
    assert!(!structural.has_exact_display_field);
}

#[test]
fn structural_output_admits_genuine_mcp_direct_result_wrappers() {
    for (is_error, expected_outcome) in [
        (false, crate::OutputOutcome::Unknown),
        (true, crate::OutputOutcome::Failure),
    ] {
        let line = serde_json::json!({
            "timestamp": "2026-07-16T12:16:29.573Z",
            "type": "event_msg",
            "payload": {
                "type": "mcp_tool_call_end",
                "call_id": "exec-real-shape",
                "invocation": {"server": "node_repl", "tool": "js", "arguments": {}},
                "duration": {"secs": 0, "nanos": 42},
                "result": {
                    "Ok": {
                        "content": [{"type": "text", "text": "complete result"}],
                        "isError": is_error
                    }
                }
            }
        })
        .to_string();
        let probe = super::record::classify_codex_record(line.as_bytes()).unwrap();
        assert!(matches!(
            probe.class,
            super::record::CodexRecordClass::ExcludedResult(
                super::record::CodexResultKind::OtherResult
            )
        ));
        assert_eq!(probe.call_id.as_deref(), Some("exec-real-shape"));
        let structural = probe.output.unwrap();
        assert!(structural.has_exact_display_field);
        assert_eq!(structural.outcome.outcome, expected_outcome);
    }
}

#[test]
fn source_backed_display_contract_distinguishes_non_display_from_revision_gaps() {
    use super::rows::CodexSourceBackedDocumentEligibility;

    for line in [
        r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"metadata-only","status":"failed"}}"#,
        r#"{"type":"response_item","payload":{"type":"reasoning","encrypted_content":"opaque","summary":[]}}"#,
    ] {
        assert_eq!(
            source_backed_display_disposition(line),
            CodexSourceBackedDocumentEligibility::IntentionallyNonDisplay
        );
    }
    assert_eq!(
        source_backed_display_disposition(
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#,
        ),
        CodexSourceBackedDocumentEligibility::ParserRevisionGap
    );
}

fn source_backed_display_disposition(
    line: &str,
) -> super::rows::CodexSourceBackedDocumentEligibility<String> {
    let probe = super::record::classify_codex_record(line.as_bytes()).unwrap();
    let envelope = serde_json::from_str::<Value>(line).unwrap();
    super::rows::source_backed_display_text(&probe, &envelope["payload"])
}
