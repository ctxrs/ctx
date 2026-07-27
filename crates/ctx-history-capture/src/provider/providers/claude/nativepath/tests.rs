use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support_paths::tempdir;

fn projects_root(root: &Path) -> PathBuf {
    root.join(".claude/projects")
}

fn session_path(projects: &Path, project: &str, session: &str) -> PathBuf {
    projects.join(project).join(format!("{session}.jsonl"))
}

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = BufWriter::new(File::create(path).unwrap());
    for line in lines {
        writeln!(writer, "{line}").unwrap();
    }
    writer.flush().unwrap();
}

fn append_line(path: &Path, line: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
}

fn message(session: &str, uuid: &str, text: &str) -> Value {
    json!({
        "sessionId": session,
        "type": "user",
        "uuid": uuid,
        "timestamp": "2026-01-01T00:00:00.000Z",
        "cwd": "/workspace/project",
        "version": "2.1.219",
        "gitBranch": "main",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }
    })
}

fn discover_session(projects: &Path, session: &str) -> DiscoveredClaudeSession {
    discover_projects(projects)
        .unwrap()
        .sessions
        .into_iter()
        .find(|source| source.key.root_session_id == session && source.key.agent_id.is_none())
        .unwrap()
}

fn parse_collect(
    source: &DiscoveredClaudeSession,
    previous: Option<&ParseCheckpoint>,
) -> (ParseOutput, Vec<ClaudeRetainedRow>, Vec<(usize, usize)>) {
    let mut rows = Vec::new();
    let mut pages = Vec::new();
    let output = parse_session(source, previous, |page| {
        pages.push((page.rows.len(), page.estimated_bytes));
        rows.extend(page.rows);
        Ok(())
    })
    .unwrap();
    (output, rows, pages)
}

fn parse_discard(
    source: &DiscoveredClaudeSession,
    previous: Option<&ParseCheckpoint>,
) -> ParseOutput {
    parse_session(source, previous, |_| Ok(())).unwrap()
}

fn scan_owned(
    source: &DiscoveredClaudeSession,
    previous: Option<&ParseCheckpoint>,
    profile: ClaudeNativeProfile,
) -> (
    ParseOutput,
    Vec<ClaudeNativePage>,
    Vec<ClaudeNativeProOutputPage>,
) {
    let mut scanner = ClaudeNativeScanner::new(source.clone(), previous, profile).unwrap();
    let mut core = Vec::new();
    let mut pro = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        match page {
            ClaudeNativeOwnedPage::Core(page) => core.push(*page),
            ClaudeNativeOwnedPage::Pro(page) => pro.push(*page),
        }
    }
    (scanner.finish().unwrap(), core, pro)
}

fn assert_core_pages_equal(left: &[ClaudeNativePage], right: &[ClaudeNativePage]) {
    assert_eq!(left.len(), right.len());
    for (left, right) in left.iter().zip(right) {
        assert_eq!(left.session, right.session);
        assert_eq!(left.expected_frontier, right.expected_frontier);
        assert_eq!(left.next_safe_frontier, right.next_safe_frontier);
        assert_eq!(left.rows, right.rows);
        assert_eq!(left.rejections, right.rejections);
        assert_eq!(left.rejected_records, right.rejected_records);
        assert_eq!(left.logical_units, right.logical_units);
        assert_eq!(left.serialized_bytes, right.serialized_bytes);
        assert_eq!(left.terminal, right.terminal);
        assert_eq!(left.certificate, right.certificate);
        assert_eq!(left.identity, right.identity);
    }
}

#[test]
fn discovery_accepts_exact_direct_and_workflow_subagents_in_stable_order() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let primary_b = session_path(&projects, "-project-b", "session-b");
    let primary_a = session_path(&projects, "-project-a", "session-a");
    let direct = projects.join("-project-a/session-a/subagents/agent-review.jsonl");
    let workflow_a =
        projects.join("-project-a/session-a/subagents/workflows/run-a/agent-worker.jsonl");
    let workflow_z =
        projects.join("-project-a/session-a/subagents/workflows/run-z/agent-worker.jsonl");
    let lookalikes = [
        projects.join("-project-a/session-a/subagents/review.jsonl"),
        projects.join("-project-a/session-a/subagents/workflow/run-a/agent-fake.jsonl"),
        projects.join("-project-a/session-a/subagents/workflows/agent-loose.jsonl"),
        projects.join("-project-a/session-a/subagents/workflows/run-a/not-agent.jsonl"),
        projects.join("-project-a/session-a/subagents/workflows/run-a/nested/agent-too-deep.jsonl"),
    ];
    for path in [&primary_b, &primary_a, &direct, &workflow_a, &workflow_z]
        .into_iter()
        .chain(lookalikes.iter())
    {
        write_lines(path, &[json!({})]);
    }

    let discovery = discover_projects(&projects).unwrap();
    assert_eq!(discovery.stats.project_directories, 2);
    assert_eq!(discovery.stats.selected_sessions, 5);
    assert_eq!(
        discovery
            .sessions
            .iter()
            .map(|source| source.key.provider_session_id())
            .collect::<Vec<_>>(),
        [
            "session-a",
            "session-a/subagents/agent-review",
            "session-a/subagents/workflows/run-a/agent-worker",
            "session-a/subagents/workflows/run-z/agent-worker",
            "session-b",
        ]
    );
    assert_eq!(discovery.sessions[0].layout, SessionLayout::Primary);
    assert_eq!(discovery.sessions[1].layout, SessionLayout::Subagent);
    assert_eq!(
        discovery.sessions[2].layout,
        SessionLayout::WorkflowSubagent
    );
    for source in &discovery.sessions[1..4] {
        assert_eq!(source.key.parent_provider_session_id(), Some("session-a"));
    }
    assert_eq!(
        discovery.sessions[2].key.workflow_run_id.as_deref(),
        Some("run-a")
    );
    assert!(lookalikes
        .iter()
        .all(|path| discovery.sessions.iter().all(|source| &source.path != path)));
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinks_in_approved_layouts_and_ignores_symlink_lookalikes() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let primary = session_path(&projects, "-project", "session");
    write_lines(&primary, &[json!({})]);
    let real = temp.path().join("real.jsonl");
    write_lines(&real, &[json!({})]);
    let lookalike_dir = projects.join("-project/session/subagents/workflow-lookalike");
    fs::create_dir_all(lookalike_dir.parent().unwrap()).unwrap();
    symlink(temp.path(), &lookalike_dir).unwrap();
    assert_eq!(discover_projects(&projects).unwrap().sessions.len(), 1);

    let selected = projects.join("-project/session/subagents/workflows/run/agent-selected.jsonl");
    fs::create_dir_all(selected.parent().unwrap()).unwrap();
    symlink(&real, &selected).unwrap();
    let error = discover_projects(&projects).unwrap_err();
    assert!(error
        .to_string()
        .contains("symlinked workflow subagent files"));
    fs::remove_file(&selected).unwrap();

    let primary_two = session_path(&projects, "-project", "session-two");
    write_lines(&primary_two, &[json!({})]);
    let selected_session_dir = projects.join("-project/session-two");
    symlink(temp.path(), &selected_session_dir).unwrap();
    let error = discover_projects(&projects).unwrap_err();
    assert!(error.to_string().contains("symlinked session directories"));
}

#[test]
fn discovery_has_deterministic_directory_and_total_traversal_bounds() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let project = projects.join("-project");
    fs::create_dir_all(&project).unwrap();
    for index in 0..=super::source::CLAUDE_MAX_DIRECTORY_ENTRIES {
        fs::write(project.join(format!("ignored-{index:05}.txt")), b"").unwrap();
    }
    let error = discover_projects(&projects).unwrap_err();
    assert!(error.to_string().contains("directory exceeds"));
}

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
    let tagged = super::privacy::preclassify_result(tagged).unwrap().unwrap();
    assert!(tagged.tagged_command_output);

    let future = br#"{"message":{"content":[{"type":"future\u005fprovider\u005fresult","content":"secret\nvalue"}]}}"#;
    let future = super::privacy::preclassify_result(future).unwrap().unwrap();
    assert!(future.result_block);
}

#[test]
fn malformed_records_advance_order_and_incomplete_tail_resumes_at_boundary() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project", "session");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let tail = message("session", "tail", "completed tail").to_string();
    let split = tail.len() - 4;
    let mut file = File::create(&path).unwrap();
    writeln!(file, "{}", message("session", "first", "before malformed")).unwrap();
    writeln!(file, "{{malformed").unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "sessionId": "session",
            "type": "user",
            "message": {"content": [{"type": "tool_result", "content": "discarded"}]}
        })
    )
    .unwrap();
    file.write_all(&tail.as_bytes()[..split]).unwrap();
    file.flush().unwrap();

    let (first, first_rows, _) = parse_collect(&discover_session(&projects, "session"), None);
    assert_eq!(first_rows.len(), 1);
    assert_eq!(first_rows[0].identity.source_record_ordinal, 0);
    assert_eq!(first.rejections.total, 1);
    assert_eq!(
        first.rejections.samples[0].kind,
        RejectionKind::MalformedJson
    );
    assert_eq!(first.rejections.samples[0].source_record_ordinal, 1);
    assert_eq!(first.stats.native_result_records, 1);
    assert_eq!(first.checkpoint.next_raw_ordinal, 3);
    assert!(!first.checkpoint.terminal);
    assert!(first.incomplete_tail.is_some());
    let checkpoint_bytes = serde_json::to_vec(&first.checkpoint).unwrap();
    let checkpoint: ParseCheckpoint = serde_json::from_slice(&checkpoint_bytes).unwrap();
    assert_eq!(checkpoint, first.checkpoint);

    let unchanged = parse_discard(&discover_session(&projects, "session"), Some(&checkpoint));
    assert_eq!(unchanged.change, ChangeSignal::Unchanged);
    assert!(unchanged.stats.metadata_only_noop);
    assert_eq!(
        unchanged.stats.source_bytes_read,
        checkpoint.complete_offset
    );
    assert_eq!(
        unchanged.stats.prefix_verification_bytes,
        checkpoint.complete_offset
    );
    assert_eq!(unchanged.stats.prefix_verification_records, 3);
    assert_eq!(unchanged.stats.semantic_record_parses, 0);
    assert!(unchanged.incomplete_tail.is_some());

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&tail.as_bytes()[split..]).unwrap();
    file.write_all(b"\n").unwrap();
    file.flush().unwrap();
    let (second, second_rows, _) =
        parse_collect(&discover_session(&projects, "session"), Some(&checkpoint));
    assert_eq!(second.change, ChangeSignal::Append);
    assert_eq!(second_rows.len(), 1);
    assert_eq!(second_rows[0].identity.source_record_ordinal, 3);
    assert_eq!(second.checkpoint.next_raw_ordinal, 4);
    assert!(second.checkpoint.terminal);
    assert!(second.incomplete_tail.is_none());
}

#[test]
fn core_advance_does_not_bless_a_stale_nonterminal_pro_lane() {
    use crate::OutputOutcome;

    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-core-first", "core-first");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let result_body = "CORE_FIRST_FUTURE_OUTPUT";
    let tail = json!({
        "sessionId": "core-first",
        "type": "user",
        "uuid": "future-result",
        "timestamp": "2026-07-25T00:00:01Z",
        "message": {"content": [{
            "type": "future_provider_output",
            "tool_use_id": "future-call",
            "content": result_body
        }]},
        "toolUseResult": {"exitCode": 17, "is_error": false}
    })
    .to_string();
    let mut file = File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        message("core-first", "prefix", "committed prefix")
    )
    .unwrap();
    file.write_all(tail.as_bytes()).unwrap();
    file.flush().unwrap();

    let initial_source = discover_session(&projects, "core-first");
    let (initial, _, _) = scan_owned(&initial_source, None, ClaudeNativeProfile::CoreAndPro);
    assert!(!initial.checkpoint.terminal);
    assert!(!initial.checkpoint.pro_terminal);
    assert_eq!(
        initial.checkpoint.core_frontier(),
        initial.checkpoint.pro_frontier()
    );

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"\n").unwrap();
    file.flush().unwrap();
    let completed_source = discover_session(&projects, "core-first");
    let current_observation = completed_source.fingerprint.observation_sha256();

    let (core_advanced, core_pages, pro_pages) = scan_owned(
        &completed_source,
        Some(&initial.checkpoint),
        ClaudeNativeProfile::CoreOnly,
    );
    assert!(pro_pages.is_empty());
    assert_eq!(core_advanced.change, ChangeSignal::Append);
    assert!(!core_advanced.stats.metadata_only_noop);
    assert_eq!(core_advanced.stats.semantic_record_parses, 1);
    assert_eq!(
        core_advanced.stats.prefix_verification_bytes,
        initial.checkpoint.complete_offset
    );
    let sparse = core_pages
        .iter()
        .flat_map(|page| page.rows.iter())
        .find_map(|row| row.sparse_output.as_ref())
        .unwrap();
    assert_eq!(sparse.outcome, ClaudeOutputOutcome::Failure);
    assert_eq!(sparse.exit_code, Some(17));
    assert!(core_advanced.checkpoint.terminal);
    assert!(!core_advanced.checkpoint.pro_terminal);
    assert_eq!(
        core_advanced.checkpoint.pro_frontier(),
        initial.checkpoint.pro_frontier()
    );
    assert_eq!(
        core_advanced.checkpoint.pro_observation_sha256,
        initial.checkpoint.pro_observation_sha256
    );
    assert_eq!(
        core_advanced.checkpoint.observation_sha256,
        current_observation
    );
    assert_ne!(
        core_advanced.checkpoint.observation_sha256,
        core_advanced.checkpoint.pro_observation_sha256
    );
    assert!(core_advanced.checkpoint.core_observation_binding_matches());
    assert!(core_advanced.checkpoint.pro_observation_binding_matches());

    let (pro_replayed, replay_core, replay_pro) = scan_owned(
        &completed_source,
        Some(&core_advanced.checkpoint),
        ClaudeNativeProfile::ProReplayOnly,
    );
    assert!(replay_core.is_empty());
    assert_eq!(pro_replayed.change, ChangeSignal::Append);
    assert!(!pro_replayed.stats.metadata_only_noop);
    assert_eq!(pro_replayed.stats.semantic_record_parses, 1);
    assert_eq!(
        pro_replayed.stats.prefix_verification_bytes,
        initial.checkpoint.pro_complete_offset
    );
    let outputs = replay_pro
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].content, result_body.as_bytes());
    assert_eq!(outputs[0].outcome.outcome, OutputOutcome::Failure);
    assert_eq!(
        pro_replayed.checkpoint.core_frontier(),
        pro_replayed.checkpoint.pro_frontier()
    );
    assert!(pro_replayed.checkpoint.terminal);
    assert!(pro_replayed.checkpoint.pro_terminal);
    assert_eq!(
        pro_replayed.checkpoint.pro_observation_sha256,
        current_observation
    );
}

#[test]
fn pro_advance_does_not_bless_a_stale_nonterminal_core_lane() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-pro-first", "pro-first");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let result_body = "PRO_FIRST_FUTURE_OUTPUT";
    let tail = json!({
        "sessionId": "pro-first",
        "type": "user",
        "uuid": "future-result",
        "timestamp": "2026-07-25T00:00:01Z",
        "message": {"content": [{
            "type": "future_provider_result",
            "tool_use_id": "future-call",
            "content": result_body
        }]},
        "toolUseResult": {"timedOut": true, "durationMs": 91}
    })
    .to_string();
    let mut file = File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        message("pro-first", "prefix", "committed prefix")
    )
    .unwrap();
    file.write_all(tail.as_bytes()).unwrap();
    file.flush().unwrap();

    let initial_source = discover_session(&projects, "pro-first");
    let (initial, _, _) = scan_owned(&initial_source, None, ClaudeNativeProfile::CoreAndPro);
    assert!(!initial.checkpoint.terminal);
    assert!(!initial.checkpoint.pro_terminal);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"\n").unwrap();
    file.flush().unwrap();
    let completed_source = discover_session(&projects, "pro-first");
    let current_observation = completed_source.fingerprint.observation_sha256();

    let (pro_advanced, core_pages, pro_pages) = scan_owned(
        &completed_source,
        Some(&initial.checkpoint),
        ClaudeNativeProfile::ProReplayOnly,
    );
    assert!(core_pages.is_empty());
    assert_eq!(pro_advanced.change, ChangeSignal::Append);
    assert!(!pro_advanced.stats.metadata_only_noop);
    assert_eq!(pro_advanced.stats.semantic_record_parses, 1);
    assert_eq!(
        pro_advanced.stats.prefix_verification_bytes,
        initial.checkpoint.pro_complete_offset
    );
    let outputs = pro_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].content, result_body.as_bytes());
    assert!(pro_advanced.checkpoint.pro_terminal);
    assert!(!pro_advanced.checkpoint.terminal);
    assert_eq!(
        pro_advanced.checkpoint.core_frontier(),
        initial.checkpoint.core_frontier()
    );
    assert_eq!(
        pro_advanced.checkpoint.observation_sha256,
        initial.checkpoint.observation_sha256
    );
    assert_eq!(
        pro_advanced.checkpoint.pro_observation_sha256,
        current_observation
    );
    assert_ne!(
        pro_advanced.checkpoint.observation_sha256,
        pro_advanced.checkpoint.pro_observation_sha256
    );

    let (core_replayed, replay_core, replay_pro) = scan_owned(
        &completed_source,
        Some(&pro_advanced.checkpoint),
        ClaudeNativeProfile::CoreOnly,
    );
    assert!(replay_pro.is_empty());
    assert_eq!(core_replayed.change, ChangeSignal::Append);
    assert!(!core_replayed.stats.metadata_only_noop);
    assert_eq!(core_replayed.stats.semantic_record_parses, 1);
    assert_eq!(
        core_replayed.stats.prefix_verification_bytes,
        initial.checkpoint.complete_offset
    );
    let sparse = replay_core
        .iter()
        .flat_map(|page| page.rows.iter())
        .find_map(|row| row.sparse_output.as_ref())
        .unwrap();
    assert_eq!(sparse.outcome, ClaudeOutputOutcome::Timeout);
    assert_eq!(sparse.duration_ms, Some(91));
    assert_eq!(
        core_replayed.checkpoint.core_frontier(),
        core_replayed.checkpoint.pro_frontier()
    );
    assert!(core_replayed.checkpoint.terminal);
    assert!(core_replayed.checkpoint.pro_terminal);
    assert_eq!(
        core_replayed.checkpoint.observation_sha256,
        current_observation
    );
}

#[test]
fn corrupt_current_observation_cannot_bless_a_stale_nonterminal_lane() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-corrupt-lane", "corrupt-lane");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let result_body = "CORRUPT_STALE_LANE_OUTPUT";
    let tail = json!({
        "sessionId": "corrupt-lane",
        "type": "user",
        "uuid": "future-result",
        "message": {"content": [{
            "type": "future_provider_output",
            "content": result_body
        }]},
        "toolUseResult": {"exitCode": 0}
    })
    .to_string();
    let mut file = File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        message("corrupt-lane", "prefix", "committed prefix")
    )
    .unwrap();
    file.write_all(tail.as_bytes()).unwrap();
    file.flush().unwrap();

    let initial_source = discover_session(&projects, "corrupt-lane");
    let (initial, _, _) = scan_owned(&initial_source, None, ClaudeNativeProfile::CoreAndPro);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"\n").unwrap();
    file.flush().unwrap();
    let completed_source = discover_session(&projects, "corrupt-lane");
    let (core_advanced, _, _) = scan_owned(
        &completed_source,
        Some(&initial.checkpoint),
        ClaudeNativeProfile::CoreOnly,
    );

    let mut corrupt = core_advanced.checkpoint;
    corrupt.pro_observed_file_len = completed_source.fingerprint.len;
    corrupt.pro_observation_sha256 = completed_source.fingerprint.observation_sha256();
    assert!(!corrupt.pro_terminal);
    assert!(!corrupt.pro_observation_binding_matches());

    let (reparsed, core_pages, pro_pages) = scan_owned(
        &completed_source,
        Some(&corrupt),
        ClaudeNativeProfile::ProReplayOnly,
    );
    assert!(core_pages.is_empty());
    assert_eq!(reparsed.change, ChangeSignal::Reparse);
    assert!(!reparsed.stats.metadata_only_noop);
    assert_eq!(reparsed.stats.prefix_verification_bytes, 0);
    assert_eq!(reparsed.stats.semantic_record_parses, 2);
    let outputs = pro_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].content, result_body.as_bytes());
    assert!(reparsed.checkpoint.pro_observation_binding_matches());
}

#[test]
fn event_identity_and_order_include_excluded_and_rejected_physical_records() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project", "session");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "sessionId": "session",
            "type": "user",
            "message": {"content": [{"type": "tool_result", "content": "output"}]}
        })
    )
    .unwrap();
    writeln!(file, "{{malformed").unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "sessionId": "session",
            "type": "assistant",
            "uuid": "mixed",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "safe text"},
                    {"type": "tool_use", "id": "call-a", "name": "Read", "input": {"path": "a"}},
                    {"type": "tool_use", "id": "call-b", "name": "Edit", "input": {"path": "b"}}
                ]
            }
        })
    )
    .unwrap();
    file.flush().unwrap();

    let (output, rows, _) = parse_collect(&discover_session(&projects, "session"), None);
    assert_eq!(
        rows.iter()
            .map(|row| (
                row.identity.source_record_ordinal,
                row.identity.source_subrecord_index
            ))
            .collect::<Vec<_>>(),
        [(2, 0), (2, 1), (2, 2)]
    );
    assert!(rows
        .windows(2)
        .all(|rows| rows[0].native_order < rows[1].native_order));
    assert_eq!(output.checkpoint.next_raw_ordinal, 3);
}

#[test]
fn rejection_samples_are_bounded_while_the_aggregate_is_exact() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project", "session");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = BufWriter::new(File::create(&path).unwrap());
    for index in 0..100 {
        writeln!(writer, "{{malformed-{index}").unwrap();
    }
    writer.flush().unwrap();

    let output = parse_discard(&discover_session(&projects, "session"), None);
    assert_eq!(output.rejections.total, 100);
    assert_eq!(
        output.rejections.samples.len(),
        CLAUDE_MAX_REJECTION_SAMPLES
    );
    assert_eq!(output.stats.malformed_records, 100);
}

#[test]
fn c0_baseline_shape_retains_seventeen_rows_and_subagent_identity() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let primary = session_path(&projects, "-project", "root-session");
    let subagent = projects.join("-project/root-session/subagents/agent-review.jsonl");
    let primary_records = (0..10)
        .map(|index| c0_record(index, "root-session"))
        .collect::<Vec<_>>();
    let subagent_records = (10..20)
        .map(|index| c0_record(index, "root-session"))
        .collect::<Vec<_>>();
    write_lines(&primary, &primary_records);
    write_lines(&subagent, &subagent_records);

    let discovery = discover_projects(&projects).unwrap();
    let mut retained = 0_u64;
    let mut results = 0_u64;
    let mut subagent_key = None;
    for source in &discovery.sessions {
        let output = parse_session(source, None, |page| {
            retained += page.rows.len() as u64;
            Ok(())
        })
        .unwrap();
        results += output.stats.native_result_records;
        if source.layout == SessionLayout::Subagent {
            subagent_key = Some(output.session.key);
        }
    }
    assert_eq!(retained, 17);
    assert_eq!(results, 3);
    assert_eq!(
        subagent_key.unwrap().agent_id.as_deref(),
        Some("agent-review")
    );
}

fn c0_record(index: usize, session: &str) -> Value {
    let kind = index % 6;
    let content = match kind {
        0 | 1 | 4 | 5 => json!([{
            "type": "text",
            "text": format!("conversation-{index}")
        }]),
        2 => json!([{
            "type": "tool_use",
            "id": format!("call-{index}"),
            "name": "Bash",
            "input": {"command": "printf nativepath"}
        }]),
        3 => json!([{
            "type": "tool_result",
            "tool_use_id": format!("call-{}", index - 1),
            "content": format!("output-{index}")
        }]),
        _ => unreachable!(),
    };
    json!({
        "sessionId": session,
        "type": if kind == 0 || kind == 3 { "user" } else if kind == 4 { "system" } else { "assistant" },
        "uuid": format!("record-{index}"),
        "message": {"content": content}
    })
}

#[test]
fn append_rewrite_truncation_replacement_relocation_and_copy_are_distinguished() {
    assert_eq!(mutation_signal(Mutation::Append), ChangeSignal::Append);
    assert_eq!(mutation_signal(Mutation::Rewrite), ChangeSignal::Rewrite);
    assert_eq!(
        mutation_signal(Mutation::Truncate),
        ChangeSignal::Truncation
    );
    assert_eq!(
        mutation_signal(Mutation::ReplaceShort),
        ChangeSignal::Replacement
    );
    assert_eq!(
        mutation_signal(Mutation::Relocate),
        ChangeSignal::Relocation
    );
    assert_eq!(mutation_signal(Mutation::Copy), ChangeSignal::LiveCopy);
    assert_eq!(
        mutation_signal(Mutation::ConflictingCopy),
        ChangeSignal::ConflictingLiveCopy
    );
    assert_eq!(
        mutation_output(Mutation::Append).lifecycle,
        ClaudeSourceLifecycle::Append
    );
    assert_eq!(
        mutation_output(Mutation::Rewrite).lifecycle,
        ClaudeSourceLifecycle::Rewrite
    );
    assert_eq!(
        mutation_output(Mutation::Truncate).lifecycle,
        ClaudeSourceLifecycle::Rewind
    );
    assert_eq!(
        mutation_output(Mutation::ReplaceShort).lifecycle,
        ClaudeSourceLifecycle::Replacement
    );
    assert_eq!(
        mutation_output(Mutation::Relocate).lifecycle,
        ClaudeSourceLifecycle::Move
    );
    assert_eq!(
        mutation_output(Mutation::Copy).lifecycle,
        ClaudeSourceLifecycle::Copy
    );
    assert_eq!(
        mutation_output(Mutation::ConflictingCopy).lifecycle,
        ClaudeSourceLifecycle::Ambiguous
    );
}

#[derive(Clone, Copy)]
enum Mutation {
    Append,
    Rewrite,
    Truncate,
    ReplaceShort,
    Relocate,
    Copy,
    ConflictingCopy,
}

fn mutation_signal(mutation: Mutation) -> ChangeSignal {
    mutation_output(mutation).change
}

fn mutation_output(mutation: Mutation) -> ParseOutput {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project-a", "session");
    write_lines(
        &path,
        &[
            message("session", "one", &"1".repeat(2_048)),
            message("session", "two", &"2".repeat(2_048)),
        ],
    );
    let first_source = discover_session(&projects, "session");
    let first = parse_discard(&first_source, None);
    assert_eq!(first.lifecycle, ClaudeSourceLifecycle::New);

    let current_path = match mutation {
        Mutation::Append => {
            append_line(&path, &message("session", "three", "333333"));
            path.clone()
        }
        Mutation::Rewrite => {
            write_lines(
                &path,
                &[
                    message("session", "one", &"A".repeat(4_096)),
                    message("session", "two", &"B".repeat(4_096)),
                ],
            );
            path.clone()
        }
        Mutation::Truncate => {
            OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(8)
                .unwrap();
            path.clone()
        }
        Mutation::ReplaceShort => {
            let replacement = path.with_extension("replacement");
            write_lines(&replacement, &[message("session", "replacement", "short")]);
            fs::rename(&replacement, &path).unwrap();
            path.clone()
        }
        Mutation::Relocate => {
            let relocated = session_path(&projects, "-project-b", "session");
            fs::create_dir_all(relocated.parent().unwrap()).unwrap();
            fs::rename(&path, &relocated).unwrap();
            relocated
        }
        Mutation::Copy => {
            let copy = session_path(&projects, "-project-b", "session");
            fs::create_dir_all(copy.parent().unwrap()).unwrap();
            fs::copy(&path, &copy).unwrap();
            copy
        }
        Mutation::ConflictingCopy => {
            let copy = session_path(&projects, "-project-b", "session");
            write_lines(
                &copy,
                &[message("session", "different-copy", "not the same source")],
            );
            copy
        }
    };
    let current = discover_projects(&projects)
        .unwrap()
        .sessions
        .into_iter()
        .find(|source| source.path == current_path)
        .unwrap();
    parse_discard(&current, Some(&first.checkpoint))
}

#[test]
fn no_op_and_append_verify_the_full_prefix_while_parsing_only_delta() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project", "session");
    write_lines(
        &path,
        &[
            message("session", "one", &"1".repeat(40_000)),
            message("session", "two", &"2".repeat(40_000)),
        ],
    );
    let first_source = discover_session(&projects, "session");
    let first_len = first_source.fingerprint.len;
    let first = parse_discard(&first_source, None);
    assert_eq!(first.stats.parsed_source_bytes, first_len);
    assert_eq!(first.stats.source_bytes_read, first_len);
    assert_eq!(first.stats.prefix_verification_bytes, 0);
    assert_eq!(first.stats.prefix_verification_records, 0);

    let unchanged = parse_discard(
        &discover_session(&projects, "session"),
        Some(&first.checkpoint),
    );
    assert_eq!(unchanged.change, ChangeSignal::Unchanged);
    assert_eq!(unchanged.lifecycle, ClaudeSourceLifecycle::Replay);
    assert!(unchanged.stats.metadata_only_noop);
    assert_eq!(unchanged.stats.source_bytes_read, first_len);
    assert_eq!(unchanged.stats.parsed_source_bytes, 0);
    assert_eq!(unchanged.stats.prefix_verification_bytes, first_len);
    assert_eq!(unchanged.stats.prefix_verification_records, 2);
    assert_eq!(unchanged.stats.semantic_record_parses, 0);

    let before_append = fs::metadata(&path).unwrap().len();
    append_line(&path, &message("session", "three", "append-tail"));
    let after_append = fs::metadata(&path).unwrap().len();
    let appended = parse_discard(
        &discover_session(&projects, "session"),
        Some(&first.checkpoint),
    );
    assert_eq!(appended.change, ChangeSignal::Append);
    assert_eq!(appended.lifecycle, ClaudeSourceLifecycle::Append);
    assert_eq!(
        appended.stats.parsed_source_bytes,
        after_append - before_append
    );
    assert_eq!(
        appended.stats.prefix_verification_bytes,
        first.checkpoint.complete_offset
    );
    assert_eq!(appended.stats.prefix_verification_records, 2);
    assert_eq!(appended.stats.semantic_record_parses, 1);
    assert_eq!(
        appended.stats.source_bytes_read,
        appended.stats.parsed_source_bytes + appended.stats.prefix_verification_bytes
    );

    write_lines(
        &path,
        &[
            message("session", "rewrite-one", &"A".repeat(80_000)),
            message("session", "rewrite-two", &"B".repeat(80_000)),
        ],
    );
    let rewritten_len = fs::metadata(&path).unwrap().len();
    let rewritten = parse_discard(
        &discover_session(&projects, "session"),
        Some(&appended.checkpoint),
    );
    assert_eq!(rewritten.change, ChangeSignal::Rewrite);
    assert_eq!(rewritten.lifecycle, ClaudeSourceLifecycle::Rewrite);
    assert_eq!(rewritten.stats.parsed_source_bytes, rewritten_len);
    assert!(rewritten.stats.prefix_verification_bytes > 64 * 1024);
    assert_eq!(
        rewritten.stats.source_bytes_read,
        rewritten.stats.parsed_source_bytes + rewritten.stats.prefix_verification_bytes
    );
}

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

#[test]
fn complete_inventory_alone_authorizes_deletion_candidates_and_pages_are_certified() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let first_path = session_path(&projects, "-inventory", "first");
    let second_path = session_path(&projects, "-inventory", "second");
    write_lines(&first_path, &[message("first", "first-1", "one")]);
    write_lines(&second_path, &[message("second", "second-1", "two")]);
    let initial = discover_projects(&projects).unwrap();
    assert!(initial.inventory.complete);
    initial.revalidate_inventory().unwrap();
    let checkpoints = initial
        .sessions
        .iter()
        .map(|source| parse_discard(source, None).checkpoint)
        .collect::<Vec<_>>();

    fs::remove_file(&second_path).unwrap();
    let current = discover_projects(&projects).unwrap();
    let candidates = authoritative_deletion_candidates(&current, &checkpoints).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].lifecycle,
        ClaudeSourceLifecycle::DeletionCandidate
    );
    assert_eq!(candidates[0].session_key.root_session_id, "second");
    assert!(candidates[0].inventory.complete);

    let source = discover_session(&projects, "first");
    let (_, pages, _) = scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    assert_eq!(pages.len(), 1);
    assert_eq!(
        pages[0].certificate.certified_prefix_end,
        pages[0].next_safe_frontier.complete_offset
    );
    assert_eq!(
        pages[0].certificate.certified_prefix_chain_sha256,
        pages[0].next_safe_frontier.complete_record_chain_sha256
    );

    append_line(&first_path, &message("first", "first-2", "changed"));
    assert!(matches!(
        current.revalidate_inventory(),
        Err(ClaudeNativePathError::InventoryChanged { .. })
    ));
    assert!(authoritative_deletion_candidates(&current, &checkpoints).is_err());
}

#[test]
fn each_emitted_page_is_certified_and_later_source_mutation_blocks_completion() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-certification", "certification");
    let records = (0..65)
        .map(|index| {
            message(
                "certification",
                &format!("message-{index}"),
                &format!("body-{index}"),
            )
        })
        .collect::<Vec<_>>();
    write_lines(&path, &records);
    let source = discover_session(&projects, "certification");
    let mut scanner =
        ClaudeNativeScanner::new(source, None, ClaudeNativeProfile::CoreOnly).unwrap();
    let first = match scanner.next_page().unwrap().unwrap() {
        ClaudeNativeOwnedPage::Core(page) => *page,
        ClaudeNativeOwnedPage::Pro(_) => unreachable!(),
    };
    assert_eq!(first.logical_units, 64);
    assert_eq!(
        first.certificate.certified_prefix_end,
        first.next_safe_frontier.complete_offset
    );

    append_line(
        &path,
        &message("certification", "late-message", "must invalidate finish"),
    );
    let error = loop {
        match scanner.next_page() {
            Ok(Some(_)) => continue,
            Ok(None) => panic!("mutated Claude source unexpectedly completed"),
            Err(error) => break error,
        }
    };
    assert!(matches!(error, ClaudeNativePathError::SourceChanged { .. }));
}

#[test]
fn more_than_eight_thousand_rows_stream_in_bounded_pages() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-scale", "scale");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = BufWriter::new(File::create(&path).unwrap());
    let body = "conversation-body ".repeat(160);
    for index in 0..9_001 {
        writeln!(
            writer,
            "{}",
            message("scale", &format!("message-{index}"), &body)
        )
        .unwrap();
    }
    writer.flush().unwrap();

    let source = discover_session(&projects, "scale");
    let mut page_count = 0_usize;
    let mut row_count = 0_usize;
    let output = parse_session(&source, None, |page| {
        page_count += 1;
        row_count += page.rows.len();
        assert!(page.rows.len() <= CLAUDE_MAX_PAGE_ROWS);
        assert!(page.estimated_bytes <= CLAUDE_MAX_PAGE_BYTES);
        Ok(())
    })
    .unwrap();
    assert_eq!(output.stats.complete_records, 9_001);
    assert_eq!(row_count, 9_001);
    assert!(page_count >= 3);
    assert_eq!(output.stats.emitted_pages, page_count as u64);
    assert_eq!(output.stats.emitted_rows, row_count as u64);
    assert!(output.stats.peak_page_rows <= CLAUDE_MAX_PAGE_ROWS);
    assert!(output.stats.peak_page_bytes <= CLAUDE_MAX_PAGE_BYTES);
    assert_eq!(output.rejections.total, 0);
}

#[test]
fn top_level_success_and_unknown_results_never_project_content_into_core() {
    use crate::OutputOutcome;

    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-result-policy", "result-policy");
    write_lines(
        &path,
        &[
            json!({
                "sessionId": "result-policy",
                "type": "tool_result",
                "uuid": "success",
                "content": "SUCCESS-RESULT-SECRET",
                "toolUseResult": {"exitCode": 0}
            }),
            json!({
                "sessionId": "result-policy",
                "type": "tool_result",
                "uuid": "unknown",
                "content": "UNKNOWN-RESULT-SECRET"
            }),
            json!({
                "sessionId": "result-policy",
                "type": "tool_result",
                "uuid": "failure",
                "content": "FAILURE-RESULT-SECRET",
                "toolUseResult": {"exitCode": 7, "is_error": false}
            }),
            message("result-policy", "message", "retained conversation"),
        ],
    );
    let source = discover_session(&projects, "result-policy");
    let (core_only, core_pages, _) = scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    let (_, combined_core, pro_pages) = scan_owned(&source, None, ClaudeNativeProfile::CoreAndPro);
    assert_core_pages_equal(&core_pages, &combined_core);

    let rows = core_pages
        .iter()
        .flat_map(|page| page.rows.iter())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .filter_map(|row| row.body.as_deref())
            .collect::<Vec<_>>(),
        ["retained conversation"]
    );
    let failure = rows
        .iter()
        .find_map(|row| row.sparse_output.as_ref())
        .unwrap();
    assert_eq!(failure.outcome, ClaudeOutputOutcome::Failure);
    assert_eq!(failure.exit_code, Some(7));
    assert!(rows.iter().all(|row| {
        row.body.as_deref() != Some("SUCCESS-RESULT-SECRET")
            && row.body.as_deref() != Some("UNKNOWN-RESULT-SECRET")
            && row.body.as_deref() != Some("FAILURE-RESULT-SECRET")
            && row.tool_call.is_none()
    }));
    assert_eq!(core_only.stats.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(core_only.stats.result_hashes_created, 0);
    assert_eq!(core_only.stats.result_previews_created, 0);
    assert_eq!(core_only.stats.result_touches_created, 0);
    assert_eq!(core_only.stats.result_fts_rows_created, 0);

    let outputs = pro_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 3);
    assert_eq!(outputs[0].content, b"SUCCESS-RESULT-SECRET");
    assert_eq!(outputs[0].outcome.outcome, OutputOutcome::Success);
    assert_eq!(outputs[1].content, b"UNKNOWN-RESULT-SECRET");
    assert_eq!(outputs[1].outcome.outcome, OutputOutcome::Unknown);
    assert_eq!(outputs[2].content, b"FAILURE-RESULT-SECRET");
    assert_eq!(outputs[2].outcome.outcome, OutputOutcome::Failure);
    assert_eq!(outputs[2].outcome.exit_code, Some(7));
}

#[test]
fn structural_preflight_is_profile_invariant_and_precedes_pro_hydration() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-preflight", "preflight");
    let too_many_blocks = (0..65)
        .map(|index| {
            json!({
                "type": "tool_result",
                "tool_use_id": format!("call-{index}"),
                "content": "BLOCK-MUST-NOT-BE-PRO-HYDRATED"
            })
        })
        .collect::<Vec<_>>();
    write_lines(
        &path,
        &[
            json!({
                "sessionId": "x".repeat(4 * 1024 + 1),
                "type": "tool_result",
                "uuid": "oversize-session",
                "content": "MUST-NOT-BE-PRO-HYDRATED",
                "toolUseResult": {"exitCode": 0}
            }),
            json!({
                "sessionId": "wrong-session",
                "type": "tool_result",
                "uuid": "mismatch",
                "content": "MISMATCH-MUST-NOT-PUBLISH",
                "toolUseResult": {"exitCode": 0}
            }),
            json!({
                "sessionId": "preflight",
                "type": "user",
                "uuid": "too-many-blocks",
                "message": {"content": too_many_blocks},
                "toolUseResult": {"exitCode": 0}
            }),
            message("preflight", "valid", "valid body"),
        ],
    );
    let source = discover_session(&projects, "preflight");
    let (core_only, core_pages, _) = scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    let (combined, combined_core, pro_pages) =
        scan_owned(&source, None, ClaudeNativeProfile::CoreAndPro);
    assert_core_pages_equal(&core_pages, &combined_core);
    assert_eq!(core_only.rejections, combined.rejections);
    assert_eq!(core_only.rejections.total, 3);
    assert_eq!(
        core_only
            .rejections
            .samples
            .iter()
            .map(|rejection| rejection.kind)
            .collect::<Vec<_>>(),
        [
            RejectionKind::MalformedJson,
            RejectionKind::SessionIdentityMismatch,
            RejectionKind::MalformedJson,
        ]
    );
    assert!(pro_pages.iter().all(|page| page.outputs.is_empty()));
    assert_eq!(combined.stats.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(core_only.stats.semantic_record_parses, 4);
    assert_eq!(combined.stats.semantic_record_parses, 4);
}

#[test]
fn same_size_early_rewrite_with_identical_tail_is_rejected_before_delta_pages() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-prefix", "prefix");
    let original = [
        message("prefix", "one", &"A".repeat(40_000)),
        message("prefix", "two", &"B".repeat(40_000)),
        message("prefix", "three", &"C".repeat(40_000)),
    ];
    write_lines(&path, &original);
    let first_source = discover_session(&projects, "prefix");
    let first = parse_discard(&first_source, None);
    let original_len = first_source.fingerprint.len;

    let rewritten = [
        message("prefix", "one", &"Z".repeat(40_000)),
        original[1].clone(),
        original[2].clone(),
    ];
    write_lines(&path, &rewritten);
    assert_eq!(fs::metadata(&path).unwrap().len(), original_len);
    let source = discover_session(&projects, "prefix");
    let mut scanner = ClaudeNativeScanner::new(
        source,
        Some(&first.checkpoint),
        ClaudeNativeProfile::CoreOnly,
    )
    .unwrap();
    let page = match scanner.next_page().unwrap().unwrap() {
        ClaudeNativeOwnedPage::Core(page) => *page,
        ClaudeNativeOwnedPage::Pro(_) => unreachable!(),
    };
    assert_eq!(page.expected_frontier.complete_offset, 0);
    while scanner.next_page().unwrap().is_some() {}
    let output = scanner.finish().unwrap();
    assert_eq!(output.change, ChangeSignal::Rewrite);
    assert_eq!(output.stats.prefix_verification_bytes, original_len);
    assert_eq!(output.stats.prefix_verification_records, 3);
    assert_eq!(output.stats.parsed_source_bytes, original_len);
    assert_eq!(output.stats.semantic_record_parses, 3);
}

#[test]
fn pro_only_revision_upgrade_does_not_stamp_preserved_core_current() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-revisions", "revisions");
    write_lines(
        &path,
        &[
            message("revisions", "message", "body"),
            json!({
                "sessionId": "revisions",
                "type": "tool_result",
                "uuid": "result",
                "content": "output",
                "toolUseResult": {"exitCode": 0}
            }),
        ],
    );
    let source = discover_session(&projects, "revisions");
    let (baseline, _, _) = scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    let mut old = baseline.checkpoint;
    old.parser_revision = 3;
    old.policy_revision = 3;
    old.pro_initialized = false;
    old.pro_parser_revision = 0;
    old.pro_policy_revision = 0;

    let (pro_upgrade, core_pages, pro_pages) =
        scan_owned(&source, Some(&old), ClaudeNativeProfile::ProReplayOnly);
    assert!(core_pages.is_empty());
    assert_eq!(
        pro_pages
            .iter()
            .flat_map(|page| page.outputs.iter())
            .count(),
        1
    );
    assert_eq!(pro_upgrade.checkpoint.parser_revision, 3);
    assert_eq!(pro_upgrade.checkpoint.policy_revision, 3);
    assert_eq!(
        pro_upgrade.checkpoint.pro_parser_revision,
        super::checkpoint::CLAUDE_NATIVEPATH_PARSER_REVISION
    );
    assert_eq!(
        pro_upgrade.checkpoint.pro_policy_revision,
        super::checkpoint::CLAUDE_NATIVEPATH_POLICY_REVISION
    );

    let (core_upgrade, core_pages, _) = scan_owned(
        &source,
        Some(&pro_upgrade.checkpoint),
        ClaudeNativeProfile::CoreOnly,
    );
    assert_eq!(core_upgrade.change, ChangeSignal::Reparse);
    assert!(!core_pages.is_empty());
    assert_eq!(core_upgrade.stats.semantic_record_parses, 2);
    assert!(core_upgrade.checkpoint.core_revisions_match());
    assert!(core_upgrade.checkpoint.pro_revisions_match());
}

#[test]
fn queued_core_sibling_is_revalidated_after_pro_return() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-siblings", "siblings");
    write_lines(
        &path,
        &[json!({
            "sessionId": "siblings",
            "type": "tool_result",
            "uuid": "result",
            "content": "output",
            "toolUseResult": {"exitCode": 0}
        })],
    );
    let source = discover_session(&projects, "siblings");
    let mut scanner =
        ClaudeNativeScanner::new(source, None, ClaudeNativeProfile::CoreAndPro).unwrap();
    assert!(matches!(
        scanner.next_page().unwrap().unwrap(),
        ClaudeNativeOwnedPage::Pro(_)
    ));
    append_line(
        &path,
        &message("siblings", "late", "must invalidate sibling"),
    );
    let error = scanner.next_page().unwrap_err();
    assert!(matches!(error, ClaudeNativePathError::SourceChanged { .. }));
}

#[test]
fn pro_page_identity_binds_every_claim_family() {
    use crate::{
        OutputCommandContext, OutputObservationKind, OutputOutcome, OutputRepositoryContext,
    };

    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-identity", "identity");
    write_lines(
        &path,
        &[json!({
            "sessionId": "identity",
            "type": "tool_result",
            "uuid": "result",
            "timestamp": "2026-01-01T00:00:00Z",
            "content": "output",
            "toolUseResult": {"exitCode": 0, "durationMs": 5}
        })],
    );
    let source = discover_session(&projects, "identity");
    let (_, _, mut pages) = scan_owned(&source, None, ClaudeNativeProfile::CoreAndPro);
    let page = &mut pages[0];
    let mut prior = super::reader::pro_page_identity_for_test(page).unwrap();
    assert_eq!(prior, page.identity);
    macro_rules! binds {
        ($mutation:expr) => {{
            $mutation;
            let next = super::reader::pro_page_identity_for_test(page).unwrap();
            assert_ne!(next, prior);
            prior = next;
        }};
    }

    binds!(page.outputs[0].kind = OutputObservationKind::Command);
    binds!(page.outputs[0].coordinate.unit_key.push('x'));
    binds!(page.outputs[0].coordinate.native_sequence += 1);
    binds!(page.outputs[0]
        .coordinate
        .native_record_id
        .as_mut()
        .unwrap()
        .push('x'));
    binds!(page.outputs[0].coordinate.source_record_ordinal = Some(9));
    binds!(page.outputs[0].coordinate.source_record_subrecord_index = Some(9));
    binds!(page.outputs[0].coordinate.byte_start = Some(9));
    binds!(page.outputs[0].coordinate.byte_end_exclusive = Some(99));
    binds!(page.outputs[0].occurred_at_unix_ms = Some(9));
    binds!(page.outputs[0].associations.direct_session_id.push('x'));
    binds!(page.outputs[0].associations.root_session_id.push('x'));
    binds!(page.outputs[0].associations.parent_session_id = Some("parent".to_owned()));
    binds!(page.outputs[0].associations.provider_session_id = Some("provider".to_owned()));
    binds!(page.outputs[0].associations.agent_id = Some("agent".to_owned()));
    binds!(
        page.outputs[0].associations.repository = Some(OutputRepositoryContext {
            repository_id: "repository".to_owned(),
            checkout_id: Some("checkout".to_owned()),
            worktree_id: Some("worktree".to_owned()),
            object_format: Some("sha256".to_owned()),
        })
    );
    binds!(page.outputs[0]
        .associations
        .repository
        .as_mut()
        .unwrap()
        .repository_id
        .push('x'));
    binds!(page.outputs[0]
        .associations
        .repository
        .as_mut()
        .unwrap()
        .checkout_id
        .as_mut()
        .unwrap()
        .push('x'));
    binds!(page.outputs[0]
        .associations
        .repository
        .as_mut()
        .unwrap()
        .worktree_id
        .as_mut()
        .unwrap()
        .push('x'));
    binds!(page.outputs[0]
        .associations
        .repository
        .as_mut()
        .unwrap()
        .object_format
        .as_mut()
        .unwrap()
        .push('x'));
    binds!(page.outputs[0].call_id = Some("call".to_owned()));
    binds!(
        page.outputs[0].command = Some(OutputCommandContext {
            tool_name: "tool".to_owned(),
            command: "command".to_owned(),
            working_directory: Some("cwd".to_owned()),
        })
    );
    binds!(page.outputs[0]
        .command
        .as_mut()
        .unwrap()
        .tool_name
        .push('x'));
    binds!(page.outputs[0].command.as_mut().unwrap().command.push('x'));
    binds!(page.outputs[0]
        .command
        .as_mut()
        .unwrap()
        .working_directory
        .as_mut()
        .unwrap()
        .push('x'));
    binds!(page.outputs[0].outcome.outcome = OutputOutcome::Failure);
    binds!(page.outputs[0].outcome.exit_code = Some(7));
    binds!(page.outputs[0].outcome.duration_ms = Some(99));
    binds!(page.outputs[0].locator.version += 1);
    binds!(page.outputs[0].locator.kind.push('x'));
    binds!(page.outputs[0].locator.payload.push(1));
    binds!(page.outputs[0].content.push(1));
    binds!(page.rejected_outputs += 1);
    binds!(page.logical_units += 1);
    binds!(page.rejections.push(RecordRejection {
        kind: RejectionKind::OversizeProOutput,
        source_record_ordinal: 0,
        locator: ClaudePhysicalLocator {
            path: path.clone(),
            byte_start: 0,
            byte_end_exclusive: 1,
            line_number: 1,
        },
        diagnostic: "identity rejection".to_owned(),
    }));
    binds!(page.rejections[0].kind = RejectionKind::TooManyResultSubrecords);
    binds!(page.rejections[0].source_record_ordinal += 1);
    binds!(page.rejections[0].locator.path.push("x"));
    binds!(page.rejections[0].locator.byte_start += 1);
    binds!(page.rejections[0].locator.byte_end_exclusive += 1);
    binds!(page.rejections[0].locator.line_number += 1);
    binds!(page.rejections[0].diagnostic.push('x'));
    binds!(page.expected_frontier.complete_offset += 1);
    binds!(page.expected_frontier.next_raw_ordinal += 1);
    binds!(page.expected_frontier.complete_record_chain_sha256[0] ^= 1);
    binds!(page.expected_frontier.boundary_proof_len += 1);
    binds!(page.expected_frontier.boundary_proof_sha256[0] ^= 1);
    binds!(page.expected_frontier.native_identity_chain_sha256[0] ^= 1);
    binds!(page.expected_frontier.native_identity_records += 1);
    binds!(
        page.expected_frontier.appendable_boundary = !page.expected_frontier.appendable_boundary
    );
    binds!(page.next_safe_frontier.complete_offset += 1);
    binds!(page.certificate.canonical_route.push("x"));
    binds!(page.certificate.observation_sha256[0] ^= 1);
    binds!(page.certificate.physical_file_id = None);
    binds!(page.certificate.certified_prefix_end += 1);
    binds!(page.certificate.certified_prefix_chain_sha256[0] ^= 1);
    binds!(page.terminal = !page.terminal);
    binds!(page.outputs.pop());
    let _ = prior;
}
