use super::super::{classify_codex_record, codex_skip_projection};
use super::*;

/// Every envelope discriminator the reader's class function branches on, plus
/// shapes it must fall through on.
const ENVELOPE_TYPES: &[&str] = &[
    "session_meta",
    "compacted",
    "response_item",
    "event_msg",
    "turn_context",
    "world_state",
    "inter_agent_communication_metadata",
    "",
    "Response_Item",
];

/// Every payload discriminator either class branch names, plus real Codex
/// payload types the branches only reach through their fallthrough arms.
const PAYLOAD_TYPES: &[&str] = &[
    "message",
    "reasoning",
    "function_call",
    "custom_tool_call",
    "web_search_call",
    "tool_search_call",
    "function_call_output",
    "custom_tool_call_output",
    "tool_search_output",
    "patch_apply_end",
    "web_search_end",
    "exec_command_end",
    "command_complete",
    "tool_complete",
    "task_started",
    "task_complete",
    "turn_aborted",
    "context_compacted",
    "token_count",
    "sub_agent_activity",
    "agent_message",
    "agent_reasoning",
    "user_message",
    "thread_settings_applied",
    "mcp_tool_call_end",
    "some_tool_output",
    "some_tool_result",
    "some_tool_response",
    "tool_output",
    "tool_result",
    "command_output",
    "command_result",
    "totally_unknown",
];

fn record(record_type: &str, item_type: &str) -> String {
    format!(
        r#"{{"timestamp":"2026-07-18T11:31:06.111Z","type":"{record_type}","payload":{{"type":"{item_type}","info":{{"a":1,"b":[1,2,3],"c":null,"d":true}},"text":"hello world"}}}}"#
    )
}

/// The prefilter's skip set is the reader's skip set.
///
/// This is the anti-drift gate: the prefilter decides from raw bytes and the
/// structural probe decides from parsed fields, and for every shape both must
/// agree about whether the reader materializes anything.
#[test]
fn prefilter_skip_set_matches_what_the_reader_materializes() {
    for record_type in ENVELOPE_TYPES {
        for item_type in PAYLOAD_TYPES {
            let raw = record(record_type, item_type);
            let expected = codex_skip_projection(
                classify_codex_record(raw.as_bytes())
                    .expect("fixture record parses")
                    .class,
            );
            let actual = match prefilter_codex_record(raw.as_bytes()) {
                CodexRecordAdmission::NoProjection(projection) => Some(projection),
                CodexRecordAdmission::Probe => None,
            };
            assert_eq!(
                actual, expected,
                "prefilter disagreed with the reader for {record_type}/{item_type}"
            );
        }
    }
}

/// Envelopes with no payload discriminator must classify exactly like the
/// structural probe's `None` item type.
#[test]
fn prefilter_matches_the_reader_without_a_payload_type() {
    let shapes = [
        r#"{"type":"event_msg","payload":{"other":1}}"#,
        r#"{"type":"event_msg","payload":null}"#,
        r#"{"type":"event_msg","payload":[1,2]}"#,
        r#"{"type":"event_msg","payload":7}"#,
        r#"{"type":"event_msg","payload":"text"}"#,
        r#"{"type":"event_msg","payload":{"type":null}}"#,
        r#"{"type":"event_msg"}"#,
        r#"{"type":"response_item","payload":{}}"#,
        r#"{"type":"turn_context","payload":{"cwd":"/tmp"}}"#,
        r#"{"type":"compacted","payload":{"message":"x"}}"#,
        r#"{"type":"session_meta","payload":{"id":"abc"}}"#,
    ];
    for raw in shapes {
        let expected = codex_skip_projection(
            classify_codex_record(raw.as_bytes())
                .expect("fixture record parses")
                .class,
        );
        let actual = match prefilter_codex_record(raw.as_bytes()) {
            CodexRecordAdmission::NoProjection(projection) => Some(projection),
            CodexRecordAdmission::Probe => None,
        };
        assert_eq!(actual, expected, "prefilter disagreed for {raw}");
    }
}

/// A skip is a proof that the structural probe would have succeeded. Anything
/// rejected by parsing or the narrow malformed-lineage boundary has to reach
/// the probe so the rejection is still recorded.
#[test]
fn prefilter_never_skips_a_record_the_probe_rejects() {
    let rejected = [
        // Truncated, spliced, and unterminated shapes seen in real rollouts.
        r#"{"type":"event_msg","payload":{"type":"token_count","info":{"a":1"#,
        r#"{"type":"event_msg","payload":{"type":"token_count","info":{"a":1{"type":"event_msg"}}}"#,
        r#"{"type":"event_msg","payload":{"type":"token_count"}}trailing"#,
        r#"{"type":"event_msg","payload":{"type":"token_count"}} {"type":"event_msg"}"#,
        // Envelope-grammar violations the probe reports as parse errors.
        r#"{"payload":{"type":"token_count"}}"#,
        r#"{"timestamp":"a","timestamp":"b","type":"event_msg"}"#,
        r#"{"type":7,"payload":{"type":"token_count"}}"#,
        r#"{"type":"event_msg","timestamp":7}"#,
        r#"{"type":"event_msg","payload":{"type":7}}"#,
        r#"{"type":"event_msg","payload":{"type":"token_count","call_id":7}}"#,
        r#"[{"type":"event_msg"}]"#,
        r#"{}"#,
        "",
        "\u{0}\u{0}\u{0}",
        // Number and literal grammar the probe rejects.
        r#"{"type":"event_msg","payload":{"type":"token_count","n":01}}"#,
        r#"{"type":"event_msg","payload":{"type":"token_count","n":+1}}"#,
        r#"{"type":"event_msg","payload":{"type":"token_count","n":1.}}"#,
        r#"{"type":"event_msg","payload":{"type":"token_count","n":NaN}}"#,
        r#"{"type":"event_msg","payload":{"type":"token_count","n":tru}}"#,
        r#"{"type":"event_msg","payload":{"type":"token_count",}}"#,
        r#"{"type":"event_msg","payload":{"type":"token_count","a":[1,]}}"#,
    ];
    for raw in rejected {
        assert!(
            classify_codex_record(raw.as_bytes()).is_err(),
            "fixture was expected to be rejected by the probe: {raw}"
        );
        assert_eq!(
            prefilter_codex_record(raw.as_bytes()),
            CodexRecordAdmission::Probe,
            "prefilter skipped a record the probe rejects: {raw}"
        );
    }

    let malformed_lineage = [
        r#"{"type":"event_msg","type":"event_msg","payload":{"type":"token_count"}}"#,
        r#"{"type":"event_msg","payload":{"type":"token_count"},"payload":{}}"#,
        r#"{"type":"event_msg","payload":{"type":"token_count","type":"token_count"}}"#,
        r#"{"type":"event_msg","payload":{"call_id":"a","call_id":"b","type":"token_count"}}"#,
    ];
    for raw in malformed_lineage {
        let probe = classify_codex_record(raw.as_bytes())
            .unwrap_or_else(|error| panic!("malformed lineage must reach its boundary: {error}"));
        assert!(
            probe.lineage_malformed(),
            "fixture was expected to be rejected at the lineage boundary: {raw}"
        );
        assert_eq!(
            prefilter_codex_record(raw.as_bytes()),
            CodexRecordAdmission::Probe,
            "prefilter skipped malformed lineage: {raw}"
        );
    }
}

/// Well-formed shapes the prefilter is deliberately too strict for still reach
/// the probe, which classifies them exactly as before.
#[test]
fn prefilter_defers_shapes_it_is_too_strict_for() {
    let deferred = [
        // A surrogate escape, whose pairing rules the prefilter will not model.
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"t\":\"\\ud83d\\ude00\"}}",
        // A non-ASCII key, which the probe has to validate as UTF-8.
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"k\u{e9}y\":1}}",
        // An escaped key, which the prefilter does not decode.
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"a\\u0062c\":1}}",
        // An escaped discriminator, likewise left to the probe to decode.
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_c\\u006funt\"}}",
    ];
    for raw in deferred {
        assert!(
            classify_codex_record(raw.as_bytes()).is_ok(),
            "fixture was expected to parse: {raw}"
        );
        assert_eq!(
            prefilter_codex_record(raw.as_bytes()),
            CodexRecordAdmission::Probe,
            "prefilter should have deferred: {raw}"
        );
    }
}

/// Real-shaped payload bodies must still be skipped: escapes, deep nesting,
/// unicode text, and long numbers all appear in Codex telemetry.
#[test]
fn prefilter_skips_realistic_ignored_bodies() {
    let skipped = [
        r#"{"timestamp":"2026-07-23T05:08:33.183Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1138394810,"cached_input_tokens":1120749312,"output_tokens":1694149},"model_context_window":272000},"rate_limits":{"primary":{"used_percent":0.5,"resets_in_seconds":86400},"plan_type":"pro","individual_limit":null}}}"#,
        r#"{"timestamp":"2026-07-23T05:08:33.183Z","type":"event_msg","payload":{"type":"agent_reasoning","text":"**Planning**\n\nLine with \"quotes\", a tab\there, emoji 🚀 and unicode ünïcödé."}}"#,
        r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","nested":{"a":{"b":{"c":{"d":[{"e":1e10},{"f":-0.25}]}}}}}}"#,
        r#"{"type":"response_item","payload":{"type":"agent_message","content":[{"type":"input_text","text":"hi"}]}}"#,
        r#"   {"type":"event_msg","payload":{"type":"token_count"}}   "#,
    ];
    for raw in skipped {
        assert_eq!(
            prefilter_codex_record(raw.as_bytes()),
            CodexRecordAdmission::NoProjection(CodexSkipProjection::Ignored),
            "prefilter should have skipped: {raw}"
        );
    }
}

/// Result envelopes always reach the structural content probe.
#[test]
fn prefilter_probes_result_envelopes_for_complete_content() {
    let probed = [
        r#"{"type":"event_msg","payload":{"type":"patch_apply_end","success":true}}"#,
        r#"{"type":"event_msg","payload":{"type":"exec_command_end","exit_code":0}}"#,
        r#"{"type":"event_msg","payload":{"type":"mcp_tool_call_end","result":{}}}"#,
        r#"{"type":"response_item","payload":{"type":"tool_result","output":"x"}}"#,
    ];
    for raw in probed {
        assert_eq!(
            prefilter_codex_record(raw.as_bytes()),
            CodexRecordAdmission::Probe,
            "prefilter should have probed: {raw}"
        );
    }
}

#[test]
fn prefilter_ignores_unknown_result_like_discriminators_by_construction() {
    let ignored = [
        r#"{"type":"event_msg","payload":{"type":"image_generation_end","result":"iVBORw0KGgo="}}"#,
        r#"{"type":"response_item","payload":{"type":"future_tool_result","result":"x"}}"#,
        r#"{"type":"event_msg","payload":{"type":"future_tool_response","output":"x"}}"#,
        r#"{"type":"event_msg","payload":{"type":"future_tool_end","result":"x"}}"#,
        r#"{"type":"response_item","payload":{"type":"command_output","output":"x"}}"#,
    ];
    for raw in ignored {
        let probe = classify_codex_record(raw.as_bytes()).unwrap();
        assert_eq!(
            codex_skip_projection(probe.class),
            Some(CodexSkipProjection::Ignored)
        );
        assert_eq!(probe.output, None);
        assert_eq!(
            prefilter_codex_record(raw.as_bytes()),
            CodexRecordAdmission::NoProjection(CodexSkipProjection::Ignored),
            "unknown discriminator should stay on the bounded ignored path: {raw}"
        );
    }
}

/// Classes that reach parsed state are never skipped.
#[test]
fn prefilter_always_probes_materialized_classes() {
    let probed = [
        r#"{"type":"session_meta","payload":{"id":"019f75da-42ea-7e01-9569-26cbf8601b25"}}"#,
        r#"{"type":"compacted","payload":{"message":"summary"}}"#,
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[]}}"#,
        r#"{"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
        r#"{"type":"response_item","payload":{"type":"function_call","name":"exec"}}"#,
        r#"{"type":"response_item","payload":{"type":"function_call_output","output":"x"}}"#,
        r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","output":"x"}}"#,
        r#"{"type":"response_item","payload":{"type":"tool_search_output","output":"x"}}"#,
    ];
    for raw in probed {
        assert_eq!(
            prefilter_codex_record(raw.as_bytes()),
            CodexRecordAdmission::Probe,
            "prefilter should have probed: {raw}"
        );
    }
}

/// Byte-mutation sweep: whatever the prefilter skips, the probe must accept and
/// classify into the same skip set.
#[test]
fn prefilter_skips_are_always_probe_provable() {
    let seeds = [
        r#"{"timestamp":"2026-07-18T11:31:06.111Z","type":"event_msg","payload":{"type":"token_count","info":{"a":1,"b":[1,2,3],"c":null,"d":true},"text":"hi \"there\"\n"}}"#,
        r#"{"type":"response_item","payload":{"type":"agent_message","content":[{"text":"x"}]}}"#,
        r#"{"type":"event_msg","payload":{"type":"patch_apply_end"}}"#,
    ];
    let injected = [
        b'"', b'\\', b'{', b'}', b'[', b']', b',', b':', b'0', b'e', b' ', 0, 0x7f, 0xff,
    ];
    let mut skipped = 0_usize;
    for seed in seeds {
        for index in 0..seed.len() {
            for byte in injected {
                let mut mutated = seed.as_bytes().to_vec();
                mutated[index] = byte;
                let Some(expected) = classify_codex_record(&mutated)
                    .ok()
                    .and_then(|probe| codex_skip_projection(probe.class))
                else {
                    assert_eq!(
                        prefilter_codex_record(&mutated),
                        CodexRecordAdmission::Probe,
                        "prefilter skipped a mutation the probe does not skip at {index}"
                    );
                    continue;
                };
                if let CodexRecordAdmission::NoProjection(projection) =
                    prefilter_codex_record(&mutated)
                {
                    assert_eq!(
                        projection, expected,
                        "mutation at {index} changed the class"
                    );
                    skipped += 1;
                }
            }
        }
    }
    assert!(skipped > 100, "mutation sweep exercised too few skips");
}

/// Truncation sweep: a record cut short is never skipped.
#[test]
fn prefilter_never_skips_a_truncated_record() {
    let seed = r#"{"timestamp":"2026-07-18T11:31:06.111Z","type":"event_msg","payload":{"type":"token_count","info":{"a":1,"b":[1,2,3]}}}"#;
    for length in 0..seed.len() {
        assert_eq!(
            prefilter_codex_record(&seed.as_bytes()[..length]),
            CodexRecordAdmission::Probe,
            "prefilter skipped a record truncated to {length} bytes"
        );
    }
}

#[test]
fn plain_json_string_scan_stops_on_every_terminator() {
    assert_eq!(plain_json_string_bytes(b""), 0);
    assert_eq!(plain_json_string_bytes(b"abc\"def"), 3);
    assert_eq!(plain_json_string_bytes(b"abc\\ndef"), 3);
    assert_eq!(plain_json_string_bytes(b"abcdefghijklmnop\"x"), 16);
    assert_eq!(plain_json_string_bytes(b"abcdefghijklmno\x01x"), 15);
    assert_eq!(plain_json_string_bytes("émoji 🚀 text".as_bytes()), 16);
    assert_eq!(plain_json_string_bytes(b"plain"), 5);
    // Every terminator is found from every alignment.
    for prefix in 0..24_usize {
        for terminator in [b'"', b'\\', 0x00, 0x1f] {
            let mut bytes = vec![b'a'; prefix];
            bytes.push(terminator);
            bytes.extend_from_slice(b"tail");
            assert_eq!(
                plain_json_string_bytes(&bytes),
                prefix,
                "missed {terminator:#04x} at offset {prefix}"
            );
        }
    }
}
