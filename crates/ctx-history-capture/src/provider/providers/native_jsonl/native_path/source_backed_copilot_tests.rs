use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use ctx_history_core::{McpToolCallAttribution, MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES};
use serde_json::{json, Value};

use super::*;
use crate::provider::source_backed::family::jsonl::{
    probe_first_record, JsonlOversizedRecordPolicy, JsonlReader, JsonlSourceIdentity,
};

struct MaterializedCopilotFixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
}

fn fixture_root() -> MaterializedCopilotFixture {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider-history/copilot-cli/v1")
        .canonicalize()
        .unwrap();
    let temporary = crate::test_support_paths::tempdir().unwrap();
    let destination = temporary.path().join("v1");
    copy_fixture_tree(&source, &destination);
    MaterializedCopilotFixture {
        _temporary: temporary,
        root: destination,
    }
}

fn copy_fixture_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.metadata().unwrap().is_dir() {
            copy_fixture_tree(&source_path, &destination_path);
        } else {
            fs::copy(source_path, destination_path).unwrap();
        }
    }
}

fn project_copilot_root(root: &Path) -> Vec<CoreRecord> {
    try_project_copilot_root(root).unwrap()
}

fn try_project_copilot_root(root: &Path) -> Result<Vec<CoreRecord>> {
    let adapter = super::super::copilot_source_backed_adapter();
    let inventory = adapter.discover(root)?;
    assert!(!inventory.leaves().is_empty());
    assert!(inventory.rejected_leaves().is_empty());
    let mut records = Vec::new();
    for leaf in inventory.leaves() {
        let source_file = leaf.open_verified()?;
        let mut projector = JsonlFamilyAdapter::projector(
            &adapter,
            leaf,
            Arc::clone(&source_file),
            DateTime::<Utc>::UNIX_EPOCH,
        )?;
        let (_, probe) = probe_first_record(leaf.source_path(), &source_file, |_| {
            Ok::<_, CaptureError>(())
        })?;
        let identity = JsonlSourceIdentity::new(
            adapter.provider().as_str(),
            adapter.parser_revision(),
            "copilot-projection-test",
            [0; 32],
            leaf.source_path(),
        );
        let mut reader = JsonlReader::open(identity, source_file, None, Some(probe))?;
        reader.set_oversized_record_policy(JsonlFamilyAdapter::oversized_record_policy(&adapter));
        while reader
            .visit_page(&mut |record| -> Result<()> {
                projector.project(record, &mut |record| {
                    records.push(record);
                    Ok(())
                })
            })?
            .is_some()
        {}
        projector.finish_projecting(&mut |record| {
            records.push(record);
            Ok(())
        })?;
    }
    Ok(records)
}

fn write_session(root: &Path, directory: &str, session_id: &str, events: &[String]) {
    let session = root.join(directory);
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("events.jsonl"),
        session_bytes(session_id, events),
    )
    .unwrap();
}

fn session_bytes(session_id: &str, events: &[String]) -> Vec<u8> {
    let mut lines = vec![json!({
        "type": "session.start",
        "id": format!("{session_id}-header"),
        "timestamp": "2026-08-03T12:00:00Z",
        "data": {
            "sessionId": session_id,
            "startTime": "2026-08-03T12:00:00Z",
            "context": {"cwd": "/workspace/sanitized"}
        }
    })
    .to_string()];
    lines.extend_from_slice(events);
    (lines.join("\n") + "\n").into_bytes()
}

fn project_events(events: &[String]) -> Vec<CoreRecord> {
    let temp = crate::test_support_paths::tempdir().unwrap();
    write_session(temp.path(), "session", "generated-session", events);
    project_copilot_root(temp.path())
}

fn project_events_through_shared_scanner(events: &[String]) -> Vec<CoreRecord> {
    let temp = crate::test_support_paths::tempdir().unwrap();
    write_session(temp.path(), "session", "generated-session", events);
    project_copilot_root(temp.path())
}

fn linkage_plan(
    events: &[String],
    limits: super::copilot::CopilotLinkageLimits,
) -> super::copilot::CopilotMcpToolCallAttributions {
    let temp = crate::test_support_paths::tempdir().unwrap();
    write_session(temp.path(), "session", "generated-session", events);
    let adapter = super::super::copilot_source_backed_adapter();
    let inventory = adapter.discover(temp.path()).unwrap();
    assert_eq!(inventory.leaves().len(), 1);
    let source_file = inventory.leaves()[0].open_verified().unwrap();
    super::copilot::copilot_mcp_tool_call_attributions_with_limits(&source_file, limits).unwrap()
}

fn start(native_id: &str, call_id: &str, server: &str, tool: &str) -> String {
    json!({
        "type": "tool.execution_start",
        "id": native_id,
        "timestamp": "2026-08-03T12:00:01Z",
        "data": {
            "toolCallId": call_id,
            "mcpServerName": server,
            "mcpToolName": tool
        }
    })
    .to_string()
}

fn completion(native_id: &str, call_id: &str, success: bool) -> String {
    json!({
        "type": "tool.execution_complete",
        "id": native_id,
        "timestamp": "2026-08-03T12:00:02Z",
        "data": {"toolCallId": call_id, "success": success}
    })
    .to_string()
}

fn record_by_native_id<'a>(records: &'a [CoreRecord], expected: &str) -> &'a CoreRecord {
    records
        .iter()
        .find(|record| native_id(record) == expected)
        .unwrap_or_else(|| panic!("missing Copilot fixture record {expected}"))
}

fn native_id(record: &CoreRecord) -> &str {
    let Some(TypedKey::Composite(parts)) = record.native_event_id.as_ref() else {
        panic!("Copilot fixture record has no composite native identity");
    };
    let Some(TypedKey::Utf8(native_id)) = parts.first() else {
        panic!("Copilot fixture record has no native event id");
    };
    native_id
}

fn attribution(server: &str, tool: &str) -> Option<McpToolCallAttribution> {
    Some(McpToolCallAttribution {
        server: server.to_owned(),
        tool: tool.to_owned(),
    })
}

fn rewrite_race_events(revision: &str) -> Vec<String> {
    vec![
        start(
            "before-start",
            "before-call",
            "before-server",
            "before-tool",
        ),
        completion("before-complete", "before-call", true),
        start(
            &format!("raced-start-{revision}"),
            &format!("raced-call-{revision}"),
            &format!("raced-server-{revision}"),
            &format!("raced-tool-{revision}"),
        ),
        completion(
            &format!("raced-complete-{revision}"),
            &format!("raced-call-{revision}"),
            false,
        ),
        start("after-start", "after-call", "after-server", "after-tool"),
        completion("after-complete", "after-call", false),
    ]
}

fn assert_same_object_rewrite_omits_stale_attribution(
    install_hook: impl FnOnce(&Path, Vec<u8>, Arc<AtomicBool>),
) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let session_id = "rewrite-session";
    let original = rewrite_race_events("a");
    let replacement = rewrite_race_events("b");
    write_session(temp.path(), "session", session_id, &original);
    let path = temp.path().join("session/events.jsonl");
    let before_metadata = fs::metadata(&path).unwrap();
    let replacement_bytes = session_bytes(session_id, &replacement);
    assert_eq!(before_metadata.len(), replacement_bytes.len() as u64);

    let rewrite_ran = Arc::new(AtomicBool::new(false));
    install_hook(&path, replacement_bytes, Arc::clone(&rewrite_ran));
    let error = try_project_copilot_root(temp.path()).unwrap_err();
    assert!(
        rewrite_ran.load(Ordering::SeqCst),
        "rewrite hook did not run"
    );
    assert!(matches!(error, CaptureError::SourceChangedDuringCapture));

    let after_metadata = fs::metadata(&path).unwrap();
    assert_eq!(after_metadata.len(), before_metadata.len());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        assert_eq!(after_metadata.dev(), before_metadata.dev());
        assert_eq!(after_metadata.ino(), before_metadata.ino());
    }

    let records = project_copilot_root(temp.path());
    assert_eq!(records.len(), replacement.len());
    assert!(records
        .iter()
        .all(|record| !native_id(record).ends_with("-a")));
    for expected in [
        "before-start",
        "before-complete",
        "raced-start-b",
        "raced-complete-b",
        "after-start",
        "after-complete",
    ] {
        assert!(records.iter().any(|record| native_id(record) == expected));
    }
    assert_eq!(
        record_by_native_id(&records, "before-complete").mcp_tool_call,
        attribution("before-server", "before-tool")
    );
    assert_eq!(
        record_by_native_id(&records, "raced-complete-b").mcp_tool_call,
        attribution("raced-server-b", "raced-tool-b")
    );
    assert_eq!(
        record_by_native_id(&records, "after-complete").mcp_tool_call,
        attribution("after-server", "after-tool")
    );
}

#[test]
fn copilot_same_object_rewrite_during_prescan_never_reuses_stale_attribution() {
    assert_same_object_rewrite_omits_stale_attribution(|path, replacement, rewrite_ran| {
        let path = path.to_path_buf();
        super::copilot::set_after_copilot_linkage_record_hook(3, move || {
            fs::write(path, replacement).unwrap();
            rewrite_ran.store(true, Ordering::SeqCst);
        });
    });
}

#[test]
fn copilot_same_object_rewrite_before_projection_never_reuses_stale_attribution() {
    assert_same_object_rewrite_omits_stale_attribution(|path, replacement, rewrite_ran| {
        let path = path.to_path_buf();
        super::copilot::set_after_copilot_linkage_plan_hook(move || {
            fs::write(path, replacement).unwrap();
            rewrite_ran.store(true, Ordering::SeqCst);
        });
    });
}

#[test]
fn copilot_context_rewrite_before_projection_invalidates_the_complete_plan() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let session_id = "context-rewrite-session";
    let raced_start = start("raced-start", "raced-call", "raced-server", "raced-tool");
    let raced_completion = completion("raced-complete", "raced-call", true);
    let late_duplicate = start(
        "raced-duplicate",
        "raced-call",
        "raced-server",
        "raced-tool",
    );
    let empty_noise = json!({"type": "session.info", "padding": ""}).to_string();
    let padding = late_duplicate.len().checked_sub(empty_noise.len()).unwrap();
    let irrelevant = json!({
        "type": "session.info",
        "padding": "x".repeat(padding)
    })
    .to_string();
    assert_eq!(irrelevant.len(), late_duplicate.len());

    let original = vec![raced_start.clone(), raced_completion.clone(), irrelevant];
    let replacement = vec![raced_start, raced_completion, late_duplicate];
    write_session(temp.path(), "session", session_id, &original);
    let path = temp.path().join("session/events.jsonl");
    let replacement_bytes = session_bytes(session_id, &replacement);
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        replacement_bytes.len() as u64
    );

    let rewrite_ran = Arc::new(AtomicBool::new(false));
    let rewrite_ran_in_hook = Arc::clone(&rewrite_ran);
    super::copilot::set_after_copilot_linkage_plan_hook(move || {
        fs::write(path, replacement_bytes).unwrap();
        rewrite_ran_in_hook.store(true, Ordering::SeqCst);
    });
    let error = try_project_copilot_root(temp.path()).unwrap_err();
    assert!(rewrite_ran.load(Ordering::SeqCst));
    assert!(matches!(error, CaptureError::SourceChangedDuringCapture));

    let records = project_copilot_root(temp.path());
    assert_eq!(records.len(), replacement.len());
    assert_eq!(
        record_by_native_id(&records, "raced-complete").mcp_tool_call,
        None
    );
    assert!(records
        .iter()
        .any(|record| native_id(record) == "raced-duplicate"));
}

#[test]
fn copilot_attributes_only_unique_exact_terminal_completions() {
    let fixture = fixture_root();
    let records = project_copilot_root(&fixture.root);
    assert_eq!(records.len(), 53);
    assert!(records.iter().all(|record| {
        record.parser_revision == super::copilot::COPILOT_DIRECT_NATIVE_JSONL_PARSER_REVISION
    }));

    let beta = record_by_native_id(&records, "complete-beta");
    assert_eq!(beta.mcp_tool_call, attribution("beta", "lookup"));
    assert_eq!(
        beta.content.normalized_body.as_deref(),
        Some("synthetic beta failure")
    );
    assert_eq!(
        beta.content.structured_content.as_ref().unwrap()["tool_result"]["outcome"],
        "failure"
    );

    let alpha = record_by_native_id(&records, "complete-alpha");
    assert_eq!(alpha.mcp_tool_call, attribution("alpha", "lookup"));
    assert_eq!(
        alpha.content.normalized_body.as_deref(),
        Some("synthetic alpha success")
    );

    let contentless = record_by_native_id(&records, "complete-contentless");
    assert_eq!(contentless.mcp_tool_call, attribution("gamma", "ping"));
    assert_eq!(
        contentless.content.normalized_body.as_deref(),
        Some("tool_output")
    );
    assert_eq!(
        contentless.content.structured_content.as_ref().unwrap()["tool_result"]["outcome"],
        "success"
    );

    assert_eq!(
        record_by_native_id(&records, "complete-malformed-envelope").mcp_tool_call,
        attribution("malformed", "lookup")
    );
    assert_eq!(
        record_by_native_id(&records, "complete-absent-shape").mcp_tool_call,
        attribution("shape-free", "lookup")
    );
    assert_eq!(
        record_by_native_id(&records, "independent-complete").mcp_tool_call,
        attribution("independent", "lookup")
    );
    assert_eq!(
        record_by_native_id(&records, "complete-duplicate-nested-key").mcp_tool_call,
        attribution("duplicate", "nested")
    );

    let mut attributed = records
        .iter()
        .filter(|record| record.mcp_tool_call.is_some())
        .map(native_id)
        .collect::<Vec<_>>();
    attributed.sort_unstable();
    assert_eq!(
        attributed,
        vec![
            "complete-absent-shape",
            "complete-alpha",
            "complete-beta",
            "complete-contentless",
            "complete-duplicate-nested-key",
            "complete-malformed-envelope",
            "independent-complete",
        ]
    );
    assert!(records
        .iter()
        .filter(|record| native_id(record).contains("start"))
        .all(|record| record.mcp_tool_call.is_none()));
}

#[test]
fn copilot_malformed_ambiguous_or_orphan_linkage_abstains() {
    let fixture = fixture_root();
    let records = project_copilot_root(&fixture.root);
    for native_id in [
        "complete-missing-pair",
        "complete-wrong-pair",
        "complete-empty-pair",
        "complete-composite-only",
        "complete-duplicate",
        "complete-conflict",
        "complete-orphan",
        "complete-double-a",
        "complete-double-b",
        "complete-wrong-id",
        "complete-before-start",
        "complete-wrong-success",
        "complete-missing-success",
        "complete-sequential-a",
        "complete-sequential-b",
        "complete-late-duplicate",
        "complete-duplicate-server-key",
        "complete-escaped-duplicate-tool-key",
        "complete-duplicate-success-key",
    ] {
        assert_eq!(
            record_by_native_id(&records, native_id).mcp_tool_call,
            None,
            "{native_id} must abstain"
        );
    }
}

#[test]
fn copilot_duplicate_envelope_or_call_id_keys_disable_the_session_plan() {
    let ambiguous_lines = [
        r#"{"type":"session.info","type":"tool.execution_start","id":"ambiguous","data":{"toolCallId":"ambiguous","mcpServerName":"x","mcpToolName":"y"}}"#,
        r#"{"type":"session.info","ty\u0070e":"tool.execution_start","id":"ambiguous","data":{"toolCallId":"ambiguous","mcpServerName":"x","mcpToolName":"y"}}"#,
        r#"{"type":"tool.execution_start","id":"ambiguous","data":{"toolCallId":"ambiguous-a"},"data":{"toolCallId":"ambiguous-b","mcpServerName":"x","mcpToolName":"y"}}"#,
        r#"{"type":"tool.execution_start","id":"ambiguous","data":{"toolCallId":"ambiguous-a"},"da\u0074a":{"toolCallId":"ambiguous-b","mcpServerName":"x","mcpToolName":"y"}}"#,
        r#"{"type":"tool.execution_start","id":"ambiguous","data":{"toolCallId":"ambiguous-a","toolCallId":"ambiguous-b","mcpServerName":"x","mcpToolName":"y"}}"#,
        r#"{"type":"tool.execution_start","id":"ambiguous","data":{"toolCallId":"ambiguous-a","toolCall\u0049d":"ambiguous-b","mcpServerName":"x","mcpToolName":"y"}}"#,
    ];
    for ambiguous in ambiguous_lines {
        let events = vec![
            start("before-start", "before", "clean", "before"),
            completion("before-complete", "before", true),
            ambiguous.to_owned(),
            start("after-start", "after", "clean", "after"),
            completion("after-complete", "after", true),
        ];
        assert!(linkage_plan(&events, super::copilot::CopilotLinkageLimits::DEFAULT).is_empty());
        let records = project_events(&events);
        assert_eq!(
            record_by_native_id(&records, "before-complete").mcp_tool_call,
            None
        );
        assert_eq!(
            record_by_native_id(&records, "after-complete").mcp_tool_call,
            None
        );
        assert!(records
            .iter()
            .any(|record| native_id(record) == "ambiguous"));
    }
}

#[test]
fn copilot_completion_duplicate_envelope_or_call_id_keys_disable_the_session_plan() {
    let ambiguous_lines = [
        r#"{"type":"tool.execution_complete","type":"tool.execution_complete","id":"ambiguous-complete","data":{"toolCallId":"ambiguous","success":true}}"#,
        r#"{"type":"tool.execution_complete","ty\u0070e":"tool.execution_complete","id":"ambiguous-complete","data":{"toolCallId":"ambiguous","success":true}}"#,
        r#"{"type":"tool.execution_complete","id":"ambiguous-complete","data":{"toolCallId":"ambiguous","success":true},"data":{"toolCallId":"ambiguous","success":true}}"#,
        r#"{"type":"tool.execution_complete","id":"ambiguous-complete","data":{"toolCallId":"ambiguous","success":true},"da\u0074a":{"toolCallId":"ambiguous","success":true}}"#,
        r#"{"type":"tool.execution_complete","id":"ambiguous-complete","data":{"toolCallId":"ambiguous","toolCallId":"ambiguous","success":true}}"#,
        r#"{"type":"tool.execution_complete","id":"ambiguous-complete","data":{"toolCallId":"ambiguous","toolCall\u0049d":"ambiguous","success":true}}"#,
    ];
    for ambiguous in ambiguous_lines {
        let events = vec![
            start("before-start", "before", "clean", "before"),
            completion("before-complete", "before", true),
            start("ambiguous-start", "ambiguous", "ambiguous", "lookup"),
            ambiguous.to_owned(),
            start("after-start", "after", "clean", "after"),
            completion("after-complete", "after", false),
        ];
        assert!(linkage_plan(&events, super::copilot::CopilotLinkageLimits::DEFAULT).is_empty());
        let records = project_events(&events);
        for completion_id in ["before-complete", "ambiguous-complete", "after-complete"] {
            assert_eq!(
                record_by_native_id(&records, completion_id).mcp_tool_call,
                None,
                "{completion_id} must remain ordinary and unattributed"
            );
        }
    }
}

#[test]
fn copilot_duplicate_identity_and_success_keys_only_retract_the_affected_id() {
    let events = vec![
        start("clean-start", "clean", "clean", "lookup"),
        completion("clean-complete", "clean", true),
        r#"{"type":"tool.execution_start","id":"server-ordinary-start","data":{"toolCallId":"server-ordinary","mcpServerName":"a","mcpServerName":"a","mcpToolName":"lookup"}}"#.to_owned(),
        completion("server-ordinary-complete", "server-ordinary", true),
        r#"{"type":"tool.execution_start","id":"server-escaped-start","data":{"toolCallId":"server-escaped","mcpServerName":"a","mcpSer\u0076erName":"a","mcpToolName":"lookup"}}"#.to_owned(),
        completion("server-escaped-complete", "server-escaped", true),
        r#"{"type":"tool.execution_start","id":"tool-ordinary-start","data":{"toolCallId":"tool-ordinary","mcpServerName":"a","mcpToolName":"lookup","mcpToolName":"lookup"}}"#.to_owned(),
        completion("tool-ordinary-complete", "tool-ordinary", true),
        r#"{"type":"tool.execution_start","id":"tool-escaped-start","data":{"toolCallId":"tool-escaped","mcpServerName":"a","mcpToolName":"lookup","mcpTool\u004eame":"lookup"}}"#.to_owned(),
        completion("tool-escaped-complete", "tool-escaped", true),
        start("success-ordinary-start", "success-ordinary", "a", "lookup"),
        r#"{"type":"tool.execution_complete","id":"success-ordinary-complete","data":{"toolCallId":"success-ordinary","success":true,"success":true}}"#.to_owned(),
        start("success-escaped-start", "success-escaped", "a", "lookup"),
        r#"{"type":"tool.execution_complete","id":"success-escaped-complete","data":{"toolCallId":"success-escaped","success":true,"succ\u0065ss":true}}"#.to_owned(),
    ];
    let records = project_events(&events);
    assert_eq!(
        record_by_native_id(&records, "clean-complete").mcp_tool_call,
        attribution("clean", "lookup")
    );
    for native_id in [
        "server-ordinary-complete",
        "server-escaped-complete",
        "tool-ordinary-complete",
        "tool-escaped-complete",
        "success-ordinary-complete",
        "success-escaped-complete",
    ] {
        assert_eq!(
            record_by_native_id(&records, native_id).mcp_tool_call,
            None,
            "{native_id} must abstain"
        );
    }
}

#[test]
fn copilot_only_counts_relevant_events_toward_session_bounds() {
    let mut events = (0..100)
        .map(|index| {
            json!({
                "type": "session.info",
                "id": format!("noise-{index}"),
                "data": {
                    "toolCallId": format!("noise-call-{index}"),
                    "mcpServerName": "noise",
                    "mcpToolName": "noise"
                }
            })
            .to_string()
        })
        .collect::<Vec<_>>();
    events.push(start("bounded-start", "id", "s", "t"));
    events.push(completion("bounded-complete", "id", true));
    let limits = super::copilot::CopilotLinkageLimits {
        max_distinct_ids: 1,
        max_total_candidates: 2,
        max_candidates_per_id: 2,
        max_retained_bytes: 6,
        ..super::copilot::CopilotLinkageLimits::DEFAULT
    };
    assert_eq!(linkage_plan(&events, limits).len(), 1);
}

#[test]
fn copilot_generated_id_count_multiplicity_and_byte_exhaustion_disable_the_plan() {
    let baseline = vec![
        start("baseline-start", "base", "s", "t"),
        completion("baseline-complete", "base", true),
    ];
    assert_eq!(
        linkage_plan(&baseline, super::copilot::CopilotLinkageLimits::DEFAULT).len(),
        1
    );

    let mut overlong_id = baseline.clone();
    overlong_id.push(start("long-id", "12345", "s", "t"));
    let limits = super::copilot::CopilotLinkageLimits {
        max_call_id_bytes: 4,
        ..super::copilot::CopilotLinkageLimits::DEFAULT
    };
    assert!(linkage_plan(&overlong_id, limits).is_empty());

    let mut too_many_ids = baseline.clone();
    too_many_ids.push(start("second-id", "second", "s", "t"));
    let limits = super::copilot::CopilotLinkageLimits {
        max_distinct_ids: 1,
        ..super::copilot::CopilotLinkageLimits::DEFAULT
    };
    assert!(linkage_plan(&too_many_ids, limits).is_empty());

    let mut too_many_candidates = baseline.clone();
    too_many_candidates.push(
        r#"{"type":"tool.execution_start","id":"missing-id","data":{"mcpServerName":"s","mcpToolName":"t"}}"#.to_owned(),
    );
    let limits = super::copilot::CopilotLinkageLimits {
        max_total_candidates: 2,
        ..super::copilot::CopilotLinkageLimits::DEFAULT
    };
    assert!(linkage_plan(&too_many_candidates, limits).is_empty());

    let mut too_many_for_id = baseline.clone();
    too_many_for_id.push(completion("late-complete", "base", false));
    let limits = super::copilot::CopilotLinkageLimits {
        max_candidates_per_id: 2,
        ..super::copilot::CopilotLinkageLimits::DEFAULT
    };
    assert!(linkage_plan(&too_many_for_id, limits).is_empty());

    let limits = super::copilot::CopilotLinkageLimits {
        max_retained_bytes: 9,
        ..super::copilot::CopilotLinkageLimits::DEFAULT
    };
    assert!(linkage_plan(&baseline, limits).is_empty());

    let mut oversized_component = baseline;
    oversized_component.push(start(
        "oversized-component",
        "large",
        &"x".repeat(MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES + 1),
        "t",
    ));
    assert!(linkage_plan(
        &oversized_component,
        super::copilot::CopilotLinkageLimits::DEFAULT
    )
    .is_empty());
}

#[test]
fn copilot_oversized_or_malformed_linkage_never_aborts_neighbor_projection() {
    let oversized = format!(
        r#"{{"type":"tool.execution_start","id":"oversized-start","data":{{"toolCallId":"oversized","mcpServerName":"oversized","mcpToolName":"lookup"}},"padding":"{}"}}"#,
        "x".repeat(super::copilot::COPILOT_LINKAGE_MAX_LINE_BYTES + 1)
    );
    let events = vec![
        start("before-start", "before", "clean", "before"),
        completion("before-complete", "before", true),
        oversized,
        start("after-start", "after", "clean", "after"),
        completion("after-complete", "after", false),
    ];
    let records = project_events_through_shared_scanner(&events);
    for expected in [
        "before-start",
        "before-complete",
        "oversized-start",
        "after-start",
        "after-complete",
    ] {
        assert!(records.iter().any(|record| native_id(record) == expected));
    }
    assert!(records.iter().all(|record| record.mcp_tool_call.is_none()));

    let malformed_events = vec![
        start("malformed-before-start", "before", "clean", "before"),
        completion("malformed-before-complete", "before", true),
        r#"{"type":"tool.execution_start","id":"broken","data":{"toolCallId":"broken""#.to_owned(),
        start("malformed-after-start", "after", "clean", "after"),
        completion("malformed-after-complete", "after", true),
    ];
    let records = project_events_through_shared_scanner(&malformed_events);
    for expected in [
        "malformed-before-start",
        "malformed-before-complete",
        "malformed-after-start",
        "malformed-after-complete",
    ] {
        assert!(records.iter().any(|record| native_id(record) == expected));
    }
    assert!(records.iter().all(|record| record.mcp_tool_call.is_none()));
}

#[test]
fn copilot_same_call_id_in_separate_sessions_remains_independent() {
    let fixture = fixture_root();
    let records = project_copilot_root(&fixture.root);
    assert_eq!(
        record_by_native_id(&records, "complete-alpha").mcp_tool_call,
        attribution("alpha", "lookup")
    );
    assert_eq!(
        record_by_native_id(&records, "independent-complete").mcp_tool_call,
        attribution("independent", "lookup")
    );
}

#[test]
fn copilot_late_duplicate_retracts_the_previously_attributed_completion() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let mut events = vec![
        start("late-start-a", "late", "late", "lookup"),
        completion("late-complete", "late", true),
    ];
    write_session(temp.path(), "session", "late-session", &events);
    let before = project_copilot_root(temp.path());
    let before_completion = record_by_native_id(&before, "late-complete");
    assert_eq!(
        before_completion.mcp_tool_call,
        attribution("late", "lookup")
    );
    let stable_id = before_completion.event_id;

    events.push(start("late-start-b", "late", "late", "lookup"));
    write_session(temp.path(), "session", "late-session", &events);
    let after = project_copilot_root(temp.path());
    let after_completion = record_by_native_id(&after, "late-complete");
    assert_eq!(after_completion.mcp_tool_call, None);
    assert_eq!(after_completion.event_id, stable_id);
}

#[test]
fn copilot_attribution_does_not_change_stable_event_ids() {
    let materialized = fixture_root();
    let fixture_root = materialized.root.join("session-mcp");
    let fixture_file = fixture_root.join("events.jsonl");
    let attributed = project_copilot_root(&fixture_root);

    let temp = crate::test_support_paths::tempdir().unwrap();
    let session = temp.path().join("session-mcp");
    fs::create_dir_all(&session).unwrap();
    let without_pairs = fs::read_to_string(fixture_file)
        .unwrap()
        .lines()
        .map(|line| {
            let mut value: Value = serde_json::from_str(line).unwrap();
            if value.get("type").and_then(Value::as_str) == Some("tool.execution_start") {
                if let Some(data) = value.get_mut("data").and_then(Value::as_object_mut) {
                    data.remove("mcpServerName");
                    data.remove("mcpToolName");
                }
            }
            serde_json::to_string(&value).unwrap()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(session.join("events.jsonl"), without_pairs).unwrap();
    let unattributed = project_copilot_root(temp.path());

    let ids = |records: Vec<CoreRecord>| {
        records
            .into_iter()
            .map(|record| (native_id(&record).to_owned(), record.event_id.to_string()))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(ids(attributed), ids(unattributed));
}

#[test]
fn only_copilot_bumps_the_shared_direct_jsonl_parser_revision() {
    let copilot = super::super::copilot_source_backed_adapter();
    assert_eq!(
        JsonlFamilyAdapter::parser_revision(&copilot),
        super::copilot::COPILOT_DIRECT_NATIVE_JSONL_PARSER_REVISION
    );
    assert_eq!(
        JsonlFamilyAdapter::append_mode(&copilot),
        JsonlFamilyAppendMode::Replacement
    );
    assert_eq!(
        JsonlFamilyAdapter::oversized_record_policy(&copilot),
        JsonlOversizedRecordPolicy::RejectRecord
    );

    for adapter in [
        super::super::antigravity_source_backed_adapter(),
        super::super::factory_droid_source_backed_adapter(),
        super::super::qoder_source_backed_adapter(),
        super::super::qwen_code_source_backed_adapter(),
        super::super::tabnine_source_backed_adapter(),
        super::super::windsurf_source_backed_adapter(),
    ] {
        assert_eq!(
            JsonlFamilyAdapter::parser_revision(&adapter),
            "direct-native-jsonl-parser-v4"
        );
        assert_eq!(
            JsonlFamilyAdapter::append_mode(&adapter),
            JsonlFamilyAppendMode::CertifiedSuffix
        );
        assert_eq!(
            JsonlFamilyAdapter::oversized_record_policy(&adapter),
            JsonlOversizedRecordPolicy::RejectSource
        );
    }
}
