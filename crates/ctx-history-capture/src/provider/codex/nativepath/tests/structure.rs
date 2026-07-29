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

#[test]
fn canonical_exit_parser_accepts_long_leading_zeroes_and_rejects_true_overflow() {
    let leading_zero_failure = format!("Process exited with code {}7", "0".repeat(128));
    let true_overflow = format!("Process exited with code {}2147483648", "0".repeat(128));
    for (call_id, output, expected) in [
        (
            "leading-zero-failure",
            leading_zero_failure.as_str(),
            crate::OutputOutcomeMetadata {
                outcome: crate::OutputOutcome::Failure,
                exit_code: Some(7),
                duration_ms: None,
            },
        ),
        (
            "true-overflow",
            true_overflow.as_str(),
            crate::OutputOutcomeMetadata {
                outcome: crate::OutputOutcome::Unknown,
                exit_code: None,
                duration_ms: None,
            },
        ),
    ] {
        let line = tool_output(call_id, output);
        let decoded = serde_json::from_str::<Value>(&line).unwrap();
        let canonical =
            crate::provider::codex::events::codex_tool_output_outcome(&decoded["payload"]);
        let structural = super::record::classify_codex_record(line.as_bytes())
            .unwrap()
            .output
            .unwrap()
            .outcome;
        assert_eq!(canonical, expected);
        assert_eq!(structural, canonical);
    }

    let contents = [
        session_meta("exit-code-owner"),
        tool_output("leading-zero-failure", &leading_zero_failure),
        tool_output("true-overflow", &true_overflow),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "exit-code-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (pro_scan, pro) = scan_collect_profile(
        discover_one(&path, "exit-code-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert_eq!(core_scan.rejections, pro_scan.rejections);
    assert!(core_scan.rejections.is_empty());
    assert_eq!(core.rows, pro.rows);
    assert_eq!(core.pages, pro.pages);
    assert_eq!(core.physical_records, pro.physical_records);
    assert_eq!(core.frontiers, pro.frontiers);
    assert_eq!(core.core_receipts, pro.core_receipts);
    assert_eq!(core.rows.len(), 1);
    assert_eq!(core.rows[0].provider_event.payload["exit_code"], 7);
    assert_eq!(
        pro.pro_outputs
            .iter()
            .map(|output| output.outcome.clone())
            .collect::<Vec<_>>(),
        vec![
            crate::OutputOutcomeMetadata {
                outcome: crate::OutputOutcome::Failure,
                exit_code: Some(7),
                duration_ms: None,
            },
            crate::OutputOutcomeMetadata {
                outcome: crate::OutputOutcome::Unknown,
                exit_code: None,
                duration_ms: None,
            },
        ]
    );
}

#[test]
fn canonical_wall_time_grammar_is_profile_invariant_for_internal_minus() {
    let duration_adversary =
        "Process exited with code 7\nWall time: 1-2 seconds\nDURATION_PROFILE_SECRET";
    let contents = [
        session_meta("duration-owner"),
        tool_call("duration-call"),
        tool_output("duration-call", duration_adversary),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);

    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "duration-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (pro_scan, pro) = scan_collect_profile(
        discover_one(&path, "duration-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert!(core_scan.rejections.is_empty());
    assert_eq!(core_scan.rejections, pro_scan.rejections);
    assert_eq!(core.rows, pro.rows);
    let diagnostic = core
        .rows
        .iter()
        .find(|row| row.raw_ordinal == 2)
        .expect("failure should retain one sparse Core diagnostic");
    assert_eq!(diagnostic.provider_event.payload["exit_code"], 7);
    assert_eq!(diagnostic.provider_event.payload["duration_ms"], 1_000);
    assert!(!format!("{:?}", core.rows).contains("DURATION_PROFILE_SECRET"));
    assert_eq!(pro.pro_outputs.len(), 1);
    assert_eq!(
        pro.pro_outputs[0].outcome,
        crate::OutputOutcomeMetadata {
            outcome: crate::OutputOutcome::Failure,
            exit_code: Some(7),
            duration_ms: Some(1_000),
        }
    );
}

#[test]
fn duplicate_unknown_output_keys_keep_profile_invariance_with_bounded_preflight() {
    let duplicate_output = r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"duplicate-content","output":{"shadow":{"exit_code":7},"shadow":{"ok":true}}}}"#.to_owned()
        + "\n";
    let contents = [
        session_meta("duplicate-content-owner"),
        tool_call("duplicate-content"),
        duplicate_output,
    ]
    .concat();
    let (_temp, path) = write_source(&contents);

    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "duplicate-content-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (pro_scan, pro) = scan_collect_profile(
        discover_one(&path, "duplicate-content-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert_eq!(core_scan.rejections, pro_scan.rejections);
    assert!(core_scan.rejections.is_empty());
    assert_eq!(core.rows, pro.rows);
    assert_eq!(pro.pro_outputs.len(), 1);
    assert_eq!(
        pro.pro_outputs[0].outcome.outcome,
        crate::OutputOutcome::Success
    );
    assert_eq!(
        String::from_utf8(pro.pro_outputs[0].content.clone()).unwrap(),
        r#"{"shadow":{"ok":true}}"#
    );
}

#[test]
fn hundred_duplicate_shadow_fields_hydrate_only_the_exact_last_value() {
    const DUPLICATE_FIELDS: usize = 100;
    const FINAL_SHADOW_BYTES: usize = 70_000;

    let shadow = "x".repeat(FINAL_SHADOW_BYTES);
    let mut duplicate_output = String::with_capacity(7_100_000);
    duplicate_output.push_str(
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"duplicate-shadow","output":{"#,
    );
    for index in 0..DUPLICATE_FIELDS {
        if index != 0 {
            duplicate_output.push(',');
        }
        write!(
            duplicate_output,
            r#""shadow":{}"#,
            serde_json::to_string(&shadow).unwrap()
        )
        .unwrap();
    }
    duplicate_output.push_str("}}}\n");
    assert!(duplicate_output.len() < MAX_CODEX_PAGE_BYTES);
    assert!(
        duplicate_output.len().saturating_add(2) / 3 * 4 > MAX_CODEX_PAGE_BYTES,
        "the discarded syntactic members must reproduce the old false size rejection"
    );
    let expected_content = format!(r#"{{"shadow":"{shadow}"}}"#).into_bytes();
    assert_eq!(expected_content.len(), 70_013);

    let contents = [session_meta("duplicate-shadow-owner"), duplicate_output].concat();
    let (_temp, path) = write_source(&contents);
    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "duplicate-shadow-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (pro_scan, pro) = scan_collect_profile(
        discover_one(&path, "duplicate-shadow-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert_eq!(core_scan.rejections, pro_scan.rejections);
    assert!(core_scan.rejections.is_empty());
    assert_eq!(core.rows, pro.rows);
    assert_eq!(core.pages, pro.pages);
    assert_eq!(core.physical_records, pro.physical_records);
    assert_eq!(core.frontiers, pro.frontiers);
    assert_eq!(core.core_receipts, pro.core_receipts);
    assert!(core.rows.is_empty());
    assert_eq!(pro.pro_outputs.len(), 1);
    assert_eq!(pro.pro_outputs[0].content, expected_content);
    assert_eq!(
        pro.pro_outputs[0].outcome.outcome,
        crate::OutputOutcome::Unknown
    );
    assert!(pro.pro_pages[0].1 <= MAX_CODEX_PAGE_BYTES);
}

#[test]
fn million_distinct_unknown_keys_fail_locally_without_core_or_pro_leak() {
    const DISTINCT_UNKNOWN_KEYS: usize = 1_000_001;

    let mut adversary = String::with_capacity(12 * 1024 * 1024);
    adversary.push_str(
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","#,
    );
    for index in 0..DISTINCT_UNKNOWN_KEYS {
        write!(adversary, r#""{index}":0,"#).unwrap();
    }
    adversary.push_str(
        r#""call_id":"million-keys","output":"Process exited with code 7\nMILLION_KEY_SECRET"}}"#,
    );
    adversary.push('\n');
    assert!(adversary.len() < MAX_CODEX_RECORD_BYTES);

    let contents = [
        session_meta("million-key-owner"),
        adversary,
        message("assistant", "survives bounded structural rejection"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);

    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "million-key-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (pro_scan, pro) = scan_collect_profile(
        discover_one(&path, "million-key-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert_eq!(core_scan.rejections, pro_scan.rejections);
    assert_eq!(core_scan.rejections.len(), 1);
    assert_eq!(core_scan.rejections[0].raw_ordinal, 1);
    assert_eq!(
        core_scan.rejections[0].reason,
        "malformed Codex JSON record"
    );
    assert_eq!(core.rows, pro.rows);
    assert_eq!(core.rows.len(), 1);
    assert_eq!(core.rows[0].raw_ordinal, 2);
    assert!(core.pro_outputs.is_empty());
    assert!(pro.pro_outputs.is_empty());
    assert_eq!(core_scan.counters.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(pro_scan.counters.result_body_bytes_decoded_or_allocated, 0);
    assert!(!format!("{:?}", core.rows).contains("MILLION_KEY_SECRET"));
}

#[test]
fn output_validation_and_rejection_are_profile_invariant_before_core_elision() {
    let contents = [
        r#"{"timestamp":"2026-01-01T00:00:00Z","type":"response_item","payload":{"type":"function_call_output","call_id":"before-success","output":"Script completed"}}"#.to_owned()
            + "\n",
        r#"{"timestamp":"2026-01-01T00:00:00Z","type":"response_item","payload":{"type":"function_call_output","call_id":"before-unknown","output":""}}"#.to_owned()
            + "\n",
        session_meta("validation-owner"),
        r#"{"timestamp":"not-rfc3339","type":"response_item","payload":{"type":"function_call_output","call_id":"bad-time-success","output":"Script completed"}}"#.to_owned()
            + "\n",
        r#"{"timestamp":"not-rfc3339","type":"response_item","payload":{"type":"function_call_output","call_id":"bad-time-failure","output":"Process exited with code 7"}}"#.to_owned()
            + "\n",
        r#"{"timestamp":null,"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"duplicate-time","output":"Process exited with code 7"}}"#.to_owned()
            + "\n",
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":null,"type":"function_call_output","call_id":"duplicate-type","output":"Process exited with code 7"}}"#.to_owned()
            + "\n",
        r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":null,"call_id":"duplicate-call","output":"Process exited with code 7"}}"#.to_owned()
            + "\n",
    ]
    .concat();
    let (_temp, path) = write_source(&contents);

    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "validation-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (pro_scan, pro) = scan_collect_profile(
        discover_one(&path, "validation-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert!(core.rows.is_empty());
    assert!(core.pro_outputs.is_empty());
    assert!(pro.rows.is_empty());
    assert!(pro.pro_outputs.is_empty());
    assert_eq!(core_scan.rejections, pro_scan.rejections);
    assert_eq!(core_scan.rejections.len(), 7);
    assert_eq!(
        core_scan
            .rejections
            .iter()
            .map(|rejection| rejection.raw_ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1, 3, 4, 5, 6, 7]
    );
    assert_eq!(
        core_scan
            .rejections
            .iter()
            .map(|rejection| rejection.reason)
            .collect::<Vec<_>>(),
        vec![
            "Codex output appeared before session metadata",
            "Codex output appeared before session metadata",
            "Codex output timestamp is not valid RFC3339",
            "Codex output timestamp is not valid RFC3339",
            "malformed Codex JSON record",
            "malformed Codex JSON record",
            "malformed Codex JSON record",
        ]
    );
}

#[test]
fn pro_oversize_is_lane_local_and_cannot_change_core_pages_or_frontiers() {
    let oversized_body = "PRO_SIZE_SECRET".repeat(430_000);
    let contents = [
        session_meta("pro-size-owner"),
        failed_tool_output("pro-size-failure", &oversized_body),
        successful_tool_output("pro-size-success", &oversized_body),
        message("assistant", "survives lane-local oversized output"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);

    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "pro-size-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (pro_scan, pro) = scan_collect_profile(
        discover_one(&path, "pro-size-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert_eq!(core.rows, pro.rows);
    assert_eq!(core.pages, pro.pages);
    assert_eq!(core.physical_records, pro.physical_records);
    assert_eq!(core.frontiers, pro.frontiers);
    assert_eq!(core.core_receipts, pro.core_receipts);
    assert_eq!(core.rows.len(), 2);
    assert_eq!(core.rows[0].raw_ordinal, 1);
    assert_eq!(core.rows[0].provider_event.payload["exit_code"], 7);
    assert_eq!(core.rows[1].raw_ordinal, 3);
    assert!(core.pro_outputs.is_empty());
    assert!(pro.pro_outputs.is_empty());
    assert_eq!(core_scan.rejections, pro_scan.rejections);
    assert!(core_scan.rejections.is_empty());
    assert_eq!(core_scan.counters.result_body_bytes_decoded_or_allocated, 0);
    assert!(pro_scan.counters.result_body_bytes_decoded_or_allocated > 0);
    assert!(!format!("{:?}", core.rows).contains("PRO_SIZE_SECRET"));
}
