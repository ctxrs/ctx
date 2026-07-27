use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    time::Instant,
};

use ctx_history_core::{AgentType, CaptureProvider, EventType};
use ctx_history_store::CatalogSession;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::*;
use crate::{observe_ordinary_file, CODEX_SESSION_SOURCE_FORMAT};

fn jsonl(value: Value) -> String {
    let mut line = serde_json::to_string(&value).unwrap();
    line.push('\n');
    line
}

fn session_meta(id: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": id,
            "timestamp": "2026-01-01T00:00:00Z",
            "cwd": "/workspace",
            "source": "cli"
        }
    }))
}

fn message(role: &str, text: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": role,
            "content": [{"type": "input_text", "text": text}]
        }
    }))
}

fn tool_call(call_id: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": call_id,
            "arguments": {"cmd": "printf retained"}
        }
    }))
}

fn tool_output(call_id: &str, output: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "output": output
        }
    }))
}

fn successful_tool_output(call_id: &str, output: &str) -> String {
    tool_output(
        call_id,
        &format!("Script completed\nProcess exited with code 0\n{output}"),
    )
}

fn failed_tool_output(call_id: &str, output: &str) -> String {
    tool_output(
        call_id,
        &format!("Process exited with code 7\nWall time: 0.25 seconds\n{output}"),
    )
}

fn timed_out_tool_output(call_id: &str, output: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "timed_out": true,
            "duration_ms": 9_000,
            "output": format!("command timed out\n{output}")
        }
    }))
}

fn reasoning(text: &str) -> String {
    jsonl(json!({
        "timestamp": "2026-01-01T00:00:04Z",
        "type": "response_item",
        "payload": {
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": text}]
        }
    }))
}

fn catalog_session(path: &Path, native_session_id: &str) -> CatalogSession {
    let observation = observe_ordinary_file(path).unwrap();
    let modified_at_ms = observation
        .modified_at()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap())
        .unwrap_or_default();
    CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        source_root: path.parent().unwrap().display().to_string(),
        source_path: path.display().to_string(),
        external_session_id: Some(native_session_id.to_owned()),
        parent_external_session_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        external_agent_id: None,
        cwd: Some("/workspace".to_owned()),
        session_started_at_ms: Some(0),
        file_size_bytes: observation.len(),
        file_modified_at_ms: modified_at_ms,
        cataloged_at_ms: 1,
        metadata: json!({
            "inventory_file_change_token_v1": observation.token_hex(),
        }),
    }
}

fn discover_one(path: &Path, native_session_id: &str) -> CodexCatalogSource {
    let discovery = discover_codex_catalog_sources(&[catalog_session(path, native_session_id)]);
    assert!(discovery.rejections.is_empty());
    assert_eq!(discovery.sources.len(), 1);
    discovery.sources.into_iter().next().unwrap()
}

fn write_source(contents: &str) -> (TempDir, std::path::PathBuf) {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("rollout.jsonl");
    fs::write(&path, contents).unwrap();
    (temp, path)
}

#[derive(Default)]
struct CollectingSink {
    rows: Vec<CodexEventRow>,
    pro_outputs: Vec<crate::ProOutputObservation>,
    pages: Vec<(usize, usize)>,
    pro_pages: Vec<(usize, usize)>,
    physical_records: Vec<u64>,
    frontiers: Vec<(CodexNativeFrontier, CodexNativeFrontier)>,
    pro_frontiers: Vec<(CodexNativeFrontier, CodexNativeFrontier)>,
    core_receipts: Vec<CodexNativePageReceipt>,
    pro_receipts: Vec<CodexNativeProOutputPageReceipt>,
    owner_ids: BTreeSet<String>,
}

fn scan_collect(
    source: CodexCatalogSource,
    proof: Option<&CodexAppendProof>,
) -> (CodexSourceScan, CollectingSink) {
    scan_collect_profile(source, proof, CodexNativeProfile::CoreOnly)
}

fn scan_collect_profile(
    source: CodexCatalogSource,
    proof: Option<&CodexAppendProof>,
    profile: CodexNativeProfile,
) -> (CodexSourceScan, CollectingSink) {
    let mut scanner = CodexNativeScanner::new(source, proof, profile).unwrap();
    let mut collected = CollectingSink::default();
    while let Some(page) = scanner.next_page().unwrap() {
        match page {
            CodexNativeOwnedPage::Core(page) => {
                let units = page.core_rows.len();
                assert!(units <= MAX_CODEX_PAGE_ROWS);
                assert!(page.physical_records <= MAX_CODEX_PAGE_ROWS as u64);
                assert!(page.serialized_bytes <= MAX_CODEX_PAGE_BYTES);
                assert_eq!(
                    page.next_safe_frontier
                        .next_raw_ordinal
                        .saturating_sub(page.expected_frontier.next_raw_ordinal),
                    page.physical_records
                );
                if let Some(owner) = page.owner.as_ref() {
                    collected.owner_ids.insert(owner.native_session_id.clone());
                }
                let receipt = page.receipt();
                collected.pages.push((units, page.serialized_bytes));
                collected.physical_records.push(page.physical_records);
                collected
                    .frontiers
                    .push((page.expected_frontier, page.next_safe_frontier));
                collected.core_receipts.push(receipt);
                collected.rows.extend(page.core_rows);
            }
            CodexNativeOwnedPage::Pro(page) => {
                assert!(page.outputs.len() <= MAX_CODEX_PAGE_ROWS);
                assert!(page.serialized_bytes <= MAX_CODEX_PAGE_BYTES);
                let receipt = page.receipt();
                collected
                    .pro_pages
                    .push((page.outputs.len(), page.serialized_bytes));
                collected
                    .pro_frontiers
                    .push((page.expected_frontier, page.next_safe_frontier));
                collected.pro_receipts.push(receipt);
                collected.pro_outputs.extend(page.outputs);
            }
        }
    }
    let scan = scanner.finish().unwrap();
    (scan, collected)
}

#[test]
fn catalog_discovery_is_deterministic_and_rejects_bad_exact_observations() {
    let (_temp, path) = write_source(&session_meta("catalog-owner"));
    let valid = catalog_session(&path, "catalog-owner");
    let mut wrong_provider = valid.clone();
    wrong_provider.provider = CaptureProvider::Claude;
    let mut missing_token = valid.clone();
    missing_token.source_path = path
        .with_file_name("missing-token.jsonl")
        .display()
        .to_string();
    missing_token.metadata = json!({});

    let discovery =
        discover_codex_catalog_sources(&[wrong_provider, missing_token, valid.clone(), valid]);
    assert_eq!(discovery.ineligible, 1);
    assert_eq!(discovery.sources.len(), 0);
    assert_eq!(
        discovery
            .rejections
            .iter()
            .map(|rejection| rejection.reason)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "Codex catalog change token is missing",
            "duplicate Codex catalog path",
        ])
    );
}

#[test]
fn raw_ordinals_include_headers_outputs_malformed_and_ignored_records() {
    let contents = [
        session_meta("ordinal-owner"),
        message("user", "first retained"),
        tool_output("call-1", "excluded body"),
        "{malformed json}\n".to_owned(),
        tool_call("call-2"),
        jsonl(json!({"type": "turn_context", "payload": {"cwd": "/workspace"}})),
        message("assistant", "last retained"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let source = discover_one(&path, "ordinal-owner");
    let (scan, sink) = scan_collect(source, None);

    assert_eq!(
        sink.rows
            .iter()
            .map(|row| row.raw_ordinal)
            .collect::<Vec<_>>(),
        vec![1, 4, 6]
    );
    assert_eq!(scan.next_raw_ordinal, 7);
    assert_eq!(scan.counters.complete_records, 7);
    assert_eq!(scan.counters.native_result_records, 1);
    assert_eq!(scan.counters.malformed_records, 1);
    assert_eq!(scan.rejections[0].raw_ordinal, 3);
}

#[test]
fn output_heavy_scan_never_constructs_result_bodies_hashes_or_previews() {
    let secret = "RESULT_ONLY_MARKER_".repeat(32_768);
    let contents = [
        session_meta("output-owner"),
        message("user", "small request"),
        tool_call("call-output"),
        tool_output("call-output", &secret),
        message("assistant", "small response"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let source = discover_one(&path, "output-owner");
    let (scan, sink) = scan_collect(source, None);

    assert_eq!(sink.rows.len(), 3);
    assert_eq!(scan.counters.native_result_records, 1);
    assert!(scan.counters.native_result_record_bytes > secret.len() as u64);
    assert_eq!(scan.counters.structural_json_parses, 5);
    assert_eq!(scan.counters.structural_output_probes, 1);
    assert_eq!(scan.counters.typed_json_parses, 4);
    assert_eq!(scan.counters.typed_output_parses, 0);
    assert_eq!(scan.counters.retained_json_parses, 3);
    assert_eq!(scan.counters.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(scan.counters.result_hashes_created, 0);
    assert_eq!(scan.counters.result_previews_created, 0);
    assert_eq!(scan.counters.result_touches_created, 0);
    assert_eq!(scan.counters.result_fts_rows_created, 0);
    assert_eq!(scan.counters.result_handoffs_created, 0);
    let prepared_rows = format!("{:?}", sink.rows);
    assert!(!prepared_rows.contains("RESULT_ONLY_MARKER_"));
}

#[test]
fn core_and_pro_profiles_match_while_pro_receives_success_failure_timeout_and_unknown() {
    let success_marker = "SUCCESS_OUTPUT_ONLY_MARKER";
    let failure_marker = "FAILURE_BODY_MUST_NOT_SURVIVE";
    let timeout_marker = "TIMEOUT_BODY_MUST_NOT_SURVIVE";
    let unknown_marker = "UNKNOWN_OUTPUT_ONLY_MARKER";
    let contents = [
        session_meta("fanout-owner"),
        message("user", "run both"),
        tool_call("call-success"),
        successful_tool_output("call-success", success_marker),
        tool_call("call-failure"),
        failed_tool_output("call-failure", failure_marker),
        tool_call("call-timeout"),
        timed_out_tool_output("call-timeout", timeout_marker),
        tool_call("call-unknown"),
        tool_output("call-unknown", unknown_marker),
        message("assistant", "done"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);

    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "fanout-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (fanout_scan, fanout) = scan_collect_profile(
        discover_one(&path, "fanout-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert_eq!(core.rows, fanout.rows);
    assert_eq!(
        core_scan.full_revision_sha256,
        fanout_scan.full_revision_sha256
    );
    assert_eq!(
        core_scan.complete_prefix_sha256,
        fanout_scan.complete_prefix_sha256
    );
    assert_eq!(core_scan.next_raw_ordinal, fanout_scan.next_raw_ordinal);
    assert_eq!(core_scan.rejections, fanout_scan.rejections);
    assert_eq!(core.core_receipts, fanout.core_receipts);
    assert!(core.pro_outputs.is_empty());
    assert_eq!(fanout.pro_outputs.len(), 4);

    let output = &fanout.pro_outputs[0];
    assert_eq!(output.outcome.outcome, crate::OutputOutcome::Success);
    assert_eq!(
        output.coordinate.unit_key,
        "codex/nativepath/fanout-owner/3/0"
    );
    assert_eq!(output.coordinate.native_sequence, 3);
    assert_eq!(output.coordinate.source_record_ordinal, Some(3));
    assert_eq!(output.coordinate.source_record_subrecord_index, Some(0));
    assert_eq!(output.call_id.as_deref(), Some("call-success"));
    assert!(String::from_utf8_lossy(&output.content).contains(success_marker));
    assert!(!String::from_utf8_lossy(&output.content).contains(failure_marker));
    assert_eq!(
        fanout
            .pro_outputs
            .iter()
            .map(|output| output.outcome.outcome)
            .collect::<Vec<_>>(),
        vec![
            crate::OutputOutcome::Success,
            crate::OutputOutcome::Failure,
            crate::OutputOutcome::Timeout,
            crate::OutputOutcome::Unknown,
        ]
    );
    assert_eq!(
        fanout
            .pro_outputs
            .iter()
            .map(|output| output.coordinate.native_sequence)
            .collect::<Vec<_>>(),
        vec![3, 5, 7, 9]
    );
    for (output, marker) in fanout.pro_outputs.iter().zip([
        success_marker,
        failure_marker,
        timeout_marker,
        unknown_marker,
    ]) {
        assert!(String::from_utf8_lossy(&output.content).contains(marker));
        assert_eq!(output.kind, crate::OutputObservationKind::Command);
        assert!(output.command.is_some());
    }
    let locator: Value = serde_json::from_slice(&output.locator.payload).unwrap();
    assert_eq!(locator["raw_ordinal"], json!(3));
    assert_eq!(locator["source_path"], json!(path));

    let core_debug = format!("{:?}", core.rows);
    assert!(!core_debug.contains(success_marker));
    assert!(!core_debug.contains(failure_marker));
    assert!(!core_debug.contains(timeout_marker));
    assert!(!core_debug.contains(unknown_marker));
    assert_eq!(
        core.rows
            .iter()
            .filter(|row| matches!(
                row.provider_event.event_type,
                EventType::CommandOutput | EventType::ToolOutput
            ))
            .count(),
        2
    );
    let failure = core.rows.iter().find(|row| row.raw_ordinal == 5).unwrap();
    assert_eq!(failure.provider_event.event_type, EventType::CommandOutput);
    assert_eq!(failure.provider_event.payload["exit_code"], 7);
    assert_eq!(failure.provider_event.payload["duration_ms"], 250);
    assert_eq!(failure.provider_event.payload["timed_out"], false);
    assert_eq!(
        failure.provider_event.payload["output_bytes"],
        format!("Process exited with code 7\nWall time: 0.25 seconds\n{failure_marker}").len()
    );
    assert_eq!(failure.provider_event.payload["command"], "printf retained");
    let timeout = core.rows.iter().find(|row| row.raw_ordinal == 7).unwrap();
    assert_eq!(timeout.provider_event.event_type, EventType::CommandOutput);
    assert_eq!(timeout.provider_event.payload["timed_out"], true);
    assert_eq!(timeout.provider_event.payload["duration_ms"], 9_000);
    assert_eq!(
        timeout.provider_event.payload["output_bytes"],
        format!("command timed out\n{timeout_marker}").len()
    );
    assert_eq!(core_scan.counters.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(core_scan.counters.result_handoffs_created, 0);
    assert_eq!(core_scan.counters.structural_json_parses, 11);
    assert_eq!(core_scan.counters.structural_output_probes, 4);
    assert_eq!(core_scan.counters.typed_json_parses, 7);
    assert_eq!(core_scan.counters.typed_output_parses, 0);
    assert_eq!(fanout_scan.counters.structural_json_parses, 11);
    assert_eq!(fanout_scan.counters.structural_output_probes, 4);
    assert_eq!(fanout_scan.counters.typed_json_parses, 11);
    assert_eq!(fanout_scan.counters.typed_output_parses, 4);
    assert_eq!(fanout_scan.counters.result_hashes_created, 0);
    assert_eq!(fanout_scan.counters.result_previews_created, 0);
    assert_eq!(fanout_scan.counters.result_touches_created, 0);
    assert_eq!(fanout_scan.counters.result_fts_rows_created, 0);
    assert_eq!(fanout_scan.counters.result_handoffs_created, 4);
    assert_eq!(fanout.pages.len(), 1);
    assert_eq!(fanout.pages[0].0, fanout.rows.len());
    assert_eq!(fanout.pro_pages.len(), 1);
    assert_eq!(fanout.pro_pages[0].0, fanout.pro_outputs.len());
}

#[test]
fn pending_call_checkpoint_makes_fresh_and_append_outputs_identical_in_both_profiles() {
    for profile in [CodexNativeProfile::CoreOnly, CodexNativeProfile::CoreAndPro] {
        let initial = [
            session_meta("split-owner"),
            tool_call("split-success"),
            tool_call("split-failure"),
            tool_call("split-timeout"),
            tool_call("split-unknown"),
        ]
        .concat();
        let appended = [
            successful_tool_output("split-success", "success"),
            failed_tool_output("split-failure", "failure"),
            timed_out_tool_output("split-timeout", "timeout"),
            tool_output("split-unknown", ""),
        ]
        .concat();
        let complete = format!("{initial}{appended}");
        let (_temp, path) = write_source(&initial);

        let (initial_scan, _) =
            scan_collect_profile(discover_one(&path, "split-owner"), None, profile);
        let proof = initial_scan
            .bind_checkpoint("canonical-split", CodexCheckpointGeneration::new(90))
            .unwrap()
            .unwrap();
        let checkpoint_wire = serde_json::from_slice::<Value>(
            &proof.checkpoint.encode().expect("checkpoint should encode"),
        )
        .unwrap();
        assert_eq!(checkpoint_wire["version"], 4);
        assert_eq!(
            checkpoint_wire["pending_tool_authorities"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        let checkpoint_text = serde_json::to_string(&checkpoint_wire).unwrap();
        assert!(!checkpoint_text.contains("split-success"));
        assert!(!checkpoint_text.contains("printf retained"));

        fs::write(&path, &complete).unwrap();
        let (append_scan, append) =
            scan_collect_profile(discover_one(&path, "split-owner"), Some(&proof), profile);
        let (fresh_scan, fresh) =
            scan_collect_profile(discover_one(&path, "split-owner"), None, profile);

        let fresh_output_rows = fresh
            .rows
            .iter()
            .filter(|row| {
                matches!(
                    row.provider_event.event_type,
                    EventType::CommandOutput | EventType::ToolOutput
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(append.rows, fresh_output_rows);
        assert_eq!(
            append
                .rows
                .iter()
                .map(|row| (
                    row.raw_ordinal,
                    row.provider_event.event_type,
                    row.normalized_body_hash.as_str()
                ))
                .collect::<Vec<_>>(),
            fresh_output_rows
                .iter()
                .map(|row| (
                    row.raw_ordinal,
                    row.provider_event.event_type,
                    row.normalized_body_hash.as_str()
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(append.pro_outputs.len(), fresh.pro_outputs.len());
        for (append_output, fresh_output) in append.pro_outputs.iter().zip(&fresh.pro_outputs) {
            assert_eq!(append_output.kind, fresh_output.kind);
            assert_eq!(append_output.coordinate, fresh_output.coordinate);
            assert_eq!(
                append_output.occurred_at_unix_ms,
                fresh_output.occurred_at_unix_ms
            );
            assert_eq!(append_output.associations, fresh_output.associations);
            assert_eq!(append_output.call_id, fresh_output.call_id);
            assert_eq!(append_output.command, fresh_output.command);
            assert_eq!(append_output.outcome, fresh_output.outcome);
            assert_eq!(append_output.locator, fresh_output.locator);
            assert_eq!(append_output.content, fresh_output.content);
        }
        assert_eq!(append_scan.rejections, fresh_scan.rejections);
        assert_eq!(append_scan.counters.bytes_read, complete.len() as u64);
        assert_eq!(
            append_scan.counters.checkpoint_validation_bytes,
            initial.len() as u64
        );
        assert_eq!(
            append_scan.complete_prefix_sha256,
            fresh_scan.complete_prefix_sha256
        );
        assert_eq!(append_scan.next_raw_ordinal, fresh_scan.next_raw_ordinal);

        if profile == CodexNativeProfile::CoreOnly {
            assert!(append.pro_outputs.is_empty());
        } else {
            assert_eq!(
                append
                    .pro_outputs
                    .iter()
                    .map(|output| output.outcome.outcome)
                    .collect::<Vec<_>>(),
                vec![
                    crate::OutputOutcome::Success,
                    crate::OutputOutcome::Failure,
                    crate::OutputOutcome::Timeout,
                    crate::OutputOutcome::Unknown,
                ]
            );
            assert!(append.pro_outputs[3].content.is_empty());
            for output in &append.pro_outputs {
                assert_eq!(output.kind, crate::OutputObservationKind::Command);
                let command = output.command.as_ref().unwrap();
                assert_eq!(command.tool_name, "exec_command");
                assert_eq!(command.command, "printf retained");
                assert_eq!(command.working_directory.as_deref(), Some("/workspace"));
            }
        }
    }
}

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

#[test]
fn byte_overflow_restores_the_record_and_emits_it_once_on_the_next_page() {
    const OUTPUT_BYTES: usize = 2_100_000;

    let mut contents = session_meta("byte-page-owner");
    for index in 0..3 {
        contents.push_str(&successful_tool_output(
            &format!("large-{index}"),
            &char::from(b'a' + index).to_string().repeat(OUTPUT_BYTES),
        ));
    }
    let (_temp, path) = write_source(&contents);
    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "byte-page-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (scan, collected) = scan_collect_profile(
        discover_one(&path, "byte-page-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert_eq!(core.rows, collected.rows);
    assert_eq!(core.pages, collected.pages);
    assert_eq!(core.physical_records, collected.physical_records);
    assert_eq!(core.frontiers, collected.frontiers);
    assert_eq!(core.core_receipts, collected.core_receipts);
    assert_eq!(core_scan.rejections, scan.rejections);
    assert_eq!(collected.pages.len(), 1);
    assert_eq!(collected.pro_pages.len(), 2);
    assert_eq!(
        collected
            .pro_outputs
            .iter()
            .map(|output| output.coordinate.native_sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(collected
        .pro_pages
        .iter()
        .all(|(_, bytes)| *bytes <= MAX_CODEX_PAGE_BYTES));
    assert_eq!(
        collected.pro_frontiers[0].1, collected.pro_frontiers[1].0,
        "the overflow output must begin the next independent Pro page"
    );
    assert_eq!(scan.counters.complete_records, 4);
    assert_eq!(scan.counters.bytes_read, contents.len() as u64);
    assert_eq!(scan.next_raw_ordinal, 4);
    assert_eq!(scan.counters.structural_json_parses, 4);
    assert_eq!(scan.counters.structural_output_probes, 3);
    assert_eq!(scan.counters.typed_json_parses, 4);
    assert_eq!(scan.counters.typed_output_parses, 3);
}

#[test]
fn core_page_receipts_are_activation_invariant_at_unit_and_pro_byte_pressure() {
    let mut contents = session_meta("activation-owner");
    for index in 0..130 {
        if index % 3 == 0 {
            contents.push_str(&successful_tool_output(
                &format!("activation-{index}"),
                &format!("output-{index}-{}", "x".repeat(150_000)),
            ));
        } else {
            contents.push_str(&message("assistant", &format!("core-{index}")));
        }
    }
    let (_temp, path) = write_source(&contents);

    let (core_scan, core) = scan_collect_profile(
        discover_one(&path, "activation-owner"),
        None,
        CodexNativeProfile::CoreOnly,
    );
    let (pro_scan, pro) = scan_collect_profile(
        discover_one(&path, "activation-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );

    assert_eq!(core.rows, pro.rows);
    assert_eq!(core.pages, pro.pages);
    assert_eq!(core.physical_records, pro.physical_records);
    assert_eq!(core.frontiers, pro.frontiers);
    assert_eq!(core.core_receipts, pro.core_receipts);
    assert_eq!(core_scan.rejections, pro_scan.rejections);
    assert_eq!(core.pages.len(), 3);
    assert_eq!(core.physical_records, vec![64, 64, 3]);
    assert!(pro.pro_pages.len() >= 2);
    assert!(pro
        .pro_pages
        .iter()
        .all(|(units, bytes)| *units <= 64 && *bytes <= MAX_CODEX_PAGE_BYTES));
    assert_eq!(
        pro_scan.counters.pro_output_pages_emitted as usize,
        pro.pro_pages.len()
    );
}

#[test]
fn owned_page_can_retry_a_lagging_lane_before_the_scanner_advances() {
    let contents = [
        session_meta("retry-owner"),
        message("user", "request"),
        successful_tool_output("retry-call", "retry-output"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let mut scanner = CodexNativeScanner::new(
        discover_one(&path, "retry-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    )
    .unwrap();

    let pro_page = match scanner.next_page().unwrap().unwrap() {
        CodexNativeOwnedPage::Pro(page) => page,
        CodexNativeOwnedPage::Core(_) => panic!("Pro lane should flush before terminal Core"),
    };
    let first_attempt = pro_page
        .outputs
        .iter()
        .map(|output| {
            (
                output.coordinate.unit_key.clone(),
                output.coordinate.native_sequence,
                output.content.clone(),
            )
        })
        .collect::<Vec<_>>();
    let retry_attempt = pro_page
        .outputs
        .iter()
        .map(|output| {
            (
                output.coordinate.unit_key.clone(),
                output.coordinate.native_sequence,
                output.content.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(first_attempt, retry_attempt);
    let page = match scanner.next_page().unwrap().unwrap() {
        CodexNativeOwnedPage::Core(page) => page,
        CodexNativeOwnedPage::Pro(_) => panic!("only one Pro page should be emitted"),
    };
    assert_eq!(page.physical_records, 3);
    assert_eq!(
        page.next_safe_frontier.next_raw_ordinal,
        page.expected_frontier.next_raw_ordinal + page.physical_records
    );
    assert!(page.terminal);
    assert!(scanner.next_page().unwrap().is_none());
    let scan = scanner.finish().unwrap();
    assert_eq!(scan.counters.complete_records, 3);
    assert_eq!(scan.counters.emitted_pages, 1);
    assert_eq!(scan.counters.pro_output_pages_emitted, 1);
}

#[test]
fn terminal_authority_rejects_mutation_and_retry_keeps_the_safe_prefix_identity() {
    let initial = [
        session_meta("mutation-owner"),
        message("user", "before mutation"),
        successful_tool_output("mutation-call", "stable output"),
    ]
    .concat();
    let (_temp, path) = write_source(&initial);
    let mut scanner = CodexNativeScanner::new(
        discover_one(&path, "mutation-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    )
    .unwrap();
    let mut safe_frontier = None;
    while let Some(page) = scanner.next_page().unwrap() {
        if let CodexNativeOwnedPage::Core(page) = page {
            assert!(page.terminal);
            safe_frontier = Some(page.next_safe_frontier);
        }
    }
    let safe_frontier = safe_frontier.expect("terminal Core page");

    let appended = message("assistant", "after mutation");
    fs::write(&path, format!("{initial}{appended}")).unwrap();
    let error = scanner.finish().unwrap_err();
    assert!(format!("{error}").contains("catalog observation changed"));

    let (retry_scan, retry) = scan_collect_profile(
        discover_one(&path, "mutation-owner"),
        None,
        CodexNativeProfile::CoreAndPro,
    );
    assert_eq!(
        retry.frontiers[0].0,
        CodexNativeFrontier {
            complete_prefix_end: 0,
            next_raw_ordinal: 0,
            complete_prefix_sha256: Sha256::digest([]).into(),
        }
    );
    assert_eq!(safe_frontier.next_raw_ordinal, 3);
    assert_eq!(
        retry
            .rows
            .iter()
            .map(|row| row.raw_ordinal)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(retry_scan.next_raw_ordinal, 4);
}

#[test]
fn c0_shapes_retain_conversation_summaries_and_calls_but_no_results() {
    let mut baseline = session_meta("c0-baseline");
    for index in 0_usize..11 {
        baseline.push_str(&message(
            if index.is_multiple_of(3) {
                "user"
            } else {
                "assistant"
            },
            &format!("message-{index}"),
        ));
    }
    for index in 0..3 {
        baseline.push_str(&reasoning(&format!("reasoning-{index}")));
        baseline.push_str(&tool_call(&format!("call-{index}")));
        baseline.push_str(&tool_output(&format!("call-{index}"), "excluded"));
    }
    let (_temp, path) = write_source(&baseline);
    let (scan, sink) = scan_collect(discover_one(&path, "c0-baseline"), None);
    assert_eq!(sink.rows.len(), 17);
    assert_eq!(scan.counters.retained_records, 17);
    assert_eq!(scan.counters.native_result_records, 3);
    assert_eq!(
        sink.rows
            .iter()
            .filter(|row| row.provider_event.event_type == EventType::Message)
            .count(),
        11
    );
    assert_eq!(
        sink.rows
            .iter()
            .filter(|row| row.provider_event.event_type == EventType::Summary)
            .count(),
        3
    );
    assert_eq!(
        sink.rows
            .iter()
            .filter(|row| row.provider_event.event_type == EventType::ToolCall)
            .count(),
        3
    );

    let mut output_heavy = session_meta("c0-output-heavy");
    for index in 0_usize..4 {
        output_heavy.push_str(&message(
            if index.is_multiple_of(2) {
                "user"
            } else {
                "assistant"
            },
            &format!("conversation-{index}"),
        ));
    }
    for index in 0..8 {
        output_heavy.push_str(&tool_call(&format!("heavy-call-{index}")));
        output_heavy.push_str(&tool_output(
            &format!("heavy-call-{index}"),
            &"excluded-result".repeat(128),
        ));
    }
    fs::write(&path, output_heavy).unwrap();
    let (scan, sink) = scan_collect(discover_one(&path, "c0-output-heavy"), None);
    assert_eq!(sink.rows.len(), 12);
    assert_eq!(scan.counters.native_result_records, 8);
    assert_eq!(scan.counters.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(scan.counters.result_hashes_created, 0);
}

#[test]
fn compacted_payloads_and_future_result_aliases_are_fail_closed() {
    let contents = [
        session_meta("shape-owner"),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:01Z",
            "type": "compacted",
            "payload": [{"summary_text": "compacted summary"}]
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "tool_result",
                "result": "must not survive"
            }
        })),
        jsonl(json!({
            "timestamp": "2026-01-01T00:00:03Z",
            "type": "event_msg",
            "payload": {
                "type": "patch_apply_end",
                "stdout": "must not survive either"
            }
        })),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "shape-owner"), None);

    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.rows[0].provider_event.event_type, EventType::Summary);
    assert_eq!(sink.rows[0].raw_ordinal, 1);
    assert_eq!(scan.counters.native_result_records, 2);
    assert_eq!(scan.counters.retained_json_parses, 1);
}

#[test]
fn incomplete_tail_stays_at_its_starting_boundary_and_ordinal() {
    let complete = [session_meta("tail-owner"), message("user", "complete")].concat();
    let partial = r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","output":"partial"#;
    let contents = format!("{complete}{partial}");
    let (_temp, path) = write_source(&contents);
    let source = discover_one(&path, "tail-owner");
    let (scan, sink) = scan_collect(source, None);
    let tail = scan.incomplete_tail.as_ref().unwrap();

    assert_eq!(sink.rows.len(), 1);
    assert_eq!(scan.next_raw_ordinal, 2);
    assert_eq!(tail.raw_ordinal, 2);
    assert_eq!(tail.start_byte, complete.len() as u64);
    assert_eq!(tail.byte_len, partial.len() as u64);
    assert_eq!(scan.complete_prefix_end, complete.len() as u64);
    assert_eq!(
        sink.frontiers.last().unwrap().1.complete_prefix_end,
        complete.len() as u64
    );
    assert_eq!(
        sink.frontiers.last().unwrap().1.complete_prefix_sha256,
        scan.complete_prefix_sha256
    );
    assert!(!scan.terminal());
    assert_eq!(scan.counters.native_result_records, 0);
    assert_eq!(scan.counters.incomplete_records, 1);

    let proof = scan
        .bind_checkpoint("canonical-tail", CodexCheckpointGeneration::new(4))
        .unwrap()
        .unwrap();
    let (replay, replay_sink) = scan_collect(discover_one(&path, "tail-owner"), Some(&proof));
    assert_eq!(replay.disposition, CodexParseDisposition::ObservationReplay);
    assert_eq!(replay.incomplete_tail.as_ref(), Some(tail));
    assert!(replay_sink.rows.is_empty());
    assert_eq!(
        replay.counters.checkpoint_validation_bytes,
        contents.len() as u64
    );
}

#[test]
fn append_resumes_at_complete_prefix_and_preserves_suffix_ordinal() {
    let initial = [session_meta("append-owner"), message("user", "first")].concat();
    let (_temp, path) = write_source(&initial);
    let first_source = discover_one(&path, "append-owner");
    let (first, _) = scan_collect(first_source, None);
    let proof = first
        .bind_checkpoint("canonical-append", CodexCheckpointGeneration::new(11))
        .unwrap()
        .unwrap();

    let appended = [tool_output("call-old", "excluded"), tool_call("call-new")].concat();
    fs::write(&path, format!("{initial}{appended}")).unwrap();
    let second_source = discover_one(&path, "append-owner");
    let (second, sink) = scan_collect(second_source, Some(&proof));

    assert_eq!(second.disposition, CodexParseDisposition::AppendDelta);
    assert!(second.prefix_proof_matches());
    assert_eq!(second.counters.prefix_bytes_read, initial.len() as u64);
    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.rows[0].raw_ordinal, 3);
    assert_eq!(sink.rows[0].provider_event.event_type, EventType::ToolCall);
    assert_eq!(second.next_raw_ordinal, 4);
    assert_eq!(second.counters.native_result_records, 1);
}

#[test]
fn append_restarts_at_an_incomplete_records_original_ordinal() {
    let complete_prefix = [
        session_meta("partial-append-owner"),
        message("user", "complete"),
    ]
    .concat();
    let completed_output = tool_output("partial-call", "excluded after completion");
    let split = completed_output.len() / 2;
    let initial = format!("{complete_prefix}{}", &completed_output[..split]);
    let (_temp, path) = write_source(&initial);
    let (first, _) = scan_collect(discover_one(&path, "partial-append-owner"), None);
    assert_eq!(first.next_raw_ordinal, 2);
    assert_eq!(first.incomplete_tail.as_ref().unwrap().raw_ordinal, 2);
    let proof = first
        .bind_checkpoint("canonical-partial", CodexCheckpointGeneration::new(12))
        .unwrap()
        .unwrap();

    fs::write(
        &path,
        format!(
            "{complete_prefix}{completed_output}{}",
            message("assistant", "after completed output")
        ),
    )
    .unwrap();
    let (appended, sink) = scan_collect(discover_one(&path, "partial-append-owner"), Some(&proof));

    assert_eq!(appended.disposition, CodexParseDisposition::AppendDelta);
    assert_eq!(
        appended.counters.prefix_bytes_read,
        complete_prefix.len() as u64
    );
    assert_eq!(appended.counters.native_result_records, 1);
    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.rows[0].raw_ordinal, 3);
    assert_eq!(appended.next_raw_ordinal, 4);
}

#[test]
fn malformed_complete_record_does_not_hide_later_valid_content() {
    let contents = [
        session_meta("recovery-owner"),
        "{not valid}\n".to_owned(),
        message("assistant", "survives"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let source = discover_one(&path, "recovery-owner");
    let (scan, sink) = scan_collect(source, None);

    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.rows[0].raw_ordinal, 2);
    assert_eq!(scan.counters.malformed_records, 1);
    assert_eq!(scan.rejections.len(), 1);
}

#[test]
fn lifecycle_classification_covers_replay_append_rewrite_truncation_and_replacement() {
    let baseline = [session_meta("life-owner"), message("user", "one")].concat();
    let (_temp, path) = write_source(&baseline);
    let (baseline_scan, _) = scan_collect(discover_one(&path, "life-owner"), None);
    let proof = baseline_scan
        .bind_checkpoint("canonical-a", CodexCheckpointGeneration::new(15))
        .unwrap()
        .unwrap();
    let known = known_source(true, proof.clone());

    let (replay, _) = scan_collect(discover_one(&path, "life-owner"), Some(&proof));
    assert!(matches!(
        classify_source_lifecycle(&replay, &[]),
        CodexSourceLifecycle::Replay { .. }
    ));

    fs::write(
        &path,
        format!("{baseline}{}", message("assistant", "appended")),
    )
    .unwrap();
    let (append, _) = scan_collect(discover_one(&path, "life-owner"), Some(&proof));
    assert!(matches!(
        classify_source_lifecycle(&append, &[]),
        CodexSourceLifecycle::Append { .. }
    ));

    let rewrite_text = [session_meta("life-owner"), message("user", "rewritten")].concat();
    fs::write(&path, rewrite_text).unwrap();
    let (rewrite, _) = scan_collect(discover_one(&path, "life-owner"), None);
    assert_eq!(rewrite.disposition, CodexParseDisposition::FullGeneration);
    assert!(matches!(
        classify_source_lifecycle(&rewrite, std::slice::from_ref(&known)),
        CodexSourceLifecycle::Rewrite { .. }
    ));

    fs::write(&path, session_meta("life-owner")).unwrap();
    let (truncation, _) = scan_collect(discover_one(&path, "life-owner"), None);
    assert_eq!(
        truncation.disposition,
        CodexParseDisposition::FullGeneration
    );
    assert!(matches!(
        classify_source_lifecycle(&truncation, std::slice::from_ref(&known)),
        CodexSourceLifecycle::Truncation { .. }
    ));

    let replacement_text = [
        session_meta("replacement-owner"),
        message("user", "replacement"),
    ]
    .concat();
    fs::write(&path, replacement_text).unwrap();
    let (replacement, _) = scan_collect(discover_one(&path, "replacement-owner"), None);
    assert!(matches!(
        classify_source_lifecycle(&replacement, std::slice::from_ref(&known)),
        CodexSourceLifecycle::Replacement { .. }
    ));
}

#[test]
fn exact_revision_at_new_locator_distinguishes_relocation_copy_and_ambiguity() {
    let contents = [session_meta("move-owner"), message("user", "same bytes")].concat();
    let (temp, original_path) = write_source(&contents);
    let (original_scan, _) = scan_collect(discover_one(&original_path, "move-owner"), None);
    let proof = original_scan
        .bind_checkpoint("canonical-a", CodexCheckpointGeneration::new(18))
        .unwrap()
        .unwrap();
    let moved_path = temp.path().join("moved.jsonl");
    fs::write(&moved_path, &contents).unwrap();
    let (moved_scan, _) = scan_collect(discover_one(&moved_path, "move-owner"), None);

    let unavailable = known_source(false, proof.clone());
    assert!(matches!(
        classify_source_lifecycle(&moved_scan, std::slice::from_ref(&unavailable)),
        CodexSourceLifecycle::Relocation { .. }
    ));

    let live = known_source(true, proof.clone());
    assert!(matches!(
        classify_source_lifecycle(&moved_scan, std::slice::from_ref(&live)),
        CodexSourceLifecycle::Copy { .. }
    ));

    let second_identity = CodexSourceIdentity::new(
        "canonical-b",
        temp.path().display().to_string(),
        temp.path().join("other.jsonl"),
    );
    let second = known_source(
        false,
        CodexAppendProof::new(
            second_identity.unwrap(),
            CodexCheckpointGeneration::new(19),
            proof.checkpoint.clone(),
        ),
    );
    assert!(matches!(
        classify_source_lifecycle(&moved_scan, &[unavailable, second]),
        CodexSourceLifecycle::AmbiguousRelocation { candidate_count: 2 }
    ));
}

#[test]
fn checkpoint_round_trip_contains_control_state_but_no_event_body() {
    let secret_call = jsonl(json!({
        "timestamp": "2026-01-01T00:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": "pending-checkpoint-call",
            "arguments": {
                "cmd": "COMMAND_CHECKPOINT_SECRET",
                "token": "ARGUMENT_CHECKPOINT_SECRET"
            }
        }
    }));
    let contents = [
        session_meta("checkpoint-owner"),
        message("user", "event body must stay out of checkpoint"),
        secret_call,
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, _) = scan_collect(discover_one(&path, "checkpoint-owner"), None);
    let checkpoint = scan.checkpoint().unwrap();
    let encoded = checkpoint.encode().unwrap();
    let wire = String::from_utf8(encoded.clone()).unwrap();

    assert!(!wire.contains("event body must stay out of checkpoint"));
    assert!(!wire.contains("pending-checkpoint-call"));
    assert!(!wire.contains("exec_command"));
    assert!(!wire.contains("printf retained"));
    assert!(!wire.contains("COMMAND_CHECKPOINT_SECRET"));
    assert!(!wire.contains("ARGUMENT_CHECKPOINT_SECRET"));
    assert!(!wire.contains("command"));
    assert!(!wire.contains("arguments_preview"));
    let decoded_wire = serde_json::from_str::<Value>(&wire).unwrap();
    assert_eq!(decoded_wire["version"], 4);
    assert_eq!(
        decoded_wire["pending_tool_authorities"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(CodexNativeCheckpoint::decode(&encoded).unwrap(), checkpoint);

    let mut old_version = decoded_wire.clone();
    old_version["version"] = json!(2);
    assert!(CodexNativeCheckpoint::decode(&serde_json::to_vec(&old_version).unwrap()).is_err());

    let mut oversized_contexts = decoded_wire;
    let contexts = oversized_contexts["pending_tool_authorities"]
        .as_array_mut()
        .unwrap();
    let context = contexts.first().unwrap().clone();
    for index in 0..25 {
        let mut context = context.clone();
        context["raw_ordinal"] = json!(100 + index);
        context["record_start"] = json!(100 + index);
        context["record_end"] = json!(101 + index);
        contexts.push(context);
    }
    assert!(
        CodexNativeCheckpoint::decode(&serde_json::to_vec(&oversized_contexts).unwrap()).is_err()
    );
}

#[test]
fn terminal_checkpoint_boundary_tamper_rejects_during_decode() {
    let contents = [
        session_meta("terminal-tamper-owner"),
        message("user", "complete"),
    ]
    .concat();
    let (_temp, path) = write_source(&contents);
    let (scan, _) = scan_collect(discover_one(&path, "terminal-tamper-owner"), None);
    let checkpoint = scan.checkpoint().unwrap();
    let mut wire = serde_json::from_slice::<Value>(&checkpoint.encode().unwrap()).unwrap();

    wire["boundary"]["complete_eof"] = json!(contents.len() as u64 - 1);
    let tampered = serde_json::to_vec(&wire).unwrap();
    assert!(CodexNativeCheckpoint::decode(&tampered).is_err());
}

#[test]
fn unchanged_replay_revalidates_raw_ordinal_boundary_and_digests() {
    let complete = [
        session_meta("checkpoint-validation-owner"),
        message("user", "complete"),
    ]
    .concat();
    let partial = r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","output":"partial"#;
    let contents = format!("{complete}{partial}");
    let (_temp, path) = write_source(&contents);
    let (scan, _) = scan_collect(discover_one(&path, "checkpoint-validation-owner"), None);
    let checkpoint = scan.checkpoint().unwrap();
    let encoded = checkpoint.encode().unwrap();

    let mut bad_length = serde_json::from_slice::<Value>(&encoded).unwrap();
    bad_length["boundary"]["incomplete_tail_len"] = json!(0);
    assert!(CodexNativeCheckpoint::decode(&serde_json::to_vec(&bad_length).unwrap()).is_err());

    let mut bad_boundary = serde_json::from_slice::<Value>(&encoded).unwrap();
    bad_boundary["boundary"]["complete_prefix_end"] = json!(complete.len() as u64 - 1);
    bad_boundary["boundary"]["incomplete_tail_len"] = json!(partial.len() as u64 + 1);
    let decoded_boundary =
        CodexNativeCheckpoint::decode(&serde_json::to_vec(&bad_boundary).unwrap()).unwrap();
    assert_checkpoint_replay_rejected(&path, "checkpoint-validation-owner", decoded_boundary);

    let mut bad_ordinal = serde_json::from_slice::<Value>(&encoded).unwrap();
    bad_ordinal["complete_record_count"] = json!(99);
    let decoded_ordinal =
        CodexNativeCheckpoint::decode(&serde_json::to_vec(&bad_ordinal).unwrap()).unwrap();
    assert_checkpoint_replay_rejected(&path, "checkpoint-validation-owner", decoded_ordinal);

    let mut bad_digest = serde_json::from_slice::<Value>(&encoded).unwrap();
    bad_digest["boundary"]["incomplete_tail_sha256"][0] = json!(255);
    let decoded_digest =
        CodexNativeCheckpoint::decode(&serde_json::to_vec(&bad_digest).unwrap()).unwrap();
    assert_checkpoint_replay_rejected(&path, "checkpoint-validation-owner", decoded_digest);
}

#[test]
fn append_proof_cannot_cross_canonical_locator_identity() {
    let contents = [
        session_meta("proof-owner"),
        message("user", "same physical bytes"),
    ]
    .concat();
    let (temp, first_path) = write_source(&contents);
    let second_path = temp.path().join("second.jsonl");
    fs::write(&second_path, &contents).unwrap();

    let (first, _) = scan_collect(discover_one(&first_path, "proof-owner"), None);
    let proof = first
        .bind_checkpoint("canonical-proof-a", CodexCheckpointGeneration::new(73))
        .unwrap()
        .unwrap();
    assert_eq!(proof.generation.get(), 73);
    assert_eq!(proof.identity.canonical_source_key, "canonical-proof-a");
    assert_eq!(proof.identity.locator, first_path);

    let error = CodexNativeScanner::new(
        discover_one(&second_path, "proof-owner"),
        Some(&proof),
        CodexNativeProfile::CoreOnly,
    )
    .unwrap_err();
    assert!(format!("{error}").contains("does not belong to catalog source"));
}

#[test]
fn retained_rows_stream_in_pages_bounded_by_64_units_and_8_mib() {
    let mut contents = session_meta("paged-owner");
    for index in 0..5_001 {
        contents.push_str(&message("assistant", &format!("bounded-row-{index}")));
    }
    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "paged-owner"), None);

    assert_eq!(sink.rows.len(), 5_001);
    assert_eq!(sink.pages.len(), 79);
    assert!(sink
        .pages
        .iter()
        .all(|(units, bytes)| *units <= 64 && *bytes <= MAX_CODEX_PAGE_BYTES));
    assert!(sink.physical_records.iter().all(|records| *records <= 64));
    assert_eq!(scan.counters.retained_records, 5_001);
    assert_eq!(scan.counters.emitted_pages, 79);
    assert_eq!(scan.counters.peak_page_rows, MAX_CODEX_PAGE_ROWS);
    assert!(scan.counters.peak_page_bytes <= MAX_CODEX_PAGE_BYTES);
    assert_eq!(scan.next_raw_ordinal, 5_002);
}

#[test]
fn records_over_16_mib_are_stream_skipped_without_losing_physical_ordinals() {
    let mut contents = session_meta("oversized-owner");
    contents.push_str(
        r#"{"timestamp":"2026-01-01T00:00:02Z","type":"response_item","payload":{"type":"function_call_output","call_id":"big-output","output":""#,
    );
    contents.push_str(&"x".repeat(MAX_CODEX_RECORD_BYTES));
    contents.push_str("\"}}\n");
    contents.push('{');
    contents.push_str(&"y".repeat(MAX_CODEX_RECORD_BYTES));
    contents.push_str("}\n");
    contents.push_str(&message("assistant", "survives oversized records"));

    let (_temp, path) = write_source(&contents);
    let (scan, sink) = scan_collect(discover_one(&path, "oversized-owner"), None);

    assert_eq!(sink.rows.len(), 1);
    assert_eq!(sink.rows[0].raw_ordinal, 3);
    assert_eq!(scan.next_raw_ordinal, 4);
    assert_eq!(scan.counters.complete_records, 4);
    assert_eq!(scan.counters.oversized_records, 2);
    assert_eq!(scan.counters.peak_line_buffer_bytes, MAX_CODEX_RECORD_BYTES);
    assert_eq!(scan.counters.bytes_read, contents.len() as u64);
    assert_eq!(
        scan.rejections
            .iter()
            .map(|rejection| rejection.raw_ordinal)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(scan.counters.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(scan.counters.result_hashes_created, 0);
    assert_eq!(scan.counters.result_previews_created, 0);
}

#[test]
#[ignore = "diagnostic release-mode benchmark over the 154 MB Codex fixture"]
fn core_only_quickbench_guards_the_nativepath_parser_hot_path() {
    const EXPECTED_FILES: usize = 6_000;
    const EXPECTED_BYTES: u64 = 154_299_600;
    const EXPECTED_SHA256: &str =
        "b8558416ccb9719c5c8e0e3e1821ea94bef1e5c413a3070b9982fa759493e82b";
    const EXPECTED_ROWS: u64 = 24_000;
    const EXPECTED_RESULTS: u64 = 6_000;
    const EXPECTED_MALFORMED: u64 = 60;
    const EXPECTED_INCOMPLETE_TAILS: u64 = 60;

    let fixture_root = std::env::var_os("CTX_CODEX_QUICKBENCH_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/ctx-codex-nativepath-quickbench-v1"));
    let paths = quickbench_fixture_paths(&fixture_root);
    assert_eq!(paths.len(), EXPECTED_FILES);
    let (fixture_bytes, fixture_sha256) = quickbench_fixture_hash(&fixture_root, &paths);
    assert_eq!(fixture_bytes, EXPECTED_BYTES);
    assert_eq!(fixture_sha256, EXPECTED_SHA256);

    let catalog = paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let mut session = catalog_session(path, &format!("quickbench-{index:06}"));
            session.source_root = fixture_root.display().to_string();
            session.agent_type = if index.is_multiple_of(10) {
                AgentType::Subagent
            } else {
                AgentType::Primary
            };
            session
        })
        .collect::<Vec<_>>();
    let discovery = discover_codex_catalog_sources(&catalog);
    assert!(discovery.rejections.is_empty(), "{discovery:?}");
    assert_eq!(discovery.sources.len(), EXPECTED_FILES);
    let sources = discovery.sources;

    let scan_once = || {
        let mut rows = 0_u64;
        let mut results = 0_u64;
        let mut malformed = 0_u64;
        let mut incomplete_tails = 0_u64;
        let mut structural_parses = 0_u64;
        let mut typed_parses = 0_u64;
        let mut structural_output_probes = 0_u64;
        let mut typed_output_parses = 0_u64;
        for source in &sources {
            let mut scanner =
                CodexNativeScanner::new(source.clone(), None, CodexNativeProfile::CoreOnly)
                    .unwrap();
            while let Some(page) = scanner.next_page().unwrap() {
                match &page {
                    CodexNativeOwnedPage::Core(page) => {
                        rows = rows.saturating_add(page.core_rows.len() as u64);
                    }
                    CodexNativeOwnedPage::Pro(_) => {
                        panic!("CoreOnly must not emit transient Pro pages");
                    }
                }
                black_box(page);
            }
            let scan = scanner.finish().unwrap();
            results = results.saturating_add(scan.counters.native_result_records);
            malformed = malformed.saturating_add(scan.counters.malformed_records);
            incomplete_tails =
                incomplete_tails.saturating_add(u64::from(scan.incomplete_tail.is_some()));
            structural_parses =
                structural_parses.saturating_add(scan.counters.structural_json_parses);
            typed_parses = typed_parses.saturating_add(scan.counters.typed_json_parses);
            structural_output_probes =
                structural_output_probes.saturating_add(scan.counters.structural_output_probes);
            typed_output_parses =
                typed_output_parses.saturating_add(scan.counters.typed_output_parses);
        }
        assert_eq!(rows, EXPECTED_ROWS);
        assert_eq!(results, EXPECTED_RESULTS);
        assert_eq!(malformed, EXPECTED_MALFORMED);
        assert_eq!(incomplete_tails, EXPECTED_INCOMPLETE_TAILS);
        assert_eq!(structural_parses, 36_060);
        assert_eq!(typed_parses, 30_000);
        assert_eq!(structural_output_probes, EXPECTED_RESULTS);
        assert_eq!(typed_output_parses, 0);
        black_box((
            rows,
            results,
            malformed,
            incomplete_tails,
            structural_parses,
            typed_parses,
        ));
    };

    scan_once();
    let mut samples = Vec::with_capacity(3);
    for _ in 0..3 {
        let started = Instant::now();
        scan_once();
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let median = samples[1];
    println!(
        "Codex NativePath CoreOnly median over {} bytes and {} sources: {:.3}s",
        EXPECTED_BYTES,
        EXPECTED_FILES,
        median.as_secs_f64()
    );
    assert!(
        median.as_secs_f64() < 1.0,
        "obvious NativePath parser regression from the recorded 0.468s behavior: {median:?}"
    );
}

fn quickbench_fixture_paths(fixture_root: &Path) -> Vec<PathBuf> {
    let mut directories = vec![fixture_root.join("sessions")];
    let mut paths = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let file_type = entry.file_type().unwrap();
            if file_type.is_dir() {
                directories.push(entry.path());
            } else if file_type.is_file()
                && entry.path().extension().and_then(|value| value.to_str()) == Some("jsonl")
            {
                paths.push(entry.path());
            }
        }
    }
    paths.sort();
    paths
}

fn quickbench_fixture_hash(fixture_root: &Path, paths: &[PathBuf]) -> (u64, String) {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-codex-nativepath-quickbench-fixture-v1\0");
    let mut byte_size = 0_u64;
    for path in paths {
        let relative = path.strip_prefix(fixture_root).unwrap().to_string_lossy();
        let bytes = fs::read(path).unwrap();
        hasher.update((relative.len() as u64).to_be_bytes());
        hasher.update(relative.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
        byte_size = byte_size.saturating_add(bytes.len() as u64);
    }
    let digest = hasher.finalize();
    let sha256 = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    (byte_size, sha256)
}

fn assert_checkpoint_replay_rejected(
    path: &Path,
    native_session_id: &str,
    checkpoint: CodexNativeCheckpoint,
) {
    let identity = CodexSourceIdentity::new(
        "canonical-tampered",
        path.parent().unwrap().display().to_string(),
        path.to_path_buf(),
    )
    .unwrap();
    let proof = CodexAppendProof::new(identity, CodexCheckpointGeneration::new(88), checkpoint);
    let error = CodexNativeScanner::new(
        discover_one(path, native_session_id),
        Some(&proof),
        CodexNativeProfile::CoreOnly,
    )
    .unwrap_err();
    assert!(format!("{error}").contains("invalid Codex append proof"));
}

fn known_source(route_live: bool, proof: CodexAppendProof) -> CodexKnownSource {
    CodexKnownSource { proof, route_live }
}
