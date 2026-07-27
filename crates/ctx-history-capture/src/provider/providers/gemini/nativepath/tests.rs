use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};
use tempfile::TempDir;

use super::discover_gemini_transcripts;
use super::discovery::{discover_gemini_transcripts_with_limits, DiscoveryBudget};
use super::dto::{
    GeminiCompleteness, GeminiEventBody, GeminiEventIdentity, GeminiNativePathProfile,
    GeminiPreviousSource, GeminiPublicationShape, GeminiRejectionKind, GeminiRetainedEvent,
    GeminiScanError, GeminiScanOutcome, GeminiSourceChange, GeminiTranscriptLayout,
    GeminiTranscriptSource,
};
use super::parser::{
    gemini_parse_counters, gemini_resume_work_counters, read_gemini_transcript_pages,
    read_gemini_transcript_pages_from_frontier, read_gemini_transcript_pages_with_profile,
    reset_gemini_parse_counters, GeminiNativeEventIds, MAX_GEMINI_FILE_TOUCHES_PER_EVENT,
    MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT, MAX_GEMINI_NATIVE_PAGE_BYTES,
    MAX_GEMINI_NATIVE_PAGE_RECORDS,
};
use crate::provider::providers::native_jsonl::result_content::{
    extract_native_jsonl_result_content, gemini_result_subrecord_oracle_for_tests,
    NativeJsonlResultExtractionError, GEMINI_RESULT_PROFILE,
};
use crate::{
    CaptureError, OutputOutcome, MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
};

fn fixture_root(temp: &TempDir) -> PathBuf {
    temp.path().join(".gemini")
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("tmp/project/chats/session-root.jsonl")
}

fn header(session_id: &str, kind: &str) -> Value {
    json!({
        "sessionId": session_id,
        "startTime": "2026-01-01T00:00:00.000Z",
        "lastUpdated": "2026-01-01T00:00:00.000Z",
        "kind": kind,
        "directories": ["/workspace/project"]
    })
}

fn jsonl(values: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in values {
        serde_json::to_writer(&mut bytes, value).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

fn write_transcript(root: &Path, values: &[Value]) -> PathBuf {
    let path = transcript_path(root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, jsonl(values)).unwrap();
    path
}

fn rediscover(root: &Path, expected_path: &Path) -> GeminiTranscriptSource {
    discover_gemini_transcripts(root)
        .unwrap()
        .transcripts
        .into_iter()
        .find(|source| source.path == fs::canonicalize(expected_path).unwrap())
        .unwrap()
}

fn scan_collect(
    source: &GeminiTranscriptSource,
    previous: Option<&GeminiPreviousSource>,
) -> (GeminiScanOutcome, Vec<GeminiRetainedEvent>) {
    let mut reader = read_gemini_transcript_pages(source, previous).unwrap();
    let mut rows = Vec::new();
    while let Some(page) = reader.next_page().unwrap() {
        rows.extend(page.events);
    }
    let outcome = reader.outcome().cloned().unwrap();
    (outcome, rows)
}

fn previous(outcome: &GeminiScanOutcome, prior_route_still_live: bool) -> GeminiPreviousSource {
    GeminiPreviousSource {
        checkpoint: outcome.checkpoint.clone(),
        prior_route_still_live,
    }
}

#[test]
fn gemini_nativepath_discovers_only_exact_chat_layout_in_stable_order() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let chats = root.join("tmp/project/chats");
    fs::create_dir_all(chats.join("root-session")).unwrap();
    fs::create_dir_all(root.join("tmp/project/telemetry")).unwrap();
    fs::write(chats.join("z-primary.jsonl"), "{}\n").unwrap();
    fs::write(chats.join("root-session/a-child.jsonl"), "{}\n").unwrap();
    fs::write(root.join("tmp/project/telemetry/noise.log"), "{}\n").unwrap();

    let discovery = discover_gemini_transcripts(&root).unwrap();

    assert!(discovery.completed_inventory);
    assert_eq!(discovery.transcripts.len(), 2);
    assert!(discovery.transcripts[0].path < discovery.transcripts[1].path);
    assert!(matches!(
        discovery.transcripts[0].layout,
        GeminiTranscriptLayout::Subagent {
            ref parent_native_session_id_hint
        } if parent_native_session_id_hint == "root-session"
    ));
    assert_eq!(
        discovery.transcripts[1].layout,
        GeminiTranscriptLayout::Primary
    );
    assert_ne!(discovery.inventory_sha256, [0; 32]);
}

#[test]
fn gemini_nativepath_rejects_extra_nesting_and_layout_lookalikes() {
    for relative_path in [
        "tmp/project/extra/chats/session.jsonl",
        "tmp/project/chat/session.jsonl",
        "tmp/project/chatsx/session.jsonl",
        "tmp/project/chats/parent/extra/session.jsonl",
        "tmp/noise/.gemini/tmp/project/chats/ghost.jsonl",
    ] {
        let temp = TempDir::new().unwrap();
        let root = fixture_root(&temp);
        let path = root.join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{}\n").unwrap();

        let error = discover_gemini_transcripts(&root).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(".gemini/tmp/<project>/chats/<session>.jsonl or one subagent directory"),
            "unexpected error for {relative_path}: {error}"
        );
    }
}

#[test]
fn gemini_nativepath_discovery_budgets_fail_at_the_exact_count_and_byte_boundaries() {
    let mut count_budget = DiscoveryBudget::with_limits(2, 1_024);
    count_budget.observe(Path::new("a")).unwrap();
    count_budget.observe(Path::new("b")).unwrap();
    let count_error = count_budget.observe(Path::new("c")).unwrap_err();
    assert!(count_error.to_string().contains("exceeds 2 entries"));

    let mut byte_budget = DiscoveryBudget::with_limits(10, 5);
    byte_budget.observe(Path::new("12345")).unwrap();
    let byte_error = byte_budget.observe(Path::new("x")).unwrap_err();
    assert!(byte_error.to_string().contains("exceeds 5 path bytes"));

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    write_transcript(&root, &[header("budget-session", "main")]);
    let integrated_error =
        discover_gemini_transcripts_with_limits(&root, 3, usize::MAX).unwrap_err();
    assert!(integrated_error.to_string().contains("exceeds 3 entries"));
}

#[test]
fn gemini_nativepath_discovery_handles_large_bounded_directories() {
    const NOISE_ENTRIES: usize = 2_000;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let noise = root.join("tmp/noise");
    fs::create_dir_all(&noise).unwrap();
    for index in 0..NOISE_ENTRIES {
        fs::write(noise.join(format!("{index:04}.log")), b"noise").unwrap();
    }
    let path = write_transcript(&root, &[header("bounded-discovery", "main")]);

    let discovery = discover_gemini_transcripts(&root).unwrap();

    assert_eq!(discovery.transcripts.len(), 1);
    assert_eq!(
        discovery.transcripts[0].path,
        fs::canonicalize(path).unwrap()
    );
}

#[test]
fn gemini_nativepath_completed_empty_inventory_is_an_explicit_zero_source_signal() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    fs::create_dir_all(root.join("tmp/project/chats")).unwrap();

    let discovery = discover_gemini_transcripts(&root).unwrap();

    assert!(discovery.completed_inventory);
    assert!(discovery.transcripts.is_empty());
    assert_ne!(discovery.inventory_sha256, [0; 32]);
}

#[test]
fn gemini_nativepath_preserves_nested_parent_identity_without_a_header_event() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = root.join("tmp/project/chats/root-session/child-session.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        jsonl(&[
            header("child-session", "subagent"),
            json!({
                "id": "child-user",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "child request"
            }),
        ]),
    )
    .unwrap();
    let source = rediscover(&root, &path);

    let (outcome, rows) = scan_collect(&source, None);

    let session = outcome.checkpoint.session.unwrap();
    assert_eq!(session.native_session_id, "child-session");
    assert_eq!(
        session.parent_native_session_id.as_deref(),
        Some("root-session")
    );
    assert_eq!(session.agent_type, ctx_history_core::AgentType::Subagent);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].native_order.raw_ordinal, 1);
    assert_eq!(outcome.metrics.header_records, 1);
}

#[cfg(unix)]
#[test]
fn gemini_nativepath_rejects_linked_inventory_components() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(root.join("tmp/project")).unwrap();
    symlink(&outside, root.join("tmp/project/chats")).unwrap();

    let error = discover_gemini_transcripts(&root).unwrap_err();
    assert!(error.to_string().contains("linked Gemini transcript"));
}

#[test]
fn gemini_nativepath_retains_core_rows_without_header_or_result_material() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let output_sentinel = "NATIVEPATH_SYNTHETIC_OUTPUT_GEMINI_PRIVATE";
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({
                "id": "user-1",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "hello Gemini"
            }),
            json!({
                "id": "assistant-1",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "gemini",
                "content": "hello user",
                "model": "gemini-test"
            }),
            json!({
                "id": "request-1",
                "timestamp": "2026-01-01T00:00:03.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "call-1",
                    "name": "write_file",
                    "args": {"path": "safe-request.txt", "content": "safe"}
                }]
            }),
            json!({
                "id": "result-1",
                "timestamp": "2026-01-01T00:00:04.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "call-1",
                    "name": "write_file",
                    "result": {
                        "content": output_sentinel,
                        "path": "/workspace/nativepath-fixture/output-only/leak.txt"
                    }
                }]
            }),
            json!({
                "id": "state-1",
                "timestamp": "2026-01-01T00:00:05.000Z",
                "$set": {"summary": "checkpoint state", "synthetic": true}
            }),
            json!({
                "id": "future-1",
                "timestamp": "2026-01-01T00:00:06.000Z",
                "type": "future_record",
                "content": "must not fabricate a notice"
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let (outcome, rows) = scan_collect(&source, None);

    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter()
            .map(|row| row.native_order.raw_ordinal)
            .collect::<Vec<_>>(),
        [1, 2, 3, 5]
    );
    assert_eq!(
        rows.iter().map(|row| row.event_type).collect::<Vec<_>>(),
        [
            EventType::Message,
            EventType::Message,
            EventType::ToolCall,
            EventType::Notice
        ]
    );
    assert_eq!(rows[0].role, EventRole::User);
    assert_eq!(rows[1].role, EventRole::Assistant);
    assert_eq!(
        rows[2].safe_file_touches,
        vec!["safe-request.txt".to_owned()]
    );
    assert!(matches!(rows[3].body, GeminiEventBody::StateNotice { .. }));
    assert!(rows.iter().all(|row| {
        !format!("{row:?}").contains(output_sentinel)
            && !row
                .safe_file_touches
                .iter()
                .any(|path| path.contains("output-only"))
    }));
    assert_eq!(outcome.metrics.header_records, 1);
    assert_eq!(outcome.metrics.native_result_records_observed, 1);
    assert!(outcome.metrics.native_result_record_bytes_observed > 0);
    assert_eq!(outcome.metrics.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(outcome.metrics.result_body_hashes_created, 0);
    assert_eq!(outcome.metrics.result_previews_created, 0);
    assert_eq!(outcome.metrics.result_file_touches_created, 0);
    assert_eq!(outcome.metrics.result_fts_documents_created, 0);
    assert_eq!(outcome.metrics.result_handoffs_created, 0);
    assert_eq!(outcome.checkpoint.retained_event_count, 4);
    assert_eq!(outcome.signals.source_change, GeminiSourceChange::Fresh);
    assert_eq!(
        outcome.signals.publication_shape,
        GeminiPublicationShape::AuthoritativeSnapshot
    );
    assert!(!outcome.signals.emitted_zero_rows);
    assert!(outcome.signals.cursor_advance_allowed);
}

#[test]
fn gemini_nativepath_malformed_record_is_local_and_incomplete_tail_is_nonterminal() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[
        header("root-session", "main"),
        json!({
            "id": "before",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": "before malformed"
        }),
    ]);
    bytes.extend_from_slice(b"{not-json}\n");
    bytes.extend_from_slice(&jsonl(&[json!({
        "id": "after",
        "timestamp": "2026-01-01T00:00:03.000Z",
        "type": "gemini",
        "content": "after malformed"
    })]));
    let complete_prefix_end = bytes.len() as u64;
    bytes.extend_from_slice(
        br#"{"id":"partial","timestamp":"2026-01-01T00:00:04.000Z","type":"gemini","content":"unfinished"#,
    );
    fs::write(&path, bytes).unwrap();
    let source = rediscover(&root, &path);

    let (outcome, rows) = scan_collect(&source, None);

    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .map(|row| row.native_order.raw_ordinal)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    assert_eq!(outcome.rejected_records, 1);
    assert_eq!(outcome.rejections.len(), 1);
    assert!(outcome.rejections[0]
        .reason
        .contains("malformed Gemini JSONL"));
    assert_eq!(outcome.checkpoint.complete_prefix_end, complete_prefix_end);
    assert_eq!(outcome.checkpoint.next_raw_ordinal, 4);
    assert!(!outcome.checkpoint.terminal);
    assert_eq!(
        outcome.signals.completeness,
        GeminiCompleteness::NonterminalCompletePrefix {
            end: complete_prefix_end
        }
    );
    assert!(outcome.signals.cursor_advance_allowed);
}

#[test]
fn gemini_nativepath_structural_rejections_advance_with_durable_detail_and_resume_exactly() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[header("structural-rejections", "main")]);
    bytes.extend_from_slice(b"{malformed-json}\n");
    bytes.extend_from_slice(&jsonl(&[json!({
        "id": "after-malformed",
        "timestamp": "2026-01-01T00:00:01.000Z",
        "type": "user",
        "content": "later sibling"
    })]));
    fs::write(&path, bytes).unwrap();
    let source = rediscover(&root, &path);

    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let page = reader.next_page().unwrap().unwrap();
    assert_eq!(page.rejections.len(), 1);
    assert_eq!(page.rejections[0].raw_ordinal, 1);
    assert!(matches!(
        page.rejections[0].kind,
        GeminiRejectionKind::InvalidRecord
    ));
    assert!(page.rejections[0].reason.contains("malformed Gemini JSONL"));
    assert_eq!(page.next_safe_frontier.rejected_records, 1);
    assert_eq!(page.next_safe_frontier.next_raw_ordinal, 3);
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].native_order.raw_ordinal, 2);
    let page_identity = page.identity;
    let expected_frontier = page.expected_frontier.clone();
    let committed_frontier = page.next_safe_frontier.clone();
    assert!(reader.next_page().unwrap().is_none());
    let outcome = reader.outcome().unwrap();
    assert_eq!(outcome.checkpoint.rejected_records, 1);
    assert!(outcome.signals.cursor_advance_allowed);

    let mut replay = read_gemini_transcript_pages_from_frontier(
        &source,
        &expected_frontier,
        GeminiNativePathProfile::CoreOnly,
    )
    .unwrap();
    let replayed_page = replay.next_page().unwrap().unwrap();
    assert_eq!(replayed_page.identity, page_identity);
    assert_eq!(replayed_page.rejections, page.rejections);
    assert_eq!(replayed_page.events, page.events);

    let mut after_commit = read_gemini_transcript_pages_from_frontier(
        &source,
        &committed_frontier,
        GeminiNativePathProfile::CoreOnly,
    )
    .unwrap();
    assert!(after_commit.next_page().unwrap().is_none());
    assert_eq!(after_commit.outcome().unwrap().rejected_records, 1);

    let mut oversized = jsonl(&[header("structural-rejections", "main")]);
    oversized.extend_from_slice(br#"{"payload":""#);
    oversized.extend(std::iter::repeat_n(
        b'x',
        MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(64),
    ));
    oversized.extend_from_slice(b"\"}\n");
    oversized.extend_from_slice(&jsonl(&[json!({
        "id": "after-oversized",
        "timestamp": "2026-01-01T00:00:02.000Z",
        "type": "gemini",
        "content": "still replayable"
    })]));
    fs::write(&path, oversized).unwrap();
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let page = reader.next_page().unwrap().unwrap();
    assert_eq!(page.rejections.len(), 1);
    assert_eq!(page.rejections[0].raw_ordinal, 1);
    assert!(page.rejections[0].reason.contains("byte limit"));
    assert_eq!(page.next_safe_frontier.rejected_records, 1);
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].native_order.raw_ordinal, 2);
    assert!(reader.next_page().unwrap().is_none());
    assert!(reader.outcome().unwrap().signals.cursor_advance_allowed);
}

#[test]
fn gemini_nativepath_hydration_failure_keeps_later_siblings_replayable() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("hydration-retry", "main"),
            json!({
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "missing native id"
            }),
            json!({
                "id": "later-valid",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "gemini",
                "content": "must not be skipped"
            }),
        ],
    );
    let header_end = u64::try_from(jsonl(&[header("hydration-retry", "main")]).len()).unwrap();
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();

    let safe_page = reader.next_page().unwrap().unwrap();
    assert!(safe_page.events.is_empty());
    assert!(safe_page.rejections.is_empty());
    assert!(!safe_page.terminal);
    assert_eq!(safe_page.next_safe_frontier.complete_prefix_end, header_end);
    assert_eq!(safe_page.next_safe_frontier.next_raw_ordinal, 1);
    assert_eq!(safe_page.next_safe_frontier.retained_event_count, 0);
    assert_eq!(safe_page.next_safe_frontier.rejected_records, 0);
    let safe_frontier = safe_page.next_safe_frontier;

    assert!(matches!(
        reader.next_page().unwrap_err(),
        GeminiScanError::UncommittedRecord {
            raw_ordinal: 1,
            ref reason,
            ..
        } if reason.contains("missing a nonempty native id")
    ));
    let mut same_input_retry = read_gemini_transcript_pages_from_frontier(
        &source,
        &safe_frontier,
        GeminiNativePathProfile::CoreOnly,
    )
    .unwrap();
    assert!(matches!(
        same_input_retry.next_page().unwrap_err(),
        GeminiScanError::UncommittedRecord { raw_ordinal: 1, .. }
    ));

    fs::write(
        &path,
        jsonl(&[
            header("hydration-retry", "main"),
            json!({
                "id": "corrected",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "corrected native id"
            }),
            json!({
                "id": "later-valid",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "gemini",
                "content": "must not be skipped"
            }),
        ]),
    )
    .unwrap();
    let corrected_source = rediscover(&root, &path);
    let mut corrected = read_gemini_transcript_pages_from_frontier(
        &corrected_source,
        &safe_frontier,
        GeminiNativePathProfile::CoreOnly,
    )
    .unwrap();
    let mut ids = Vec::new();
    while let Some(page) = corrected.next_page().unwrap() {
        ids.extend(page.events.into_iter().map(|event| match event.identity {
            GeminiEventIdentity::NativeRecordId(id) => id,
        }));
    }
    assert_eq!(ids, ["corrected", "later-valid"]);
    let outcome = corrected.outcome().unwrap();
    assert_eq!(outcome.checkpoint.next_raw_ordinal, 3);
    assert_eq!(outcome.checkpoint.retained_event_count, 2);
    assert_eq!(outcome.checkpoint.rejected_records, 0);
}

#[test]
fn gemini_nativepath_every_unterminated_final_record_stays_uncommitted() {
    let mut oversized = vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(64)];
    oversized.insert(0, b'{');
    let cases = [
        (
            "valid-json",
            serde_json::to_vec(&json!({
                "id": "committed-before-tail",
                "type": "user",
                "content": "valid but unterminated"
            }))
            .unwrap(),
        ),
        ("syntax-error", br#"{"id":"broken",]}"#.to_vec()),
        ("oversized", oversized),
    ];

    for (case, tail) in cases {
        let temp = TempDir::new().unwrap();
        let root = fixture_root(&temp);
        let path = transcript_path(&root);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let committed = jsonl(&[
            header(&format!("unterminated-{case}"), "main"),
            json!({
                "id": "committed-before-tail",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "committed"
            }),
        ]);
        let committed_end = u64::try_from(committed.len()).unwrap();
        let mut bytes = committed;
        bytes.extend_from_slice(&tail);
        fs::write(&path, bytes).unwrap();
        let source = rediscover(&root, &path);
        let mut reader = read_gemini_transcript_pages(&source, None).unwrap();

        let page = reader.next_page().unwrap().unwrap();
        assert!(!page.terminal, "{case}");
        assert_eq!(page.physical_records, 2, "{case}");
        assert_eq!(page.events.len(), 1, "{case}");
        assert_eq!(page.next_safe_frontier.complete_prefix_end, committed_end);
        assert_eq!(page.next_safe_frontier.next_raw_ordinal, 2);
        assert_eq!(page.next_safe_frontier.rejected_records, 0);
        assert!(reader.next_page().unwrap().is_none());
        let outcome = reader.outcome().unwrap();
        assert_eq!(page.terminal, outcome.checkpoint.terminal, "{case}");
        assert!(!outcome.checkpoint.terminal, "{case}");
        assert_eq!(outcome.checkpoint.complete_prefix_end, committed_end);
        assert_eq!(outcome.checkpoint.next_raw_ordinal, 2);
        assert_eq!(outcome.rejected_records, 0);
    }
}

#[test]
fn gemini_nativepath_resumes_at_incomplete_record_boundary_when_tail_completes() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = jsonl(&[
        header("root-session", "main"),
        json!({
            "id": "complete-user",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": "complete"
        }),
    ]);
    bytes.extend_from_slice(
        br#"{"id":"tail-assistant","timestamp":"2026-01-01T00:00:02.000Z","type":"gemini","content":"tail"#,
    );
    fs::write(&path, bytes).unwrap();
    let source = rediscover(&root, &path);
    let (incomplete, incomplete_rows) = scan_collect(&source, None);
    assert_eq!(incomplete_rows.len(), 1);
    assert!(!incomplete.checkpoint.terminal);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"\"}\n").unwrap();
    drop(file);
    let source = rediscover(&root, &path);
    let (completed, delta_rows) = scan_collect(&source, Some(&previous(&incomplete, true)));

    assert_eq!(completed.signals.source_change, GeminiSourceChange::Append);
    assert_eq!(
        completed.signals.publication_shape,
        GeminiPublicationShape::AppendDelta
    );
    assert!(completed.checkpoint.terminal);
    assert_eq!(delta_rows.len(), 1);
    assert_eq!(
        delta_rows[0].identity,
        GeminiEventIdentity::NativeRecordId("tail-assistant".to_owned())
    );
    assert_eq!(delta_rows[0].native_order.raw_ordinal, 2);
    assert_eq!(completed.checkpoint.retained_event_count, 2);
}

#[test]
fn gemini_nativepath_physical_growth_with_only_incomplete_bytes_is_append_delta() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({
                "id": "complete-user",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "complete"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (baseline, _) = scan_collect(&source, None);
    let boundary = baseline.checkpoint.complete_prefix_end;
    let boundary_hash = baseline.checkpoint.complete_prefix_sha256;

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(
        br#"{"id":"partial-append","timestamp":"2026-01-01T00:00:02.000Z","type":"gemini","content":"still incomplete"#,
    )
    .unwrap();
    drop(file);
    let source = rediscover(&root, &path);
    let (partial, partial_rows) = scan_collect(&source, Some(&previous(&baseline, true)));

    assert!(partial_rows.is_empty());
    assert_eq!(partial.signals.source_change, GeminiSourceChange::Append);
    assert_eq!(
        partial.signals.publication_shape,
        GeminiPublicationShape::AppendDelta
    );
    assert_eq!(
        partial.signals.completeness,
        GeminiCompleteness::NonterminalCompletePrefix { end: boundary }
    );
    assert_eq!(partial.checkpoint.complete_prefix_end, boundary);
    assert_eq!(partial.checkpoint.complete_prefix_sha256, boundary_hash);
    assert_eq!(
        partial.checkpoint.retained_event_count,
        baseline.checkpoint.retained_event_count
    );
    assert!(partial.checkpoint.append_boundary_safe);
    assert!(partial.signals.cursor_advance_allowed);
    assert!(partial.signals.content_changed);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"\"}\n").unwrap();
    drop(file);
    let source = rediscover(&root, &path);
    let (completed, completed_rows) = scan_collect(&source, Some(&previous(&partial, true)));

    assert_eq!(completed.signals.source_change, GeminiSourceChange::Append);
    assert_eq!(
        completed.signals.publication_shape,
        GeminiPublicationShape::AppendDelta
    );
    assert_eq!(completed_rows.len(), 1);
    assert_eq!(completed_rows[0].native_order.raw_ordinal, 2);
}

#[test]
fn gemini_nativepath_unchanged_and_append_emit_only_provider_native_delta() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({
                "id": "user-1",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "baseline"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (baseline, baseline_rows) = scan_collect(&source, None);
    let old_identity = baseline_rows[0].identity.clone();
    let old_order = baseline_rows[0].native_order;
    let previous = previous(&baseline, true);

    let source = rediscover(&root, &path);
    reset_gemini_parse_counters();
    let (unchanged, unchanged_rows) = scan_collect(&source, Some(&previous));
    assert!(unchanged_rows.is_empty());
    assert_eq!(
        gemini_resume_work_counters(),
        (0, baseline.checkpoint.complete_prefix_end)
    );
    assert_eq!(
        unchanged.signals.source_change,
        GeminiSourceChange::Unchanged
    );
    assert_eq!(
        unchanged.signals.publication_shape,
        GeminiPublicationShape::ObservationOnly
    );

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&jsonl(&[json!({
        "id": "assistant-2",
        "timestamp": "2026-01-01T00:00:02.000Z",
        "type": "gemini",
        "content": "appended"
    })]))
    .unwrap();
    drop(file);
    let source = rediscover(&root, &path);
    reset_gemini_parse_counters();
    let (append, append_rows) = scan_collect(&source, Some(&previous));

    assert_eq!(append.signals.source_change, GeminiSourceChange::Append);
    assert_eq!(
        gemini_resume_work_counters(),
        (1, baseline.checkpoint.complete_prefix_end)
    );
    assert_eq!(
        append.signals.publication_shape,
        GeminiPublicationShape::AppendDelta
    );
    assert_eq!(append_rows.len(), 1);
    assert_eq!(
        append_rows[0].identity,
        GeminiEventIdentity::NativeRecordId("assistant-2".to_owned())
    );
    assert_eq!(append_rows[0].native_order.raw_ordinal, 2);
    assert_eq!(append.checkpoint.retained_event_count, 2);
    assert_eq!(
        old_identity,
        GeminiEventIdentity::NativeRecordId("user-1".to_owned())
    );
    assert_eq!(old_order.raw_ordinal, 1);
}

#[test]
fn gemini_nativepath_repeated_appends_hash_the_prefix_but_parse_only_the_delta() {
    const APPENDS: usize = 96;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(&root, &[header("linear-append-work", "main")]);
    let source = rediscover(&root, &path);
    let (mut prior, baseline_rows) = scan_collect(&source, None);
    assert!(baseline_rows.is_empty());

    let mut total_record_reads = 0_u64;
    let mut total_prefix_bytes = 0_u64;
    for index in 0..APPENDS {
        let expected_prefix_bytes = prior.checkpoint.complete_prefix_end;
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&jsonl(&[json!({
                "id": format!("append-{index:03}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "content": format!("delta-{index:03}")
            })]))
            .unwrap();

        let source = rediscover(&root, &path);
        reset_gemini_parse_counters();
        let (next, rows) = scan_collect(&source, Some(&previous(&prior, true)));
        let (record_reads, prefix_bytes) = gemini_resume_work_counters();

        assert_eq!(next.signals.source_change, GeminiSourceChange::Append);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].native_order.raw_ordinal, index as u64 + 1);
        assert_eq!(record_reads, 1, "append {index} replayed prior records");
        assert_eq!(
            prefix_bytes, expected_prefix_bytes,
            "append {index} did not hash the complete committed prefix"
        );
        total_record_reads += record_reads;
        total_prefix_bytes += prefix_bytes;
        prior = next;
    }

    assert_eq!(total_record_reads, APPENDS as u64);
    assert!(total_prefix_bytes > prior.checkpoint.complete_prefix_end);
    assert_eq!(prior.checkpoint.next_raw_ordinal, APPENDS as u64 + 1);
    assert_eq!(prior.checkpoint.retained_event_count, APPENDS as u64);
}

#[test]
fn gemini_nativepath_full_prefix_hash_rejects_byte_zero_rewrite_with_preserved_tail() {
    const PRESERVED_TAIL_BYTES: usize = 64 * 1024;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut baseline_bytes = jsonl(&[
        header("full-prefix-proof", "main"),
        json!({
            "id": "early-event",
            "type": "user",
            "content": "early event remains semantically identical"
        }),
        json!({
            "id": "large-middle",
            "type": "gemini",
            "content": "m".repeat(PRESERVED_TAIL_BYTES + 8 * 1024)
        }),
        json!({
            "id": "tail-event",
            "type": "gemini",
            "content": "the old boundary tail is preserved byte-for-byte"
        }),
    ]);
    baseline_bytes.insert(0, b' ');
    fs::write(&path, &baseline_bytes).unwrap();

    let source = rediscover(&root, &path);
    let mut baseline_reader = read_gemini_transcript_pages(&source, None).unwrap();
    let baseline_page = baseline_reader.next_page().unwrap().unwrap();
    assert!(baseline_reader.next_page().unwrap().is_none());
    let baseline_outcome = baseline_reader.outcome().unwrap().clone();
    let frontier = baseline_page.next_safe_frontier;
    assert!(frontier.complete_prefix_end > PRESERVED_TAIL_BYTES as u64);

    let old_prefix_end = usize::try_from(frontier.complete_prefix_end).unwrap();
    let old_tail = baseline_bytes[old_prefix_end - PRESERVED_TAIL_BYTES..old_prefix_end].to_vec();
    let mut rewritten = baseline_bytes;
    assert_eq!(rewritten[0], b' ');
    rewritten[0] = b'\t';
    fs::write(&path, &rewritten).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&jsonl(&[json!({
            "id": "appended-after-rewrite",
            "type": "gemini",
            "content": "later append must not authorize the rewritten prefix"
        })]))
        .unwrap();
    let rewritten_with_append = fs::read(&path).unwrap();
    assert_eq!(
        &rewritten_with_append[old_prefix_end - PRESERVED_TAIL_BYTES..old_prefix_end],
        old_tail
    );
    let changed_source = rediscover(&root, &path);

    reset_gemini_parse_counters();
    assert!(matches!(
        read_gemini_transcript_pages_from_frontier(
            &changed_source,
            &frontier,
            GeminiNativePathProfile::CoreOnly
        ),
        Err(GeminiScanError::Capture(
            CaptureError::SourceChangedDuringCapture
        ))
    ));
    assert_eq!(
        gemini_resume_work_counters(),
        (0, frontier.complete_prefix_end)
    );

    reset_gemini_parse_counters();
    let baseline_previous = previous(&baseline_outcome, true);
    let mut fallback =
        read_gemini_transcript_pages(&changed_source, Some(&baseline_previous)).unwrap();
    assert_eq!(
        gemini_resume_work_counters(),
        (0, baseline_outcome.checkpoint.complete_prefix_end)
    );
    let fallback_page = fallback.next_page().unwrap().unwrap();
    assert_eq!(fallback_page.expected_frontier.complete_prefix_end, 0);
    assert!(fallback.next_page().unwrap().is_none());
    assert_eq!(
        fallback.outcome().unwrap().signals.source_change,
        GeminiSourceChange::Rewrite
    );
    assert!(fallback_page.events.iter().any(|event| {
        event.identity == GeminiEventIdentity::NativeRecordId("appended-after-rewrite".to_owned())
    }));
}

#[test]
fn gemini_nativepath_classifies_rewrite_truncation_and_replacement() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let baseline_values = [
        header("root-session", "main"),
        json!({
            "id": "user-1",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": "alpha"
        }),
        json!({
            "id": "assistant-1",
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "gemini",
            "content": "reply"
        }),
    ];
    let path = write_transcript(&root, &baseline_values);
    let source = rediscover(&root, &path);
    let (baseline, _) = scan_collect(&source, None);
    let previous_baseline = previous(&baseline, true);

    let rewrite_values = [
        header("root-session", "main"),
        json!({
            "id": "user-1",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": "omega"
        }),
        baseline_values[2].clone(),
    ];
    assert_eq!(jsonl(&baseline_values).len(), jsonl(&rewrite_values).len());
    fs::write(&path, jsonl(&rewrite_values)).unwrap();
    let source = rediscover(&root, &path);
    let (rewrite, rewrite_rows) = scan_collect(&source, Some(&previous_baseline));
    assert_eq!(rewrite.signals.source_change, GeminiSourceChange::Rewrite);
    assert_eq!(rewrite_rows.len(), 2);
    assert!(rewrite_rows[0].searchable_text.contains("omega"));

    fs::write(&path, jsonl(&rewrite_values[..2])).unwrap();
    let source = rediscover(&root, &path);
    let (truncation, truncation_rows) = scan_collect(&source, Some(&previous_baseline));
    assert_eq!(
        truncation.signals.source_change,
        GeminiSourceChange::Truncation
    );
    assert_eq!(truncation_rows.len(), 1);

    let replacement_values = [
        header("replacement-session", "main"),
        json!({
            "id": "replacement-user",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": "replacement"
        }),
    ];
    fs::write(&path, jsonl(&replacement_values)).unwrap();
    let source = rediscover(&root, &path);
    let (replacement, replacement_rows) = scan_collect(&source, Some(&previous_baseline));
    assert_eq!(
        replacement.signals.source_change,
        GeminiSourceChange::Replacement
    );
    assert_eq!(replacement_rows.len(), 1);
    assert_eq!(
        replacement
            .checkpoint
            .session
            .as_ref()
            .unwrap()
            .native_session_id,
        "replacement-session"
    );
}

#[test]
fn gemini_nativepath_distinguishes_relocation_from_live_copy() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({
                "id": "user-1",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "portable"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (baseline, _) = scan_collect(&source, None);
    let moved = root.join("tmp/relocated/chats/session-root.jsonl");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::copy(&path, &moved).unwrap();

    let moved_source = rediscover(&root, &moved);
    let (live_copy, live_copy_rows) = scan_collect(&moved_source, Some(&previous(&baseline, true)));
    assert_eq!(
        live_copy.signals.source_change,
        GeminiSourceChange::LiveCopy
    );
    assert_eq!(live_copy_rows.len(), 1);

    fs::remove_file(&path).unwrap();
    let moved_source = rediscover(&root, &moved);
    let (relocation, relocation_rows) =
        scan_collect(&moved_source, Some(&previous(&baseline, false)));
    assert_eq!(
        relocation.signals.source_change,
        GeminiSourceChange::Relocation
    );
    assert_eq!(relocation_rows.len(), 1);
    assert_eq!(relocation.checkpoint.session, baseline.checkpoint.session);
}

#[test]
fn gemini_nativepath_treats_divergent_routes_as_independent_replacements() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({
                "id": "user-1",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "original"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (baseline, _) = scan_collect(&source, None);

    let divergent = root.join("tmp/divergent/chats/session-root.jsonl");
    fs::create_dir_all(divergent.parent().unwrap()).unwrap();
    fs::write(
        &divergent,
        jsonl(&[
            header("root-session", "main"),
            json!({
                "id": "user-1",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": "divergent"
            }),
        ]),
    )
    .unwrap();
    let divergent_source = rediscover(&root, &divergent);
    let (divergent_outcome, divergent_rows) =
        scan_collect(&divergent_source, Some(&previous(&baseline, false)));

    assert_eq!(divergent_rows.len(), 1);
    assert_eq!(
        divergent_outcome.signals.source_change,
        GeminiSourceChange::Replacement
    );
    assert_eq!(
        divergent_outcome.signals.publication_shape,
        GeminiPublicationShape::AuthoritativeSnapshot
    );
    assert!(divergent_outcome.signals.cursor_advance_allowed);
    assert!(!divergent_outcome.signals.emitted_zero_rows);
    assert_ne!(
        divergent_outcome.checkpoint.source_sha256,
        baseline.checkpoint.source_sha256
    );

    let incompatible = root.join("tmp/relocated/chats/root-session/session-root.jsonl");
    fs::create_dir_all(incompatible.parent().unwrap()).unwrap();
    fs::copy(&path, &incompatible).unwrap();
    let incompatible_source = rediscover(&root, &incompatible);
    let (incompatible_outcome, incompatible_rows) =
        scan_collect(&incompatible_source, Some(&previous(&baseline, false)));

    assert_eq!(incompatible_rows.len(), 1);
    assert_eq!(
        incompatible_outcome.signals.source_change,
        GeminiSourceChange::Replacement
    );
    assert!(incompatible_outcome.signals.cursor_advance_allowed);
    assert_eq!(
        incompatible_outcome
            .checkpoint
            .session
            .as_ref()
            .unwrap()
            .parent_native_session_id
            .as_deref(),
        Some("root-session")
    );
}

#[test]
fn gemini_nativepath_delegates_cross_page_duplicate_authority_to_canonical_identity_consumer() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut records = vec![header("duplicate-session", "main")];
    records.extend((0..MAX_GEMINI_NATIVE_PAGE_RECORDS - 1).map(|index| {
        json!({
            "id": if index == 0 {
                "duplicate-id".to_owned()
            } else {
                format!("valid-{index:02}")
            },
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": format!("retained request {index}")
        })
    }));
    records.push(json!({
        "id": "duplicate-id",
        "timestamp": "2026-01-01T00:00:02.000Z",
        "type": "gemini",
        "content": "same canonical native identity on the next bounded page"
    }));
    records.push(json!({
        "id": "later-valid",
        "timestamp": "2026-01-01T00:00:03.000Z",
        "type": "gemini",
        "content": "later sibling survives"
    }));
    let path = write_transcript(&root, &records);
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();

    let first_page = reader.next_page().unwrap().unwrap();
    assert_eq!(first_page.physical_records, MAX_GEMINI_NATIVE_PAGE_RECORDS);
    assert_eq!(first_page.events.len(), MAX_GEMINI_NATIVE_PAGE_RECORDS - 1);
    assert!(first_page.rejections.is_empty());
    let committed_valid_identity = first_page.identity;

    let second_page = reader.next_page().unwrap().unwrap();
    assert_eq!(second_page.expected_frontier, first_page.next_safe_frontier);
    assert!(second_page.rejections.is_empty());
    assert_eq!(second_page.events.len(), 2);
    assert_eq!(
        second_page.events[0].identity,
        GeminiEventIdentity::NativeRecordId("duplicate-id".to_owned())
    );
    assert_eq!(
        first_page.events[0].identity,
        second_page.events[0].identity
    );
    assert_eq!(
        second_page.events[1].identity,
        GeminiEventIdentity::NativeRecordId("later-valid".to_owned())
    );
    let replay_frontier = second_page.expected_frontier.clone();
    let second_identity = second_page.identity;
    assert!(reader.next_page().unwrap().is_none());
    assert_eq!(reader.outcome().unwrap().rejected_records, 0);

    let mut replay = read_gemini_transcript_pages_from_frontier(
        &source,
        &replay_frontier,
        GeminiNativePathProfile::CoreOnly,
    )
    .unwrap();
    let replayed = replay.next_page().unwrap().unwrap();
    assert_eq!(replayed.identity, second_identity);
    assert_eq!(replayed.rejections, second_page.rejections);
    assert_eq!(replayed.events, second_page.events);
    assert_ne!(committed_valid_identity, replayed.identity);
}

#[test]
fn gemini_nativepath_rejects_duplicates_within_one_bounded_page() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let duplicate_sentinel = "WITHIN_PAGE_DUPLICATE_MUST_STAY_PRIVATE";
    let path = write_transcript(
        &root,
        &[
            header("within-page-duplicate", "main"),
            json!({
                "id": "duplicate-id",
                "type": "user",
                "content": "first canonical observation"
            }),
            json!({
                "id": "duplicate-id",
                "type": "gemini",
                "content": duplicate_sentinel
            }),
            json!({
                "id": "later-valid",
                "type": "gemini",
                "content": "later sibling survives"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();

    let page = reader.next_page().unwrap().unwrap();
    assert_eq!(page.physical_records, 4);
    assert_eq!(page.events.len(), 2);
    assert_eq!(page.rejections.len(), 1);
    assert!(page.rejections[0]
        .reason
        .contains("duplicate Gemini native event id"));
    assert_eq!(
        page.events
            .iter()
            .map(|event| &event.identity)
            .collect::<Vec<_>>(),
        [
            &GeminiEventIdentity::NativeRecordId("duplicate-id".to_owned()),
            &GeminiEventIdentity::NativeRecordId("later-valid".to_owned())
        ]
    );
    assert!(!format!("{page:?}").contains(duplicate_sentinel));
    assert!(reader.next_page().unwrap().is_none());
    assert_eq!(reader.outcome().unwrap().rejected_records, 1);
}

#[test]
fn gemini_nativepath_native_event_identity_state_has_exact_count_and_byte_bounds() {
    let mut count_bounded = GeminiNativeEventIds::with_limits(2, 100);
    count_bounded.insert("first".to_owned(), 0).unwrap();
    count_bounded.insert("second".to_owned(), 1).unwrap();
    assert!(matches!(
        count_bounded.insert("third".to_owned(), 2),
        Err(GeminiScanError::NativeEventIdentityCountOverflow { limit: 2 })
    ));

    let mut byte_bounded = GeminiNativeEventIds::with_limits(10, 5);
    byte_bounded.insert("12345".to_owned(), 0).unwrap();
    assert!(matches!(
        byte_bounded.insert("6".to_owned(), 1),
        Err(GeminiScanError::NativeEventIdentityBytesOverflow { limit: 5 })
    ));

    let mut duplicate = GeminiNativeEventIds::with_limits(1, 3);
    duplicate.insert("one".to_owned(), 7).unwrap();
    assert!(matches!(
        duplicate.insert("one".to_owned(), 9),
        Err(GeminiScanError::DuplicateNativeEventId {
            first_raw_ordinal: 7,
            duplicate_raw_ordinal: 9,
            ..
        })
    ));
}

#[test]
fn gemini_nativepath_result_only_failure_retains_only_a_sparse_core_diagnostic() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("root-session", "main"),
            json!({
                "id": "result-only",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "call-1",
                    "name": "run_shell_command",
                    "result": {
                        "content": "result-only-secret",
                        "error": "failure is excluded too"
                    }
                }]
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let (outcome, rows) = scan_collect(&source, None);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].event_type, EventType::ToolOutput);
    assert_eq!(rows[0].role, EventRole::Tool);
    assert!(rows[0].preview.contains("result-only-secret"));
    assert!(!outcome.signals.emitted_zero_rows);
    assert!(!outcome.signals.source_has_zero_retained_rows);
    assert_eq!(outcome.metrics.native_result_records_observed, 1);
    assert!(outcome.metrics.result_body_bytes_decoded_or_allocated > 0);
    assert_eq!(outcome.metrics.result_body_hashes_created, 1);
    assert_eq!(outcome.metrics.result_previews_created, 1);
    assert_eq!(outcome.metrics.result_file_touches_created, 0);
    assert_eq!(outcome.metrics.result_handoffs_created, 0);
}

#[test]
fn gemini_nativepath_matches_c0_retention_counts_without_header_notices() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut baseline = vec![header("baseline-session", "main")];
    let kinds = [
        "user",
        "assistant",
        "tool_call",
        "tool_output",
        "state",
        "assistant",
    ];
    for index in 0..20 {
        let record = match kinds[index % kinds.len()] {
            "user" => json!({
                "id": format!("baseline-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": format!("user {index}")
            }),
            "assistant" => json!({
                "id": format!("baseline-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "content": format!("assistant {index}")
            }),
            "tool_call" => json!({
                "id": format!("baseline-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{"id": format!("call-{index}"), "name": "write_file", "args": {"path": "safe.txt"}}]
            }),
            "tool_output" => json!({
                "id": format!("baseline-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{"id": format!("call-{index}"), "name": "write_file", "result": {"content": format!("NATIVEPATH_SYNTHETIC_OUTPUT_{index}")}}]
            }),
            "state" => json!({
                "id": format!("baseline-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "$set": {"summary": format!("state {index}")}
            }),
            unexpected => panic!("unexpected synthetic event kind {unexpected}"),
        };
        baseline.push(record);
    }
    let path = write_transcript(&root, &baseline);
    let source = rediscover(&root, &path);
    let (baseline_outcome, baseline_rows) = scan_collect(&source, None);
    assert_eq!(baseline_rows.len(), 17);
    assert_eq!(baseline_outcome.metrics.header_records, 1);
    assert_eq!(baseline_outcome.metrics.native_result_records_observed, 3);
    assert_eq!(baseline_outcome.metrics.retained_messages, 11);
    assert_eq!(baseline_outcome.metrics.retained_tool_calls, 3);
    assert_eq!(baseline_outcome.metrics.retained_notices, 3);

    let mut output_heavy = vec![header("output-session", "main")];
    for index in 0..20 {
        let record = match index {
            0 | 10 => json!({
                "id": format!("output-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "user",
                "content": format!("user {index}")
            }),
            1 | 11 => json!({
                "id": format!("output-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "content": format!("assistant {index}")
            }),
            index if index % 2 == 0 => json!({
                "id": format!("output-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{"id": format!("call-{index}"), "name": "write_file", "args": {"path": "safe.txt"}}]
            }),
            _ => json!({
                "id": format!("output-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{"id": format!("call-{index}"), "name": "write_file", "result": {"content": format!("NATIVEPATH_SYNTHETIC_OUTPUT_HEAVY_{index}")}}]
            }),
        };
        output_heavy.push(record);
    }
    fs::write(&path, jsonl(&output_heavy)).unwrap();
    let source = rediscover(&root, &path);
    let (output_outcome, output_rows) = scan_collect(&source, None);
    assert_eq!(output_rows.len(), 12);
    assert_eq!(output_outcome.metrics.header_records, 1);
    assert_eq!(output_outcome.metrics.native_result_records_observed, 8);
    assert_eq!(output_outcome.metrics.retained_messages, 4);
    assert_eq!(output_outcome.metrics.retained_tool_calls, 8);
    assert_eq!(output_outcome.metrics.retained_notices, 0);
    assert!(output_rows
        .iter()
        .all(|row| !format!("{row:?}").contains("NATIVEPATH_SYNTHETIC_OUTPUT")));
}

#[test]
fn gemini_nativepath_file_touch_set_is_deterministic_and_rejects_count_overflow() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let calls: Vec<_> = (0..MAX_GEMINI_FILE_TOUCHES_PER_EVENT)
        .rev()
        .map(|index| {
            json!({
                "id": format!("call-{index}"),
                "name": "write_file",
                "args": {"path": format!("path-{index:04}.txt")}
            })
        })
        .collect();
    let path = write_transcript(
        &root,
        &[
            header("touch-count-session", "main"),
            json!({
                "id": "touch-count-boundary",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": calls
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (boundary, rows) = scan_collect(&source, None);

    assert_eq!(boundary.rejected_records, 0);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].safe_file_touches.len(),
        MAX_GEMINI_FILE_TOUCHES_PER_EVENT
    );
    assert!(rows[0]
        .safe_file_touches
        .windows(2)
        .all(|pair| pair[0] < pair[1]));

    let overflow_calls: Vec<_> = (0..=MAX_GEMINI_FILE_TOUCHES_PER_EVENT)
        .map(|index| {
            json!({
                "id": format!("overflow-call-{index}"),
                "name": "write_file",
                "args": {"path": format!("overflow-{index:04}.txt")}
            })
        })
        .collect();
    fs::write(
        &path,
        jsonl(&[
            header("touch-count-session", "main"),
            json!({
                "id": "touch-count-overflow",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": overflow_calls
            }),
        ]),
    )
    .unwrap();
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let safe_page = reader.next_page().unwrap().unwrap();
    assert!(safe_page.events.is_empty());
    assert_eq!(safe_page.next_safe_frontier.next_raw_ordinal, 1);
    assert_eq!(safe_page.next_safe_frontier.rejected_records, 0);
    let error = reader.next_page().unwrap_err();
    assert!(matches!(
        error,
        GeminiScanError::UncommittedRecord {
            raw_ordinal: 1,
            ref reason,
            ..
        } if reason.contains("256 unique file-touch limit")
    ));
}

#[test]
fn gemini_nativepath_file_touch_set_enforces_exact_byte_boundary() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("touch-byte-session", "main"),
            json!({
                "id": "touch-byte-boundary",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "call-boundary",
                    "name": "write_file",
                    "args": {"path": "x".repeat(MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT)}
                }]
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let (boundary, rows) = scan_collect(&source, None);

    assert_eq!(boundary.rejected_records, 0);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].safe_file_touches[0].len(),
        MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT
    );

    fs::write(
        &path,
        jsonl(&[
            header("touch-byte-session", "main"),
            json!({
                "id": "touch-byte-overflow",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "call-overflow",
                    "name": "write_file",
                    "args": {"path": "x".repeat(MAX_GEMINI_FILE_TOUCH_BYTES_PER_EVENT + 1)}
                }]
            }),
        ]),
    )
    .unwrap();
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let safe_page = reader.next_page().unwrap().unwrap();
    assert!(safe_page.events.is_empty());
    assert_eq!(safe_page.next_safe_frontier.next_raw_ordinal, 1);
    let error = reader.next_page().unwrap_err();
    assert!(matches!(
        error,
        GeminiScanError::UncommittedRecord {
            raw_ordinal: 1,
            ref reason,
            ..
        } if reason.contains("65536 file-touch byte limit")
    ));
}

#[test]
fn gemini_nativepath_streams_local_scale_without_accumulating_rows_or_results() {
    const PAIRS: usize = 2_000;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = transcript_path(&root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::File::create(&path).unwrap();
    serde_json::to_writer(&mut file, &header("scale-session", "main")).unwrap();
    file.write_all(b"\n").unwrap();
    let output_payload = "x".repeat(1_024);
    for index in 0..PAIRS {
        serde_json::to_writer(
            &mut file,
            &json!({
                "id": format!("request-{index}"),
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": format!("call-{index}"),
                    "name": "write_file",
                    "args": {"path": format!("safe-{index}.txt")}
                }]
            }),
        )
        .unwrap();
        file.write_all(b"\n").unwrap();
        serde_json::to_writer(
            &mut file,
            &json!({
                "id": format!("result-{index}"),
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": format!("call-{index}"),
                    "name": "write_file",
                    "result": {
                        "content": output_payload,
                        "path": format!("/workspace/nativepath-fixture/output-only/{index}")
                    }
                }]
            }),
        )
        .unwrap();
        file.write_all(b"\n").unwrap();
    }
    drop(file);
    let source = rediscover(&root, &path);
    let (outcome, retained) = scan_collect(&source, None);
    for event in &retained {
        assert!(event
            .safe_file_touches
            .iter()
            .all(|path| !path.contains("output-only")));
    }

    assert_eq!(retained.len(), PAIRS);
    assert_eq!(outcome.metrics.retained_tool_calls, PAIRS as u64);
    assert_eq!(outcome.metrics.native_result_records_observed, PAIRS as u64);
    assert!(
        outcome.metrics.native_result_record_bytes_observed > (PAIRS as u64).saturating_mul(1_024)
    );
    assert_eq!(outcome.metrics.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(outcome.metrics.result_body_hashes_created, 0);
    assert_eq!(outcome.metrics.result_previews_created, 0);
    assert_eq!(outcome.metrics.result_file_touches_created, 0);
    assert_eq!(outcome.checkpoint.next_raw_ordinal, 1 + (PAIRS as u64 * 2));
}

#[test]
fn gemini_nativepath_pull_reader_pages_at_physical_record_bound() {
    const EVENTS: usize = MAX_GEMINI_NATIVE_PAGE_RECORDS * 2 + 7;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut values = vec![header("page-records", "main")];
    values.extend((0..EVENTS).map(|index| {
        json!({
            "id": format!("event-{index}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "content": format!("message {index}")
        })
    }));
    let path = write_transcript(&root, &values);
    let source = rediscover(&root, &path);

    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let mut physical_records = 0_usize;
    let mut retained_events = 0_usize;
    let mut pages = 0_usize;
    while let Some(page) = reader.next_page().unwrap() {
        assert!(page.physical_records <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
        assert!(page.retained_event_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
        physical_records += page.physical_records;
        retained_events += page.events.len();
        pages += 1;
    }
    let outcome = reader.outcome().unwrap();

    assert_eq!(physical_records, EVENTS + 1);
    assert_eq!(retained_events, EVENTS);
    assert_eq!(pages, 3);
    assert_eq!(outcome.checkpoint.next_raw_ordinal, (EVENTS + 1) as u64);
}

#[test]
fn gemini_nativepath_pull_reader_pages_at_retained_byte_bound() {
    const CONTENT_BYTES: usize = 2_100_000;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("page-bytes", "main"),
            json!({
                "id": "large-1",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "content": "a".repeat(CONTENT_BYTES)
            }),
            json!({
                "id": "large-2",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "gemini",
                "content": "b".repeat(CONTENT_BYTES)
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let mut page_bytes = Vec::new();
    let mut retained_events = 0_usize;
    while let Some(page) = reader.next_page().unwrap() {
        assert!(page.physical_records <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
        assert!(page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
        if !page.events.is_empty() {
            assert!(page.retained_event_bytes > CONTENT_BYTES * 2);
            page_bytes.push(page.retained_event_bytes);
        }
        retained_events += page.events.len();
    }
    let outcome = reader.outcome().unwrap();

    assert_eq!(page_bytes.len(), 2);
    assert!(page_bytes
        .iter()
        .all(|bytes| *bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES));
    assert_eq!(retained_events, 2);
    assert_eq!(outcome.rejected_records, 0);
}

#[test]
fn gemini_nativepath_safe_pages_rewind_before_an_uncommitted_overflow_record() {
    const CONTENT_BYTES: usize = 2_100_000;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut records = vec![header("safe-frontier-pages", "main")];
    records.extend((0..3).map(|index| {
        json!({
            "id": format!("large-{index}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "content": "x".repeat(CONTENT_BYTES)
        })
    }));
    let path = write_transcript(&root, &records);
    let source = rediscover(&root, &path);

    let mut reader = read_gemini_transcript_pages(&source, None).unwrap();
    let mut previous_frontier = None;
    let mut identities = Vec::new();
    let mut event_ids = Vec::new();
    while let Some(page) = reader.next_page().unwrap() {
        if let Some(previous) = previous_frontier.as_ref() {
            assert_eq!(&page.expected_frontier, previous);
        }
        assert!(page.physical_records <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
        assert!(page.logical_units <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
        assert!(page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
        assert_ne!(page.identity.as_bytes(), &[0; 32]);
        identities.push(page.identity);
        event_ids.extend(page.events.iter().map(|event| match &event.identity {
            GeminiEventIdentity::NativeRecordId(id) => id.clone(),
        }));
        previous_frontier = Some(page.next_safe_frontier);
    }
    let outcome = reader.outcome().unwrap();

    assert_eq!(event_ids, vec!["large-0", "large-1", "large-2"]);
    assert_eq!(identities.len(), 3);
    assert_eq!(
        previous_frontier.unwrap().complete_prefix_end,
        outcome.checkpoint.complete_prefix_end
    );
    assert_eq!(outcome.rejected_records, 0);
}

#[test]
fn gemini_nativepath_profile_gates_successful_output_hydration_from_core() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let success_body = "PRO_SUCCESSFUL_OUTPUT_CONTENT";
    let failure_body = "PRO_FAILED_OUTPUT_CONTENT";
    let timeout_body = "PRO_TIMEOUT_OUTPUT_CONTENT";
    let unknown_body = "PRO_UNKNOWN_OUTPUT_CONTENT";
    let path = write_transcript(
        &root,
        &[
            header("profile-gate", "main"),
            json!({
                "id": "success-result-record",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "success-call",
                    "name": "run_shell_command",
                    "result": {
                        "content": success_body,
                        "error": false,
                        "exitCode": 0,
                        "durationMs": 17
                    }
                }]
            }),
            json!({
                "id": "failure-result-record",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "failure-call",
                    "name": "run_shell_command",
                    "result": {
                        "content": failure_body,
                        "error": "command failed",
                        "exitCode": 1
                    }
                }]
            }),
            json!({
                "id": "safe-message",
                "timestamp": "2026-01-01T00:00:03.000Z",
                "type": "gemini",
                "content": "safe core message"
            }),
            json!({
                "id": "timeout-result-record",
                "timestamp": "2026-01-01T00:00:04.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "timeout-call",
                    "name": "run_shell_command",
                    "result": {
                        "content": timeout_body,
                        "timedOut": true,
                        "durationMs": 2_000
                    }
                }]
            }),
            json!({
                "id": "unknown-result-record",
                "timestamp": "2026-01-01T00:00:05.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "unknown-call",
                    "name": "run_shell_command",
                    "result": {
                        "content": unknown_body,
                        "timedOut": false,
                        "timeout": false
                    }
                }]
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let mut core_reader = read_gemini_transcript_pages(&source, None).unwrap();
    let mut core_page_ids = Vec::new();
    let mut core_rows = Vec::new();
    while let Some(page) = core_reader.next_page().unwrap() {
        assert!(page.output_pages.is_empty());
        core_page_ids.push(page.identity);
        core_rows.extend(page.events);
    }
    let core_outcome = core_reader.outcome().unwrap();
    assert_eq!(core_rows.len(), 3);
    assert!(!format!("{core_rows:?}").contains(success_body));
    assert!(format!("{core_rows:?}").contains(failure_body));
    assert!(format!("{core_rows:?}").contains(timeout_body));
    assert!(!format!("{core_rows:?}").contains(unknown_body));
    let diagnostic_outcomes = core_rows
        .iter()
        .filter_map(|row| match &row.body {
            GeminiEventBody::OutputDiagnostic { outcome, .. } => Some(outcome.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(diagnostic_outcomes, ["failure", "timeout"]);
    assert_eq!(
        core_outcome.metrics.result_body_bytes_decoded_or_allocated,
        u64::try_from(failure_body.len() + timeout_body.len()).unwrap()
    );
    assert_eq!(core_outcome.metrics.result_body_hashes_created, 2);
    assert_eq!(core_outcome.metrics.result_previews_created, 2);
    assert_eq!(core_outcome.metrics.result_file_touches_created, 0);
    assert_eq!(core_outcome.metrics.result_fts_documents_created, 0);
    assert_eq!(core_outcome.metrics.result_handoffs_created, 0);

    let mut pro_reader = read_gemini_transcript_pages_with_profile(
        &source,
        None,
        GeminiNativePathProfile::CoreAndTransientOutputs,
    )
    .unwrap();
    let mut pro_page_ids = Vec::new();
    let mut pro_output_page_ids = Vec::new();
    let mut outputs = Vec::new();
    let mut pro_rows = Vec::new();
    let mut saw_terminal_page = false;
    while let Some(mut page) = pro_reader.next_page().unwrap() {
        assert!(page.logical_units <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
        assert!(page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
        saw_terminal_page |= page.terminal;
        pro_page_ids.push(page.identity);
        for mut output_page in page.output_pages {
            assert!(output_page.logical_units <= MAX_GEMINI_NATIVE_PAGE_RECORDS);
            assert!(output_page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES);
            pro_output_page_ids.push(output_page.identity);
            outputs.append(&mut output_page.outputs);
        }
        pro_rows.append(&mut page.events);
    }
    let pro_outcome = pro_reader.outcome().unwrap();

    assert_eq!(pro_rows, core_rows);
    assert_eq!(outputs.len(), 4);
    assert_eq!(outputs[0].content, success_body.as_bytes());
    assert_eq!(outputs[0].outcome.outcome, OutputOutcome::Success);
    assert_eq!(outputs[0].outcome.exit_code, Some(0));
    assert_eq!(outputs[0].outcome.duration_ms, Some(17));
    assert_eq!(outputs[1].content, failure_body.as_bytes());
    assert_eq!(outputs[1].outcome.outcome, OutputOutcome::Failure);
    assert_eq!(outputs[1].outcome.exit_code, Some(1));
    assert_eq!(outputs[2].content, timeout_body.as_bytes());
    assert_eq!(outputs[2].outcome.outcome, OutputOutcome::Timeout);
    assert_eq!(outputs[2].outcome.duration_ms, Some(2_000));
    assert_eq!(outputs[3].content, unknown_body.as_bytes());
    assert_eq!(outputs[3].outcome.outcome, OutputOutcome::Success);
    assert_eq!(
        outputs[0].coordinate.native_record_id.as_deref(),
        Some("success-result-record")
    );
    assert_eq!(outputs[0].coordinate.source_record_subrecord_index, Some(0));
    assert_eq!(outputs[0].call_id.as_deref(), Some("success-call"));
    assert_eq!(
        pro_outcome.metrics.result_body_bytes_decoded_or_allocated,
        u64::try_from(
            success_body.len() + failure_body.len() + timeout_body.len() + unknown_body.len()
        )
        .unwrap()
    );
    assert_eq!(pro_outcome.metrics.result_handoffs_created, 4);
    assert_eq!(pro_outcome.metrics.result_body_hashes_created, 2);
    assert_eq!(pro_outcome.metrics.result_previews_created, 2);
    assert_eq!(pro_outcome.metrics.result_file_touches_created, 0);
    assert_eq!(pro_outcome.metrics.result_fts_documents_created, 0);
    assert!(saw_terminal_page);
    assert_eq!(core_page_ids, pro_page_ids);
    assert!(pro_output_page_ids
        .iter()
        .all(|identity| identity.as_bytes() != &[0; 32]));
    assert_eq!(pro_outcome.terminal_source_observation, source.observation);
    assert_eq!(
        pro_outcome.terminal_source_observation,
        pro_outcome.checkpoint.source_observation
    );
}

#[test]
fn gemini_nativepath_core_pages_are_profile_invariant_under_output_unit_pressure() {
    const RESULT_RECORDS: usize = 33;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut values = vec![header("profile-unit-pressure", "main")];
    values.extend((0..RESULT_RECORDS).map(|index| {
        let first_content = if index == 0 {
            String::new()
        } else {
            format!("first-output-{index:02}")
        };
        json!({
            "id": format!("result-{index:02}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "toolCalls": [
                {
                    "id": format!("first-call-{index:02}"),
                    "result": {"content": first_content}
                },
                {
                    "id": format!("second-call-{index:02}"),
                    "result": {"content": format!("second-output-{index:02}")}
                }
            ]
        })
    }));
    let path = write_transcript(&root, &values);
    let source = rediscover(&root, &path);

    let collect = |profile| {
        let mut reader = read_gemini_transcript_pages_with_profile(&source, None, profile).unwrap();
        let mut core_pages = Vec::new();
        let mut output_pages = Vec::new();
        while let Some(mut page) = reader.next_page().unwrap() {
            output_pages.append(&mut page.output_pages);
            core_pages.push((
                page.expected_frontier,
                page.next_safe_frontier,
                page.identity,
                page.terminal,
                page.physical_records,
                page.logical_units,
                page.retained_event_bytes,
                page.conservative_serialized_bytes,
                page.events,
                page.rejections,
            ));
        }
        let metrics = reader.outcome().unwrap().metrics.clone();
        (core_pages, output_pages, metrics)
    };

    let (core_only_pages, core_only_output_pages, core_only_metrics) =
        collect(GeminiNativePathProfile::CoreOnly);
    let (pro_core_pages, pro_output_pages, pro_metrics) =
        collect(GeminiNativePathProfile::CoreAndTransientOutputs);

    assert_eq!(core_only_pages, pro_core_pages);
    assert_eq!(core_only_pages.len(), 1);
    assert_eq!(core_only_pages[0].4, RESULT_RECORDS + 1);
    assert_eq!(core_only_pages[0].5, 0);
    assert!(core_only_output_pages.is_empty());
    assert_eq!(
        pro_output_pages
            .iter()
            .map(|page| page.logical_units)
            .collect::<Vec<_>>(),
        [MAX_GEMINI_NATIVE_PAGE_RECORDS, 2]
    );
    assert_eq!(
        pro_output_pages
            .iter()
            .map(|page| page.page_ordinal)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert!(pro_output_pages.iter().all(|page| {
        page.logical_units <= MAX_GEMINI_NATIVE_PAGE_RECORDS
            && page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES
            && page.identity.as_bytes() != &[0; 32]
    }));
    assert_ne!(pro_output_pages[0].identity, pro_output_pages[1].identity);
    let outputs = pro_output_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), RESULT_RECORDS * 2);
    assert!(outputs[0].content.is_empty());
    assert_eq!(outputs[0].coordinate.source_record_subrecord_index, Some(0));
    assert_eq!(outputs[1].coordinate.source_record_subrecord_index, Some(1));
    assert_eq!(core_only_metrics.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(core_only_metrics.result_handoffs_created, 0);
    assert_eq!(
        pro_metrics.result_handoffs_created,
        (RESULT_RECORDS * 2) as u64
    );
}

#[test]
fn gemini_nativepath_core_pages_are_profile_invariant_under_output_byte_pressure() {
    const RESULT_RECORDS: usize = 4;
    const CONTENT_BYTES: usize = 1_650_000;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut values = vec![header("profile-byte-pressure", "main")];
    values.extend((0..RESULT_RECORDS).map(|index| {
        json!({
            "id": format!("large-result-{index}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "toolCalls": [{
                "id": format!("large-call-{index}"),
                "result": {"content": char::from(b'a' + index as u8).to_string().repeat(CONTENT_BYTES)}
            }]
        })
    }));
    let path = write_transcript(&root, &values);
    let source = rediscover(&root, &path);

    let collect = |profile| {
        let mut reader = read_gemini_transcript_pages_with_profile(&source, None, profile).unwrap();
        let mut core_pages = Vec::new();
        let mut output_pages = Vec::new();
        while let Some(mut page) = reader.next_page().unwrap() {
            output_pages.append(&mut page.output_pages);
            core_pages.push((
                page.expected_frontier,
                page.next_safe_frontier,
                page.identity,
                page.terminal,
                page.physical_records,
                page.logical_units,
                page.retained_event_bytes,
                page.conservative_serialized_bytes,
                page.events,
                page.rejections,
            ));
        }
        let metrics = reader.outcome().unwrap().metrics.clone();
        (core_pages, output_pages, metrics)
    };

    let (core_only_pages, core_only_output_pages, core_only_metrics) =
        collect(GeminiNativePathProfile::CoreOnly);
    let (pro_core_pages, pro_output_pages, pro_metrics) =
        collect(GeminiNativePathProfile::CoreAndTransientOutputs);

    assert_eq!(core_only_pages, pro_core_pages);
    assert_eq!(core_only_pages.len(), 1);
    assert_eq!(core_only_pages[0].4, RESULT_RECORDS + 1);
    assert_eq!(core_only_pages[0].5, 0);
    assert!(core_only_output_pages.is_empty());
    assert!(pro_output_pages.len() > 1);
    assert!(pro_output_pages.iter().all(|page| {
        page.logical_units <= MAX_GEMINI_NATIVE_PAGE_RECORDS
            && page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES
            && page.identity.as_bytes() != &[0; 32]
    }));
    assert!(
        pro_output_pages
            .iter()
            .map(|page| page.conservative_serialized_bytes)
            .sum::<usize>()
            > MAX_GEMINI_NATIVE_PAGE_BYTES
    );
    let outputs = pro_output_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), RESULT_RECORDS);
    assert!(outputs
        .iter()
        .all(|output| output.content.len() == CONTENT_BYTES));
    assert_eq!(core_only_metrics.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(core_only_metrics.result_handoffs_created, 0);
    assert_eq!(
        pro_metrics.result_body_bytes_decoded_or_allocated,
        (RESULT_RECORDS * CONTENT_BYTES) as u64
    );
    assert_eq!(pro_metrics.result_handoffs_created, RESULT_RECORDS as u64);
}

#[test]
fn gemini_nativepath_reads_each_record_once_and_performs_one_full_pro_hydration() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("one-pass", "main"),
            json!({
                "id": "failure",
                "type": "gemini",
                "toolCalls": [{
                    "id": "failure-call",
                    "result": {"content": "bounded failure", "error": true}
                }]
            }),
            json!({
                "id": "success",
                "type": "gemini",
                "toolCalls": [{
                    "id": "success-call",
                    "result": {"content": "complete success", "success": true}
                }]
            }),
            json!({
                "id": "message",
                "type": "user",
                "content": "later message"
            }),
        ],
    );
    let source = rediscover(&root, &path);

    reset_gemini_parse_counters();
    let (core_outcome, core_rows) = scan_collect(&source, None);
    assert_eq!(core_rows.len(), 2);
    assert_eq!(core_outcome.rejected_records, 0);
    assert_eq!(gemini_parse_counters(), (4, 2, 0));

    reset_gemini_parse_counters();
    let mut pro = read_gemini_transcript_pages_with_profile(
        &source,
        None,
        GeminiNativePathProfile::CoreAndTransientOutputs,
    )
    .unwrap();
    let mut outputs = Vec::new();
    while let Some(page) = pro.next_page().unwrap() {
        outputs.extend(page.output_pages.into_iter().flat_map(|page| page.outputs));
    }
    assert_eq!(outputs.len(), 2);
    assert_eq!(gemini_parse_counters(), (4, 2, 2));
}

#[test]
fn gemini_nativepath_core_only_builds_only_a_bounded_large_failure_preview() {
    const FAILURE_BYTES: usize = 3 * 1024 * 1024;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let canary = "FULL_FAILURE_BODY_MUST_NOT_BE_CONSTRUCTED_IN_CORE";
    // Newlines force escaped JSON string decoding. CoreOnly must still use
    // the raw bounded visitor rather than ask serde_json for an owned body.
    let failure = format!("{}{}", "\n".repeat(FAILURE_BYTES), canary);
    let path = write_transcript(
        &root,
        &[
            header("bounded-failure", "main"),
            json!({
                "id": "large-failure",
                "type": "gemini",
                "toolCalls": [{
                    "id": "failure-call",
                    "result": {
                        "content": failure,
                        "error": true,
                        "exitCode": 1
                    }
                }]
            }),
        ],
    );
    let source = rediscover(&root, &path);

    reset_gemini_parse_counters();
    let (outcome, rows) = scan_collect(&source, None);

    assert_eq!(rows.len(), 1);
    assert!(matches!(
        &rows[0].body,
        GeminiEventBody::OutputDiagnostic {
            output_preview: Some(preview),
            ..
        } if preview.chars().count() == PROVIDER_MAX_PREVIEW_CHARS
            && !preview.contains(canary)
    ));
    assert!(!format!("{rows:?}").contains(canary));
    assert!(
        outcome.metrics.result_body_bytes_decoded_or_allocated <= PROVIDER_MAX_PREVIEW_CHARS as u64
    );
    assert_eq!(gemini_parse_counters(), (2, 1, 0));
}

#[test]
fn gemini_nativepath_two_three_mib_outputs_emit_on_independent_pro_pages() {
    const CONTENT_BYTES: usize = 3 * 1024 * 1024;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("independent-output-sizing", "main"),
            json!({
                "id": "two-large-outputs",
                "type": "gemini",
                "toolCalls": [
                    {
                        "id": "first",
                        "result": {"content": "a".repeat(CONTENT_BYTES)}
                    },
                    {
                        "id": "second",
                        "result": {"content": "b".repeat(CONTENT_BYTES)}
                    }
                ]
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let mut core = read_gemini_transcript_pages(&source, None).unwrap();
    let core_page = core.next_page().unwrap().unwrap();
    let core_identity = core_page.identity;
    assert!(core_page.rejections.is_empty());
    assert!(core.next_page().unwrap().is_none());

    let mut pro = read_gemini_transcript_pages_with_profile(
        &source,
        None,
        GeminiNativePathProfile::CoreAndTransientOutputs,
    )
    .unwrap();
    let pro_core_page = pro.next_page().unwrap().unwrap();
    assert_eq!(pro_core_page.identity, core_identity);
    assert_eq!(pro_core_page.output_pages.len(), 2);
    assert!(pro_core_page.output_pages.iter().all(|page| {
        page.logical_units == 1
            && page.conservative_serialized_bytes <= MAX_GEMINI_NATIVE_PAGE_BYTES
    }));
    let outputs = pro_core_page
        .output_pages
        .into_iter()
        .flat_map(|page| page.outputs)
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0].content.len(), CONTENT_BYTES);
    assert_eq!(outputs[1].content.len(), CONTENT_BYTES);
    assert!(pro.next_page().unwrap().is_none());
}

#[test]
fn gemini_nativepath_oversized_output_is_local_and_core_identity_stays_profile_invariant() {
    const OVERSIZED_CONTENT_BYTES: usize = 6 * 1024 * 1024;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let path = write_transcript(
        &root,
        &[
            header("oversized-output-local", "main"),
            json!({
                "id": "mixed-size-outputs",
                "type": "gemini",
                "toolCalls": [
                    {
                        "id": "oversized",
                        "result": {"content": "x".repeat(OVERSIZED_CONTENT_BYTES)}
                    },
                    {
                        "id": "small-sibling",
                        "result": {"content": "small sibling survives"}
                    }
                ]
            }),
            json!({
                "id": "later-message",
                "type": "gemini",
                "content": "later record survives"
            }),
        ],
    );
    let source = rediscover(&root, &path);

    let collect = |profile| {
        let mut reader = read_gemini_transcript_pages_with_profile(&source, None, profile).unwrap();
        let mut core = Vec::new();
        let mut outputs = Vec::new();
        while let Some(mut page) = reader.next_page().unwrap() {
            outputs.extend(
                page.output_pages
                    .drain(..)
                    .flat_map(|output_page| output_page.outputs),
            );
            core.push((
                page.identity,
                page.expected_frontier,
                page.next_safe_frontier,
                page.events,
                page.rejections,
            ));
        }
        (core, outputs, reader.outcome().unwrap().clone())
    };

    let (core_only, core_only_outputs, core_outcome) = collect(GeminiNativePathProfile::CoreOnly);
    let (core_and_pro, pro_outputs, pro_outcome) =
        collect(GeminiNativePathProfile::CoreAndTransientOutputs);

    assert_eq!(core_only, core_and_pro);
    assert!(core_only_outputs.is_empty());
    assert_eq!(pro_outputs.len(), 1);
    assert_eq!(pro_outputs[0].content, b"small sibling survives");
    assert_eq!(
        pro_outputs[0].coordinate.source_record_subrecord_index,
        Some(1)
    );
    assert_eq!(core_only.len(), 1);
    assert_eq!(core_only[0].3.len(), 1);
    assert_eq!(
        core_only[0].3[0].identity,
        GeminiEventIdentity::NativeRecordId("later-message".to_owned())
    );
    assert_eq!(core_only[0].4.len(), 1);
    assert!(core_only[0].4[0].reason.contains("output subrecord 0"));
    assert_eq!(core_outcome.rejected_records, 1);
    assert_eq!(pro_outcome.rejected_records, 1);
}

#[test]
fn gemini_nativepath_output_selection_matches_legacy_first_present_oracle() {
    let fixtures = [
        ("direct-string", json!("direct-string")),
        (
            "empty-content",
            json!({
                "content": "",
                "output": "EMPTY_CONTENT_MUST_NOT_FALL_THROUGH",
                "text": "lower-priority"
            }),
        ),
        (
            "array-content",
            json!({
                "content": [],
                "output": "ARRAY_CONTENT_MUST_NOT_FALL_THROUGH"
            }),
        ),
        (
            "object-content",
            json!({
                "content": {"nested": "unsupported"},
                "output": "OBJECT_CONTENT_MUST_NOT_FALL_THROUGH"
            }),
        ),
        (
            "null-content",
            json!({
                "content": null,
                "output": "NULL_CONTENT_MUST_NOT_FALL_THROUGH"
            }),
        ),
        (
            "multiple-fields",
            json!({
                "content": "content-wins",
                "output": "output-loses",
                "text": "text-loses"
            }),
        ),
        (
            "output-before-text",
            json!({
                "output": "output-wins",
                "text": "text-loses"
            }),
        ),
        ("text-only", json!({"text": "text-wins"})),
    ];

    for (case, result) in fixtures {
        let temp = TempDir::new().unwrap();
        let root = fixture_root(&temp);
        let record = json!({
            "id": format!("record-{case}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "toolCalls": [{
                "id": format!("call-{case}"),
                "result": result
            }]
        });
        let oracle = extract_native_jsonl_result_content(GEMINI_RESULT_PROFILE, &record);
        let path = write_transcript(
            &root,
            &[header(&format!("precedence-{case}"), "main"), record],
        );
        let source = rediscover(&root, &path);

        let collect = |profile| {
            let mut reader =
                read_gemini_transcript_pages_with_profile(&source, None, profile).unwrap();
            let mut core_pages = Vec::new();
            let mut outputs = Vec::new();
            while let Some(page) = reader.next_page().unwrap() {
                outputs.extend(
                    page.output_pages
                        .into_iter()
                        .flat_map(|page| page.outputs)
                        .map(|output| output.content),
                );
                core_pages.push((
                    page.identity,
                    page.expected_frontier,
                    page.next_safe_frontier,
                    page.terminal,
                    page.physical_records,
                    page.logical_units,
                    page.conservative_serialized_bytes,
                    page.events,
                    page.rejections,
                ));
            }
            let rejected_records = reader.outcome().unwrap().rejected_records;
            (core_pages, outputs, rejected_records)
        };

        let (core_pages, core_outputs, core_rejections) =
            collect(GeminiNativePathProfile::CoreOnly);
        let (pro_core_pages, pro_outputs, pro_rejections) =
            collect(GeminiNativePathProfile::CoreAndTransientOutputs);
        assert_eq!(core_pages, pro_core_pages, "{case}");
        assert!(core_outputs.is_empty(), "{case}");
        assert_eq!(core_rejections, pro_rejections, "{case}");

        match oracle {
            Ok(Some(expected)) => {
                assert_eq!(pro_outputs, [expected.into_bytes()], "{case}");
            }
            Ok(None) | Err(NativeJsonlResultExtractionError::InvalidShape) => {
                assert!(pro_outputs.is_empty(), "{case}");
            }
            other => panic!("unexpected legacy oracle result for {case}: {other:?}"),
        }
    }
}

#[test]
fn gemini_nativepath_outcomes_match_shared_legacy_subrecord_oracle() {
    let fixtures = [
        (
            "empty-array-error",
            json!({"result": {"content": "empty-array", "error": []}}),
        ),
        (
            "empty-object-error",
            json!({"result": {"content": "empty-object", "error": {}}}),
        ),
        (
            "nonempty-array-error",
            json!({"result": {"content": "nonempty-array", "error": ["failure"]}}),
        ),
        (
            "string-false-error",
            json!({"result": {"content": "string-false", "error": "false"}}),
        ),
        (
            "floating-error",
            json!({"result": {"content": "floating-error", "error": 1.5}}),
        ),
        (
            "status-timeout",
            json!({"status": "timeout", "result": {"content": "status-timeout"}}),
        ),
        (
            "boolean-timeout",
            json!({"timeout": true, "result": {"content": "boolean-timeout"}}),
        ),
        (
            "spaced-timeout-status",
            json!({"status": "timed out", "result": {"content": "spaced-timeout"}}),
        ),
        (
            "false-timeout-success",
            json!({"timedOut": false, "result": {"content": "false-timeout"}}),
        ),
        (
            "false-ok-unknown",
            json!({"ok": false, "result": {"content": "false-ok"}}),
        ),
        (
            "nested-meta-redacted",
            json!({
                "result": {
                    "content": "nested-redaction-is-not-authoritative",
                    "meta": {"redacted": true}
                }
            }),
        ),
    ];

    for (case, mut call) in fixtures {
        call["id"] = json!(format!("call-{case}"));
        let record = json!({
            "id": format!("record-{case}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "toolCalls": [call]
        });
        let oracle = gemini_result_subrecord_oracle_for_tests(&record).unwrap();
        assert_eq!(oracle.len(), 1, "{case}");
        let (expected_subordinal, expected_content, expected_outcome) = &oracle[0];
        assert_eq!(*expected_subordinal, 0, "{case}");

        let temp = TempDir::new().unwrap();
        let root = fixture_root(&temp);
        let path = write_transcript(&root, &[header(&format!("outcome-{case}"), "main"), record]);
        let source = rediscover(&root, &path);
        let collect = |profile| {
            let mut reader =
                read_gemini_transcript_pages_with_profile(&source, None, profile).unwrap();
            let mut core_pages = Vec::new();
            let mut outputs = Vec::new();
            while let Some(page) = reader.next_page().unwrap() {
                outputs.extend(
                    page.output_pages
                        .into_iter()
                        .flat_map(|page| page.outputs)
                        .map(|output| (output.content, output.outcome)),
                );
                core_pages.push((
                    page.identity,
                    page.expected_frontier,
                    page.next_safe_frontier,
                    page.terminal,
                    page.physical_records,
                    page.logical_units,
                    page.conservative_serialized_bytes,
                    page.events,
                    page.rejections,
                ));
            }
            (core_pages, outputs)
        };

        let (core_pages, core_outputs) = collect(GeminiNativePathProfile::CoreOnly);
        let (pro_core_pages, pro_outputs) =
            collect(GeminiNativePathProfile::CoreAndTransientOutputs);
        assert_eq!(core_pages, pro_core_pages, "{case}");
        assert!(core_outputs.is_empty(), "{case}");
        assert_eq!(
            pro_outputs.len(),
            usize::from(expected_content.is_some()),
            "{case}"
        );
        if let Some(expected_content) = expected_content {
            assert_eq!(pro_outputs[0].0, expected_content.as_bytes(), "{case}");
            assert_eq!(&pro_outputs[0].1, expected_outcome, "{case}");
        }
        let retained_core_events = core_pages.iter().map(|page| page.7.len()).sum::<usize>();
        assert_eq!(
            retained_core_events,
            usize::from(matches!(
                expected_outcome.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            )),
            "{case}"
        );
    }
}

#[test]
fn gemini_nativepath_redaction_matches_legacy_and_suppresses_core_and_pro() {
    let base_record = |case: &str| {
        json!({
            "id": format!("record-{case}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "toolCalls": [{
                "id": format!("call-{case}"),
                "timedOut": true,
                "result": {
                    "content": format!("SECRET_REDACTED_DIAGNOSTIC_{case}")
                }
            }]
        })
    };
    let mut fixtures = Vec::new();

    let mut record = base_record("record-true");
    record["redacted"] = json!(true);
    fixtures.push(("record-true", record));

    let mut record = base_record("record-null");
    record["redacted"] = Value::Null;
    fixtures.push(("record-null", record));

    let mut record = base_record("call-string");
    record["toolCalls"][0]["isRedacted"] = json!("false");
    fixtures.push(("call-string", record));

    let mut record = base_record("call-number");
    record["toolCalls"][0]["is_redacted"] = json!(0);
    fixtures.push(("call-number", record));

    let mut record = base_record("result-array");
    record["toolCalls"][0]["result"]["redacted"] = json!([]);
    fixtures.push(("result-array", record));

    let mut record = base_record("result-object");
    record["toolCalls"][0]["result"]["isRedacted"] = json!({});
    fixtures.push(("result-object", record));

    let mut record = base_record("record-status");
    record["status"] = json!("redacted");
    fixtures.push(("record-status", record));

    let mut record = base_record("call-state");
    record["toolCalls"][0]["state"] = json!("output-redacted");
    fixtures.push(("call-state", record));

    let mut record = base_record("false-control");
    record["toolCalls"][0]["result"]["redacted"] = json!(false);
    fixtures.push(("false-control", record));

    let mut record = base_record("case-sensitive-control");
    record["toolCalls"][0]["result"]["status"] = json!("Redacted");
    fixtures.push(("case-sensitive-control", record));

    for (case, record) in fixtures {
        let oracle = extract_native_jsonl_result_content(GEMINI_RESULT_PROFILE, &record);
        let expected_redacted = oracle == Err(NativeJsonlResultExtractionError::Redacted);
        assert_eq!(
            expected_redacted,
            !matches!(case, "false-control" | "case-sensitive-control"),
            "fixture disagrees with the shared oracle: {case}"
        );
        let expected_content = oracle.as_ref().ok().and_then(Option::as_ref).cloned();

        let temp = TempDir::new().unwrap();
        let root = fixture_root(&temp);
        let path = write_transcript(
            &root,
            &[header(&format!("redaction-{case}"), "main"), record],
        );
        let source = rediscover(&root, &path);
        let collect = |profile| {
            let mut reader =
                read_gemini_transcript_pages_with_profile(&source, None, profile).unwrap();
            let mut core_pages = Vec::new();
            let mut outputs = Vec::new();
            while let Some(page) = reader.next_page().unwrap() {
                outputs.extend(
                    page.output_pages
                        .into_iter()
                        .flat_map(|page| page.outputs)
                        .map(|output| output.content),
                );
                core_pages.push((
                    page.identity,
                    page.expected_frontier,
                    page.next_safe_frontier,
                    page.terminal,
                    page.physical_records,
                    page.logical_units,
                    page.conservative_serialized_bytes,
                    page.events,
                    page.rejections,
                ));
            }
            let rejected_records = reader.outcome().unwrap().rejected_records;
            (core_pages, outputs, rejected_records)
        };

        let (core_pages, core_outputs, core_rejections) =
            collect(GeminiNativePathProfile::CoreOnly);
        let (pro_core_pages, pro_outputs, pro_rejections) =
            collect(GeminiNativePathProfile::CoreAndTransientOutputs);
        assert_eq!(core_pages, pro_core_pages, "{case}");
        assert!(core_outputs.is_empty(), "{case}");
        assert_eq!(core_rejections, pro_rejections, "{case}");

        if expected_redacted {
            assert!(core_pages.iter().all(|page| page.7.is_empty()), "{case}");
            assert!(pro_outputs.is_empty(), "{case}");
            assert_eq!(core_rejections, 0, "{case}");
            assert!(
                !format!("{core_pages:?}{pro_outputs:?}")
                    .contains(&format!("SECRET_REDACTED_DIAGNOSTIC_{case}")),
                "{case}"
            );
        } else {
            assert_eq!(
                core_pages.iter().map(|page| page.7.len()).sum::<usize>(),
                1,
                "{case}"
            );
            assert_eq!(
                pro_outputs,
                [expected_content.unwrap().into_bytes()],
                "{case}"
            );
        }
    }
}

#[test]
fn gemini_nativepath_page_identity_is_deterministic_for_one_profile() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut values = vec![header("deterministic-pages", "main")];
    values.extend((0..MAX_GEMINI_NATIVE_PAGE_RECORDS + 4).map(|index| {
        json!({
            "id": format!("message-{index:03}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "user",
            "content": format!("deterministic-{index:03}")
        })
    }));
    let path = write_transcript(&root, &values);
    let first_page = |source: &GeminiTranscriptSource| {
        let mut reader = read_gemini_transcript_pages(source, None).unwrap();
        reader.next_page().unwrap().unwrap()
    };

    let source = rediscover(&root, &path);
    let original = first_page(&source);
    let repeated = first_page(&source);
    assert_eq!(original.identity, repeated.identity);
    assert_eq!(original.expected_frontier, repeated.expected_frontier);
    assert_eq!(original.next_safe_frontier, repeated.next_safe_frontier);

    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&jsonl(&[json!({
            "id": "appended-after-first-page",
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "user",
            "content": "append does not re-identify a certified prefix"
        })]))
        .unwrap();
    let appended_source = rediscover(&root, &path);
    assert_eq!(original.identity, first_page(&appended_source).identity);

    let mut mutated = fs::read(&path).unwrap();
    let needle = b"deterministic-000";
    let position = mutated
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap();
    mutated[position + needle.len() - 1] = b'x';
    fs::write(&path, mutated).unwrap();
    let mutated_source = rediscover(&root, &path);
    assert_ne!(original.identity, first_page(&mutated_source).identity);
}

#[test]
fn gemini_nativepath_reopens_exactly_from_a_lagging_safe_frontier() {
    const EVENTS: usize = MAX_GEMINI_NATIVE_PAGE_RECORDS * 2 + 3;

    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let mut values = vec![header("frontier-resume", "main")];
    values.extend((0..EVENTS).map(|index| {
        json!({
            "id": format!("event-{index:03}"),
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "gemini",
            "content": format!("message {index}")
        })
    }));
    let path = write_transcript(&root, &values);
    let source = rediscover(&root, &path);

    let mut first_reader = read_gemini_transcript_pages(&source, None).unwrap();
    let first_page = first_reader.next_page().unwrap().unwrap();
    let frontier = first_page.next_safe_frontier.clone();
    let mut all_ids: Vec<_> = first_page
        .events
        .iter()
        .map(|event| match &event.identity {
            GeminiEventIdentity::NativeRecordId(id) => id.clone(),
        })
        .collect();
    drop(first_reader);

    let mut resumed = read_gemini_transcript_pages_from_frontier(
        &source,
        &frontier,
        GeminiNativePathProfile::CoreOnly,
    )
    .unwrap();
    let mut first_resumed_page = true;
    while let Some(page) = resumed.next_page().unwrap() {
        if first_resumed_page {
            assert_eq!(page.expected_frontier, frontier);
            first_resumed_page = false;
        }
        all_ids.extend(page.events.iter().map(|event| match &event.identity {
            GeminiEventIdentity::NativeRecordId(id) => id.clone(),
        }));
    }
    assert_eq!(all_ids.len(), EVENTS);
    assert_eq!(
        all_ids,
        (0..EVENTS)
            .map(|index| format!("event-{index:03}"))
            .collect::<Vec<_>>()
    );

    let mut mutated = fs::read(&path).unwrap();
    let needle = b"event-000";
    let position = mutated
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap();
    mutated[position + needle.len() - 1] = b'x';
    fs::write(&path, mutated).unwrap();
    let changed_source = rediscover(&root, &path);
    assert!(matches!(
        read_gemini_transcript_pages_from_frontier(
            &changed_source,
            &frontier,
            GeminiNativePathProfile::CoreOnly
        ),
        Err(GeminiScanError::Capture(
            CaptureError::SourceChangedDuringCapture
        ))
    ));
}

#[test]
fn gemini_nativepath_output_fanout_failure_retains_the_prior_frontier_for_retry() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let calls: Vec<_> = (0..=MAX_GEMINI_NATIVE_PAGE_RECORDS)
        .map(|index| {
            json!({
                "id": format!("call-{index}"),
                "result": {"content": format!("output-{index}")}
            })
        })
        .collect();
    let path = write_transcript(
        &root,
        &[
            header("bounded-output-fanout", "main"),
            json!({
                "id": "too-many-outputs",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": calls
            }),
            json!({
                "id": "later-valid",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "user",
                "content": "valid sibling survives"
            }),
        ],
    );
    let source = rediscover(&root, &path);
    let mut reader = read_gemini_transcript_pages_with_profile(
        &source,
        None,
        GeminiNativePathProfile::CoreAndTransientOutputs,
    )
    .unwrap();
    let safe_page = reader.next_page().unwrap().unwrap();
    assert!(safe_page.events.is_empty());
    assert_eq!(safe_page.output_pages.len(), 1);
    assert!(safe_page.output_pages[0].outputs.is_empty());
    assert_eq!(safe_page.next_safe_frontier.next_raw_ordinal, 1);
    assert_eq!(safe_page.next_safe_frontier.rejected_records, 0);
    let safe_frontier = safe_page.next_safe_frontier;
    let error = reader.next_page().unwrap_err();
    assert!(matches!(
        error,
        GeminiScanError::UncommittedRecord {
            raw_ordinal: 1,
            ref reason,
            ..
        } if reason.contains("exceeds the 64 output limit")
    ));
    assert!(reader.outcome().is_none());

    let mut same_input_retry = read_gemini_transcript_pages_from_frontier(
        &source,
        &safe_frontier,
        GeminiNativePathProfile::CoreAndTransientOutputs,
    )
    .unwrap();
    assert!(matches!(
        same_input_retry.next_page().unwrap_err(),
        GeminiScanError::UncommittedRecord { raw_ordinal: 1, .. }
    ));

    fs::write(
        &path,
        jsonl(&[
            header("bounded-output-fanout", "main"),
            json!({
                "id": "corrected-outputs",
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "gemini",
                "toolCalls": [
                    {"id": "corrected-0", "result": {"content": "output-0"}},
                    {"id": "corrected-1", "result": {"content": "output-1"}}
                ]
            }),
            json!({
                "id": "later-valid",
                "timestamp": "2026-01-01T00:00:02.000Z",
                "type": "user",
                "content": "valid sibling survives"
            }),
        ]),
    )
    .unwrap();
    let corrected_source = rediscover(&root, &path);
    let mut corrected = read_gemini_transcript_pages_from_frontier(
        &corrected_source,
        &safe_frontier,
        GeminiNativePathProfile::CoreAndTransientOutputs,
    )
    .unwrap();
    let mut rows = Vec::new();
    let mut outputs = Vec::new();
    while let Some(mut page) = corrected.next_page().unwrap() {
        rows.append(&mut page.events);
        for mut output_page in page.output_pages {
            outputs.append(&mut output_page.outputs);
        }
    }
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0].content, b"output-0");
    assert_eq!(outputs[1].content, b"output-1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].native_order.raw_ordinal, 2);
    assert_eq!(corrected.outcome().unwrap().rejected_records, 0);
}
