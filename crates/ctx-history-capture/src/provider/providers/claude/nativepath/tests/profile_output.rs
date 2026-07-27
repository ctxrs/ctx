use super::*;

#[test]
fn core_and_pro_fanout_is_profile_invariant_complete_and_privacy_safe() {
    use crate::OutputOutcome;

    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-fanout", "fanout");
    let output_patch =
        "*** Begin Patch\n*** Update File: leaked-output.rs\n@@\n-old\n+new\n*** End Patch";
    write_lines(
        &path,
        &[
            json!({
                "sessionId": "fanout",
                "type": "assistant",
                "uuid": "call-record",
                "timestamp": "2026-07-25T00:00:00Z",
                "message": {"role": "assistant", "content": [
                    {"type": "text", "text": "before call"},
                    {"type": "tool_use", "id": "call-read", "name": "Read",
                     "input": {"path": "src/owned.rs"}}
                ]}
            }),
            json!({
                "sessionId": "fanout",
                "type": "user",
                "uuid": "success-record",
                "timestamp": "2026-07-25T00:00:01Z",
                "message": {"role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": "call-read",
                    "content": output_patch
                }]},
                "toolUseResult": {"exitCode": 0}
            }),
            json!({
                "sessionId": "fanout",
                "type": "user",
                "uuid": "failure-record",
                "timestamp": "2026-07-25T00:00:02Z",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call-a", "content": "failed-a"},
                    {"type": "tool_result", "tool_use_id": "call-b", "content": ""}
                ]},
                "toolUseResult": {"exitCode": 7, "durationMs": 12}
            }),
            json!({
                "sessionId": "fanout",
                "type": "user",
                "uuid": "timeout-record",
                "timestamp": "2026-07-25T00:00:03Z",
                "toolUseResult": {"stdout": "", "timedOut": true, "durationMs": 55}
            }),
            json!({
                "sessionId": "fanout",
                "type": "user",
                "uuid": "unknown-record",
                "timestamp": "2026-07-25T00:00:04Z",
                "message": {"role": "user", "content": [{
                    "type": "future_provider_output", "content": null
                }]}
            }),
        ],
    );
    let source = discover_session(&projects, "fanout");
    let (core_only_scan, core_only, core_only_pro) =
        scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    let (fanout_scan, fanout_core, fanout_pro) =
        scan_owned(&source, None, ClaudeNativeProfile::CoreAndPro);

    assert!(core_only_pro.is_empty());
    assert_core_pages_equal(&core_only, &fanout_core);
    assert_eq!(
        core_only_scan.checkpoint.core_frontier(),
        fanout_scan.checkpoint.core_frontier()
    );
    assert_eq!(core_only_scan.stats.semantic_record_parses, 5);
    assert_eq!(fanout_scan.stats.semantic_record_parses, 5);
    assert_eq!(
        core_only_scan.stats.result_body_bytes_decoded_or_allocated,
        0
    );
    assert_eq!(core_only_scan.stats.result_hashes_created, 0);
    assert_eq!(core_only_scan.stats.result_previews_created, 0);
    assert_eq!(core_only_scan.stats.result_touches_created, 0);
    assert_eq!(core_only_scan.stats.result_fts_rows_created, 0);

    let core_rows = core_only
        .iter()
        .flat_map(|page| page.rows.iter())
        .collect::<Vec<_>>();
    let call = core_rows
        .iter()
        .find(|row| row.kind == ClaudeEventKind::ToolCall)
        .and_then(|row| row.tool_call.as_ref())
        .unwrap();
    assert_eq!(
        call.file_touches
            .iter()
            .map(|touch| touch.path.as_str())
            .collect::<Vec<_>>(),
        ["src/owned.rs"]
    );
    assert!(core_rows
        .iter()
        .flat_map(|row| row.tool_call.iter())
        .flat_map(|call| call.file_touches.iter())
        .all(|touch| touch.path != "leaked-output.rs"));
    assert!(core_rows
        .iter()
        .filter(|row| row.kind == ClaudeEventKind::ToolOutput)
        .all(|row| {
            row.body.is_none()
                && row.body_sha256.is_none()
                && row
                    .sparse_output
                    .as_ref()
                    .is_some_and(|diagnostic| diagnostic.call_id.as_deref() != Some("call-read"))
        }));

    let outputs = fanout_pro
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 5);
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.outcome.outcome)
            .collect::<Vec<_>>(),
        [
            OutputOutcome::Success,
            OutputOutcome::Failure,
            OutputOutcome::Failure,
            OutputOutcome::Timeout,
            OutputOutcome::Unknown,
        ]
    );
    assert_eq!(outputs[0].content, output_patch.as_bytes());
    assert_eq!(outputs[1].content, b"failed-a");
    assert!(outputs[2].content.is_empty());
    assert!(outputs[3].content.is_empty());
    assert!(outputs[4].content.is_empty());
    assert_eq!(outputs[0].call_id.as_deref(), Some("call-read"));
    assert_eq!(outputs[1].call_id.as_deref(), Some("call-a"));
    assert_eq!(outputs[2].call_id.as_deref(), Some("call-b"));
    assert_eq!(
        outputs
            .iter()
            .map(|output| {
                (
                    output.coordinate.source_record_ordinal.unwrap(),
                    output.coordinate.source_record_subrecord_index.unwrap(),
                )
            })
            .collect::<Vec<_>>(),
        [(1, 0), (2, 0), (2, 1), (3, 0), (4, 0)]
    );
    assert!(outputs.iter().all(|output| {
        output.associations.direct_session_id == "fanout"
            && output.associations.root_session_id == "fanout"
            && output.coordinate.byte_start.unwrap() < output.coordinate.byte_end_exclusive.unwrap()
    }));
    assert!(fanout_pro.iter().all(|page| {
        page.logical_units <= CLAUDE_MAX_PAGE_ROWS
            && page.outputs.len() <= CLAUDE_MAX_PAGE_ROWS
            && page.serialized_bytes <= CLAUDE_MAX_PAGE_BYTES
    }));
}

#[test]
fn pro_oversize_rejection_does_not_change_core_authority() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-oversize-pro", "oversize-pro");
    let body = "x".repeat(CLAUDE_MAX_PAGE_BYTES + 64 * 1024);
    write_lines(
        &path,
        &[json!({
            "sessionId": "oversize-pro",
            "type": "user",
            "uuid": "oversize-result",
            "message": {"content": [{
                "type": "tool_result", "tool_use_id": "call-big", "content": body
            }]},
            "toolUseResult": {"exitCode": 0}
        })],
    );
    let source = discover_session(&projects, "oversize-pro");
    let (core_scan, core, _) = scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    let (fanout_scan, fanout_core, pro) =
        scan_owned(&source, None, ClaudeNativeProfile::CoreAndPro);

    assert_core_pages_equal(&core, &fanout_core);
    assert_eq!(core_scan.rejections.total, fanout_scan.rejections.total);
    assert_eq!(core_scan.rejections.total, 0);
    assert_eq!(fanout_scan.pro_rejections.total, 1);
    assert_eq!(
        fanout_scan.pro_rejections.samples[0].kind,
        RejectionKind::OversizeProOutput
    );
    assert_eq!(pro.len(), 1);
    assert!(pro[0].outputs.is_empty());
    assert_eq!(pro[0].rejected_outputs, 1);
    assert!(pro[0].terminal);
    assert_eq!(
        pro[0].next_safe_frontier,
        fanout_scan.checkpoint.pro_frontier()
    );
}

#[test]
fn workflow_subagent_outputs_keep_exact_hierarchy_and_locator_coordinates() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = projects.join("-workflow/root/subagents/workflows/run-7/agent-worker.jsonl");
    write_lines(
        &path,
        &[json!({
            "sessionId": "root",
            "type": "user",
            "uuid": "workflow-result",
            "message": {"content": [{
                "type": "tool_result", "tool_use_id": "workflow-call", "content": "workflow-output"
            }]},
            "toolUseResult": {"exitCode": 0}
        })],
    );
    let source = discover_projects(&projects)
        .unwrap()
        .sessions
        .into_iter()
        .find(|source| source.path == path)
        .unwrap();
    let (_, _, pro) = scan_owned(&source, None, ClaudeNativeProfile::CoreAndPro);
    let output = &pro[0].outputs[0];
    assert_eq!(
        output.associations.direct_session_id,
        "root/subagents/workflows/run-7/agent-worker"
    );
    assert_eq!(output.associations.root_session_id, "root");
    assert_eq!(
        output.associations.parent_session_id.as_deref(),
        Some("root")
    );
    assert_eq!(
        output.associations.provider_session_id.as_deref(),
        Some("root/subagents/workflows/run-7/agent-worker")
    );
    assert_eq!(
        output.associations.agent_id.as_deref(),
        Some("agent-worker")
    );
    assert_eq!(
        output.coordinate.native_record_id.as_deref(),
        Some("workflow-result")
    );
    assert_eq!(output.coordinate.source_record_ordinal, Some(0));
    assert_eq!(output.coordinate.source_record_subrecord_index, Some(0));
    assert_eq!(output.coordinate.byte_start, Some(0));
    assert_eq!(
        output.coordinate.byte_end_exclusive,
        Some(fs::metadata(&path).unwrap().len())
    );
}

#[test]
fn page_receipts_restart_without_prefix_reparse_and_later_pro_replay_is_independent() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-restart", "restart");
    let records = (0..130)
        .map(|index| {
            if index % 5 == 4 {
                json!({
                    "sessionId": "restart",
                    "type": "user",
                    "uuid": format!("result-{index}"),
                    "message": {"content": [{
                        "type": "tool_result",
                        "tool_use_id": format!("call-{index}"),
                        "content": format!("output-{index}")
                    }]},
                    "toolUseResult": {"exitCode": 0}
                })
            } else {
                message(
                    "restart",
                    &format!("message-{index}"),
                    &format!("body-{index}"),
                )
            }
        })
        .collect::<Vec<_>>();
    write_lines(&path, &records);
    let source = discover_session(&projects, "restart");

    let mut scanner =
        ClaudeNativeScanner::new(source.clone(), None, ClaudeNativeProfile::CoreOnly).unwrap();
    let first = match scanner.next_page().unwrap().unwrap() {
        ClaudeNativeOwnedPage::Core(page) => *page,
        ClaudeNativeOwnedPage::Pro(_) => unreachable!(),
    };
    let failed_receipt = first.receipt();
    assert_eq!(failed_receipt, first.receipt());
    assert_eq!(failed_receipt.accepted_physical_records, 64);
    drop(scanner);

    let mut restarted = ClaudeNativeScanner::resume_page(
        source.clone(),
        failed_receipt.committed_frontier.clone(),
        first.session.clone(),
        ClaudeNativeProfile::CoreOnly,
    )
    .unwrap();
    let mut resumed_records = 0;
    while let Some(page) = restarted.next_page().unwrap() {
        let ClaudeNativeOwnedPage::Core(page) = page else {
            unreachable!()
        };
        assert_eq!(
            page.expected_frontier.next_raw_ordinal,
            64 + u64::try_from(resumed_records).unwrap()
        );
        resumed_records += page.logical_units;
    }
    let restarted_scan = restarted.finish().unwrap();
    assert_eq!(resumed_records, 66);
    assert_eq!(
        restarted_scan.stats.prefix_verification_bytes,
        failed_receipt.committed_frontier.complete_offset
    );
    assert_eq!(restarted_scan.stats.prefix_verification_records, 64);
    assert_eq!(restarted_scan.checkpoint.next_raw_ordinal, 130);

    let (core_only, _, _) = scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    assert!(!core_only.checkpoint.pro_initialized);
    let (pro_replay, replay_core, replay_pages) = scan_owned(
        &source,
        Some(&core_only.checkpoint),
        ClaudeNativeProfile::ProReplayOnly,
    );
    assert!(replay_core.is_empty());
    assert_eq!(
        replay_pages
            .iter()
            .flat_map(|page| page.outputs.iter())
            .count(),
        26
    );
    assert_eq!(
        pro_replay.checkpoint.core_frontier(),
        core_only.checkpoint.core_frontier()
    );
    assert_eq!(
        pro_replay.checkpoint.pro_frontier(),
        core_only.checkpoint.core_frontier()
    );
    assert!(pro_replay.checkpoint.pro_initialized);
    assert!(pro_replay.checkpoint.pro_terminal);

    let (pro_noop, noop_core, noop_pro) = scan_owned(
        &source,
        Some(&pro_replay.checkpoint),
        ClaudeNativeProfile::ProReplayOnly,
    );
    assert!(noop_core.is_empty());
    assert!(noop_pro.is_empty());
    assert!(pro_noop.stats.metadata_only_noop);
    assert_eq!(
        pro_noop.stats.source_bytes_read,
        pro_replay.checkpoint.pro_complete_offset
    );
    assert_eq!(pro_noop.stats.prefix_verification_records, 130);

    append_line(
        &path,
        &json!({
            "sessionId": "restart",
            "type": "user",
            "uuid": "result-append",
            "message": {"content": [{
                "type": "tool_result", "tool_use_id": "call-append", "content": "append-output"
            }]},
            "toolUseResult": {"exitCode": 0}
        }),
    );
    let appended_source = discover_session(&projects, "restart");
    let (_, appended_core_only, _) = scan_owned(
        &appended_source,
        Some(&pro_replay.checkpoint),
        ClaudeNativeProfile::CoreOnly,
    );
    let (combined_append, combined_core, combined_pro) = scan_owned(
        &appended_source,
        Some(&pro_replay.checkpoint),
        ClaudeNativeProfile::CoreAndPro,
    );
    assert_core_pages_equal(&appended_core_only, &combined_core);
    assert_eq!(
        combined_pro
            .iter()
            .flat_map(|page| page.outputs.iter())
            .count(),
        1
    );
    assert_eq!(
        combined_append.checkpoint.core_frontier(),
        combined_append.checkpoint.pro_frontier()
    );
}

#[test]
fn duplicate_critical_keys_are_local_and_complete_oversize_alone_advances() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-adversarial", "adversarial");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = File::create(&path).unwrap();
    writeln!(
        file,
        r#"{{"sessionId":"adversarial","sessionId":"other","type":"user","message":{{"content":"bad"}}}}"#
    )
    .unwrap();
    writeln!(file, "{}", message("adversarial", "good", "retained")).unwrap();
    file.flush().unwrap();
    let source = discover_session(&projects, "adversarial");
    let (scan, rows, _) = parse_collect(&source, None);
    assert_eq!(scan.rejections.total, 1);
    assert_eq!(scan.rejections.samples[0].source_record_ordinal, 0);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].identity.source_record_ordinal, 1);

    let oversize_path = session_path(&projects, "-oversize-tail", "oversize-tail");
    fs::create_dir_all(oversize_path.parent().unwrap()).unwrap();
    fs::write(
        &oversize_path,
        vec![b'x'; crate::MAX_PROVIDER_JSONL_LINE_BYTES + 1],
    )
    .unwrap();
    let first = parse_discard(&discover_session(&projects, "oversize-tail"), None);
    assert_eq!(first.checkpoint.next_raw_ordinal, 0);
    assert_eq!(first.checkpoint.complete_offset, 0);
    assert!(!first.checkpoint.terminal);
    assert!(first.incomplete_tail.is_some());

    let mut file = OpenOptions::new()
        .append(true)
        .open(&oversize_path)
        .unwrap();
    file.write_all(b"\n").unwrap();
    file.flush().unwrap();
    let second = parse_discard(
        &discover_session(&projects, "oversize-tail"),
        Some(&first.checkpoint),
    );
    assert_eq!(second.rejections.total, 1);
    assert_eq!(
        second.rejections.samples[0].kind,
        RejectionKind::OversizeRecord
    );
    assert_eq!(second.checkpoint.next_raw_ordinal, 1);
    assert_eq!(
        second.checkpoint.complete_offset,
        fs::metadata(&oversize_path).unwrap().len()
    );
    assert!(second.checkpoint.terminal);
}
