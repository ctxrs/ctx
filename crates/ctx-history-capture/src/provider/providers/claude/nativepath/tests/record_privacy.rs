use super::*;

#[test]
fn claude_2_1_219_tagged_and_result_families_are_excluded_before_retention() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project", "session");
    let secret = "NATIVE_RESULT_SECRET_DO_NOT_ALLOCATE_OR_HASH_\"\\\n".repeat(32 * 1024);
    let mut lines = vec![
        json!({
            "sessionId": "session",
            "type": "user",
            "uuid": "bash-native",
            "message": {
                "role": "user",
                "content": format!(
                    "<bash-stdout>{secret}</bash-stdout><bash-stderr>{secret}</bash-stderr>"
                )
            }
        }),
        json!({
            "sessionId": "session",
            "type": "user",
            "uuid": "bash-attribute-variant",
            "message": {
                "content": format!("<bash-stdout data-kind=\"persisted-output\">{secret}")
            }
        }),
        json!({
            "sessionId": "session",
            "type": "user",
            "uuid": "local-native",
            "message": {
                "content": format!(
                    "<local-command-stdout>{secret}</local-command-stdout>\
                     <local-command-stderr>{secret}</local-command-stderr>"
                )
            }
        }),
    ];
    let result_block_types = [
        "tool_result",
        "custom_tool_result",
        "server_tool_result",
        "mcp_tool_result",
        "search_result",
        "tool_search_tool_result",
        "web_search_result",
        "web_search_tool_result",
        "web_fetch_result",
        "web_fetch_tool_result",
        "bash_code_execution_result",
        "bash_code_execution_tool_result",
        "advisor_tool_result",
        "code_execution_tool_result",
        "text_editor_code_execution_tool_result",
        "future_provider_result",
        "future_provider_output",
    ];
    for (index, block_type) in result_block_types.into_iter().enumerate() {
        lines.push(json!({
            "sessionId": "session",
            "type": "user",
            "uuid": format!("result-block-{index}"),
            "message": {
                "content": [{
                    "type": block_type,
                    "content": secret,
                    "is_error": false
                }]
            }
        }));
    }
    let long_future_result_label = format!("{}result", "future_provider_".repeat(32));
    lines.push(json!({
        "sessionId": "session",
        "type": "user",
        "uuid": "long-future-result-block",
        "message": {
            "content": [{
                "type": long_future_result_label,
                "content": secret
            }]
        }
    }));
    lines.push(json!({
        "sessionId": "session",
        "type": "assistant",
        "uuid": "unknown-result-shape",
        "message": {
            "content": [{
                "type": "text",
                "text": "unsafe mixed sibling",
                "futureToolResult": {"payload": secret}
            }]
        }
    }));
    lines.push(json!({
        "sessionId": "session",
        "type": "assistant",
        "uuid": "top-level-result-shape",
        "toolUseResult": {"stderr": secret, "exitCode": 9},
        "message": {
            "content": [{"type": "text", "text": "unsafe result sibling"}]
        }
    }));
    lines.push(json!({
        "sessionId": "session",
        "type": "assistant",
        "uuid": "safe-mixed",
        "message": {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "safe retained text"},
                {
                    "type": "tool_use",
                    "id": "call-safe",
                    "name": "Read",
                    "input": {
                        "path": secret,
                        "result": secret,
                        "output_file": secret
                    }
                }
            ]
        }
    }));
    write_lines(&path, &lines);

    let (output, rows, pages) = parse_collect(&discover_session(&projects, "session"), None);
    assert_eq!(
        output.stats.native_result_records,
        3 + result_block_types.len() as u64 + 3
    );
    assert_eq!(output.stats.tagged_command_output_records, 3);
    assert_eq!(
        output.stats.result_block_records,
        result_block_types.len() as u64 + 1
    );
    assert_eq!(
        output.stats.result_like_shape_records,
        result_block_types.len() as u64 + 2
    );
    assert_eq!(output.stats.retention_pass_records, 1);
    assert_eq!(
        output.stats.preallocation_excluded_result_records,
        output.stats.native_result_records
    );
    assert!(output.stats.native_result_record_bytes > secret.len() as u64);
    assert_eq!(output.stats.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(output.stats.result_hashes_created, 0);
    assert_eq!(output.stats.result_previews_created, 0);
    assert_eq!(output.stats.result_touches_created, 0);
    assert_eq!(pages.len(), 1);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter()
            .find(|row| row.kind == ClaudeEventKind::Message)
            .and_then(|row| row.body.as_deref()),
        Some("safe retained text")
    );
    let tool_call = rows
        .iter()
        .find(|row| row.kind == ClaudeEventKind::ToolCall)
        .unwrap();
    assert_eq!(
        tool_call.tool_call.as_ref().unwrap().call_id.as_deref(),
        Some("call-safe")
    );
    assert!(tool_call.body.is_none());
    assert!(tool_call.body_sha256.is_none());
    let sparse_failure = rows
        .iter()
        .find(|row| row.kind == ClaudeEventKind::ToolOutput)
        .and_then(|row| row.sparse_output.as_ref())
        .unwrap();
    assert_eq!(sparse_failure.exit_code, Some(9));
    assert!(rows
        .iter()
        .filter(|row| row.kind == ClaudeEventKind::ToolOutput)
        .all(|row| row.body.is_none() && row.body_sha256.is_none()));
    assert!(rows
        .iter()
        .filter_map(|row| row.body.as_deref())
        .all(|body| !body.contains("NATIVE_RESULT_SECRET")));
}

#[test]
fn future_result_block_bodies_are_metadata_only_in_core_and_exact_in_pro() {
    use crate::OutputOutcome;

    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-future-results", "future-results");
    let success_body = format!(
        "FUTURE_SUCCESS_BODY_MUST_NOT_ALLOCATE_IN_CORE\n{}",
        "S".repeat(256 * 1024)
    );
    let failure_body = format!(
        "FUTURE_FAILURE_BODY_MUST_NOT_ALLOCATE_IN_CORE\n{}",
        "F".repeat(256 * 1024)
    );
    let timeout_body = format!(
        "FUTURE_TIMEOUT_BODY_MUST_NOT_ALLOCATE_IN_CORE\n{}",
        "T".repeat(256 * 1024)
    );
    write_lines(
        &path,
        &[
            json!({
                "sessionId": "future-results",
                "type": "user",
                "uuid": "future-success",
                "timestamp": "2026-07-25T00:00:00Z",
                "message": {"content": [{
                    "type": "future_provider_output",
                    "tool_use_id": "call-success",
                    "text": success_body
                }]},
                "toolUseResult": {"exitCode": 0}
            }),
            json!({
                "sessionId": "future-results",
                "type": "user",
                "uuid": "future-failure",
                "timestamp": "2026-07-25T00:00:01Z",
                "message": {"content": [{
                    "type": "future_provider_result",
                    "tool_use_id": "call-failure",
                    "text": failure_body,
                    "is_error": false
                }]},
                "toolUseResult": {"exitCode": 29, "is_error": false}
            }),
            json!({
                "sessionId": "future-results",
                "type": "user",
                "uuid": "future-timeout",
                "timestamp": "2026-07-25T00:00:02Z",
                "message": {"content": [{
                    "type": "future_provider_output",
                    "tool_use_id": "call-timeout",
                    "text": timeout_body
                }]},
                "toolUseResult": {"timedOut": true, "durationMs": 77}
            }),
        ],
    );
    let source = discover_session(&projects, "future-results");
    let (core_only, core_pages, core_pro_pages) =
        scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    let (combined, combined_core, pro_pages) =
        scan_owned(&source, None, ClaudeNativeProfile::CoreAndPro);

    assert!(core_pro_pages.is_empty());
    assert_core_pages_equal(&core_pages, &combined_core);
    assert_eq!(core_only.stats.native_result_records, 3);
    assert_eq!(core_only.stats.result_block_records, 3);
    assert_eq!(core_only.stats.preallocation_excluded_result_records, 3);
    assert_eq!(core_only.stats.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(core_only.stats.retained_body_bytes, 0);
    assert_eq!(core_only.stats.retained_body_hashes, 0);
    assert_eq!(core_only.stats.result_hashes_created, 0);
    assert_eq!(core_only.stats.result_previews_created, 0);
    assert_eq!(core_only.stats.result_touches_created, 0);
    assert_eq!(core_only.stats.result_fts_rows_created, 0);
    assert_eq!(core_only.stats.retention_pass_records, 0);

    let core_rows = core_pages
        .iter()
        .flat_map(|page| page.rows.iter())
        .collect::<Vec<_>>();
    assert_eq!(core_rows.len(), 2);
    assert!(core_rows.iter().all(|row| {
        row.body.is_none()
            && row.body_sha256.is_none()
            && row.tool_call.is_none()
            && row.sparse_output.is_some()
    }));
    assert_eq!(
        core_rows
            .iter()
            .map(|row| {
                let sparse = row.sparse_output.as_ref().unwrap();
                (
                    sparse.call_id.as_deref(),
                    sparse.outcome.clone(),
                    sparse.exit_code,
                    sparse.duration_ms,
                )
            })
            .collect::<Vec<_>>(),
        [
            (
                Some("call-failure"),
                ClaudeOutputOutcome::Failure,
                Some(29),
                None,
            ),
            (
                Some("call-timeout"),
                ClaudeOutputOutcome::Timeout,
                None,
                Some(77),
            ),
        ]
    );
    assert!(core_rows
        .iter()
        .filter_map(|row| row.body.as_deref())
        .all(|body| !body.contains("FUTURE_")));

    let outputs = pro_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 3);
    assert_eq!(outputs[0].content, success_body.as_bytes());
    assert_eq!(outputs[1].content, failure_body.as_bytes());
    assert_eq!(outputs[2].content, timeout_body.as_bytes());
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.outcome.outcome)
            .collect::<Vec<_>>(),
        [
            OutputOutcome::Success,
            OutputOutcome::Failure,
            OutputOutcome::Timeout,
        ]
    );
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.call_id.as_deref())
            .collect::<Vec<_>>(),
        [
            Some("call-success"),
            Some("call-failure"),
            Some("call-timeout"),
        ]
    );
    assert_eq!(
        combined.stats.result_body_bytes_decoded_or_allocated,
        u64::try_from(success_body.len() + failure_body.len() + timeout_body.len()).unwrap()
    );
    assert_eq!(combined.stats.preallocation_excluded_result_records, 0);
}

#[test]
fn camel_case_is_error_is_preclassified_before_core_body_allocation() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-camel-error", "camel-error");
    let sentinel = format!(
        "CAMEL_IS_ERROR_SENTINEL\n*** Update File: must-not-touch.rs\n{}",
        "X".repeat(256 * 1024)
    );
    write_lines(
        &path,
        &[json!({
            "sessionId": "camel-error",
            "type": "user",
            "uuid": "camel-result",
            "message": {"content": {
                "type": "text",
                "isError": false,
                "text": sentinel
            }}
        })],
    );
    let source = discover_session(&projects, "camel-error");
    let (core, core_pages, _) = scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    let (_, combined_core, pro_pages) = scan_owned(&source, None, ClaudeNativeProfile::CoreAndPro);
    assert_core_pages_equal(&core_pages, &combined_core);

    assert_eq!(core.stats.native_result_records, 1);
    assert_eq!(core.stats.result_like_shape_records, 1);
    assert_eq!(core.stats.preallocation_excluded_result_records, 1);
    assert_eq!(core.stats.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(core.stats.retained_body_bytes, 0);
    assert_eq!(core.stats.retained_body_hashes, 0);
    assert_eq!(core.stats.result_hashes_created, 0);
    assert_eq!(core.stats.result_previews_created, 0);
    assert_eq!(core.stats.result_touches_created, 0);
    assert_eq!(core.stats.result_fts_rows_created, 0);
    assert_eq!(core.stats.retention_pass_records, 0);
    assert!(core_pages.iter().all(|page| page.rows.is_empty()));

    let outputs = pro_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].content, sentinel.as_bytes());
    assert_eq!(outputs[0].call_id, None);
}

#[test]
fn singular_nested_and_array_result_content_hydrates_exactly() {
    use crate::OutputOutcome;

    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-result-content", "result-content");
    write_lines(
        &path,
        &[
            json!({
                "sessionId": "result-content",
                "type": "user",
                "uuid": "singular-result",
                "message": {"content": {
                    "type": "future_provider_output",
                    "tool_use_id": "call-singular",
                    "content": "SINGULAR_FUTURE_PROVIDER_OUTPUT"
                }},
                "toolUseResult": {"exitCode": 0}
            }),
            json!({
                "sessionId": "result-content",
                "type": "user",
                "uuid": "nested-result",
                "message": {"content": [[{
                    "type": "future_provider_output",
                    "toolUseId": "call-nested",
                    "content": "NESTED_FUTURE_PROVIDER_OUTPUT"
                }]]},
                "toolUseResult": {"exitCode": 17, "durationMs": 23}
            }),
            json!({
                "sessionId": "result-content",
                "type": "user",
                "uuid": "array-result",
                "message": {"content": [{
                    "type": "tool_result",
                    "tool_use_id": "call-array",
                    "content": "ORDINARY_ARRAY_OUTPUT"
                }]},
                "toolUseResult": {"timedOut": true, "durationMs": 41}
            }),
        ],
    );
    let source = discover_session(&projects, "result-content");
    let (core, core_pages, _) = scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    let (combined, combined_core, pro_pages) =
        scan_owned(&source, None, ClaudeNativeProfile::CoreAndPro);
    assert_core_pages_equal(&core_pages, &combined_core);
    assert_eq!(core.stats.native_result_records, 3);
    assert_eq!(core.stats.preallocation_excluded_result_records, 3);
    assert_eq!(core.stats.result_body_bytes_decoded_or_allocated, 0);

    let sparse = core_pages
        .iter()
        .flat_map(|page| page.rows.iter())
        .filter_map(|row| row.sparse_output.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(sparse.len(), 2);
    assert_eq!(sparse[0].call_id.as_deref(), Some("call-nested"));
    assert_eq!(sparse[0].outcome, ClaudeOutputOutcome::Failure);
    assert_eq!(sparse[0].exit_code, Some(17));
    assert_eq!(sparse[0].duration_ms, Some(23));
    assert_eq!(sparse[1].call_id.as_deref(), Some("call-array"));
    assert_eq!(sparse[1].outcome, ClaudeOutputOutcome::Timeout);
    assert_eq!(sparse[1].duration_ms, Some(41));

    let outputs = pro_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 3);
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.content.as_slice())
            .collect::<Vec<_>>(),
        [
            b"SINGULAR_FUTURE_PROVIDER_OUTPUT".as_slice(),
            b"NESTED_FUTURE_PROVIDER_OUTPUT".as_slice(),
            b"ORDINARY_ARRAY_OUTPUT".as_slice(),
        ]
    );
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.call_id.as_deref())
            .collect::<Vec<_>>(),
        [
            Some("call-singular"),
            Some("call-nested"),
            Some("call-array"),
        ]
    );
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.outcome.outcome)
            .collect::<Vec<_>>(),
        [
            OutputOutcome::Success,
            OutputOutcome::Failure,
            OutputOutcome::Timeout,
        ]
    );
    assert_eq!(
        outputs
            .iter()
            .map(|output| {
                (
                    output.coordinate.native_record_id.as_deref(),
                    output.coordinate.source_record_ordinal,
                    output.coordinate.source_record_subrecord_index,
                )
            })
            .collect::<Vec<_>>(),
        [
            (Some("singular-result"), Some(0), Some(0)),
            (Some("nested-result"), Some(1), Some(0)),
            (Some("array-result"), Some(2), Some(0)),
        ]
    );
    assert_eq!(
        combined.stats.result_body_bytes_decoded_or_allocated,
        u64::try_from(
            "SINGULAR_FUTURE_PROVIDER_OUTPUT".len()
                + "NESTED_FUTURE_PROVIDER_OUTPUT".len()
                + "ORDINARY_ARRAY_OUTPUT".len()
        )
        .unwrap()
    );
    assert!(pro_pages.iter().all(|page| {
        page.logical_units <= CLAUDE_MAX_PAGE_ROWS
            && page.outputs.len() <= CLAUDE_MAX_PAGE_ROWS
            && page.serialized_bytes <= CLAUDE_MAX_PAGE_BYTES
    }));
}

#[test]
fn escaped_result_syntax_is_preclassified_without_content_deserialization() {
    let tagged = br#"{"message":{"content":"\u003cbash-stdout\u003esecret\nvalue"}}"#;
    let tagged = super::super::privacy::preclassify_result(tagged)
        .unwrap()
        .unwrap();
    assert!(tagged.tagged_command_output);

    let future = br#"{"message":{"content":[{"type":"future\u005fprovider\u005fresult","content":"secret\nvalue"}]}}"#;
    let future = super::super::privacy::preclassify_result(future)
        .unwrap()
        .unwrap();
    assert!(future.result_block);
}
