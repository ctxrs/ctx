use ctx_history_core::{RepositoryAbstentionReason, RepositoryFileObservationKind};
use serde_json::json;

use super::repository_tool_evidence;
use crate::provider::codex::events::CodexToolCallContext;
use crate::repository_attribution::{attribute, AttributionInput, CommandEvidenceDisposition};
use crate::{OutputOutcome, OutputOutcomeMetadata};

#[test]
fn accepts_only_one_top_level_native_argument_decode_and_redacts_it() {
    let payload = json!({
        "type": "function_call",
        "name": "exec_command",
        "call_id": "call-1",
        "arguments": json!({
            "cmd": "git status",
            "workdir": "/repo",
            "yield_time_ms": 10000,
            "decoy": {"cmd": "git commit -m decoy", "workdir": "/other"}
        }).to_string()
    });
    let evidence = repository_tool_evidence(&payload).remove(0);
    assert_eq!(evidence.command.as_deref(), Some("git status"));
    assert_eq!(evidence.declared_workdir.as_deref(), Some("/repo"));
    let encoded = serde_json::to_string(&evidence.structured_content).unwrap();
    assert!(!encoded.contains("git status"));
    assert!(!encoded.contains("decoy"));
    assert_eq!(
        evidence.structured_content["provider_native_tool"]["raw_arguments_retained"],
        false
    );
}

#[test]
fn oversized_native_command_retains_typed_abstention_and_blocks_cwd_fallback() {
    let temp = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("/usr/bin/git")
        .args(["init", "-q"])
        .current_dir(temp.path())
        .status()
        .unwrap()
        .success());
    let oversized = "x".repeat(super::MAX_COMMAND_BYTES + 1);
    let payload = json!({
        "type": "function_call",
        "name": "exec_command",
        "call_id": "oversized-command",
        "arguments": json!({"cmd": oversized}).to_string(),
    });
    let evidence = repository_tool_evidence(&payload).remove(0);
    assert!(evidence.command.is_none());
    assert!(evidence.command_too_large);

    let annotation = attribute(AttributionInput {
        session_cwd: Some(temp.path().to_string_lossy().into_owned()),
        command: evidence.command,
        command_disposition: CommandEvidenceDisposition::CommandTooLarge,
        ..AttributionInput::default()
    });
    assert!(annotation.repository_bindings.is_empty());
    assert!(annotation
        .repository_abstentions
        .iter()
        .any(|abstention| { abstention.reason == RepositoryAbstentionReason::CommandTooLarge }));
}

#[test]
fn javascript_wrappers_nested_only_commands_and_missing_call_ids_abstain() {
    for payload in [
        json!({
            "type": "custom_tool_call",
            "name": "exec_command",
            "call_id": "call-1",
            "arguments": "tools.exec_command({cmd:'git status',workdir:'/repo'})"
        }),
        json!({
            "type": "function_call",
            "name": "exec_command",
            "call_id": "call-2",
            "arguments": {"dead_branch": {"cmd": "git status", "workdir": "/repo"}}
        }),
        json!({
            "type": "function_call",
            "name": "exec_command",
            "arguments": {"cmd": "git status", "workdir": "/repo"}
        }),
        json!({
            "type": "function_call",
            "name": "exec_command",
            "call_id": "call-3",
            "arguments": {"cmd": "git status"},
            "input": {"cmd": "git commit -m decoy"}
        }),
        json!({
            "type": "function_call",
            "name": "exec_command",
            "tool": "wait",
            "call_id": "call-4",
            "arguments": {"cmd": "git status"}
        }),
    ] {
        assert!(repository_tool_evidence(&payload).is_empty());
    }
}

#[test]
fn exact_top_level_literal_calls_decode_commands_and_patch_headers() {
    let payload = json!({
        "type": "custom_tool_call",
        "name": "exec",
        "call_id": "outer-1",
        "input": r#"
            const first = await tools.exec_command({cmd:"git status",workdir:"/repo/one",yield_time_ms:10000});
            const second = await tools.exec_command({cmd:"git log -1",workdir:"/repo/two"});
            const patched = await tools.apply_patch("*** Begin Patch\n*** Add File: /repo/one/src/new.rs\n*** Update File: /repo/one/src/lib.rs\n*** Delete File: /repo/one/src/old.rs\n*** End Patch");
            text(first.output);
        "#
    });
    let evidence = repository_tool_evidence(&payload);
    assert_eq!(evidence.len(), 3);
    assert_eq!(evidence[0].tool_name, "exec_command");
    assert_eq!(evidence[0].declared_workdir.as_deref(), Some("/repo/one"));
    assert_eq!(evidence[0].command.as_deref(), Some("git status"));
    assert_eq!(evidence[1].declared_workdir.as_deref(), Some("/repo/two"));
    assert_eq!(evidence[1].command.as_deref(), Some("git log -1"));
    assert_eq!(evidence[2].tool_name, "apply_patch");
    assert_eq!(evidence[2].file_observations.len(), 3);
    assert_eq!(
        evidence[2].file_observations[0].path,
        "/repo/one/src/new.rs"
    );
    assert_eq!(
        evidence[2].file_observations[0].kind,
        RepositoryFileObservationKind::Created
    );
    assert_eq!(
        evidence[2].file_observations[1].path,
        "/repo/one/src/lib.rs"
    );
    assert_eq!(
        evidence[2].file_observations[1].kind,
        RepositoryFileObservationKind::Modified
    );
    assert_eq!(
        evidence[2].file_observations[2].path,
        "/repo/one/src/old.rs"
    );
    assert_eq!(
        evidence[2].file_observations[2].kind,
        RepositoryFileObservationKind::Deleted
    );
}

#[test]
fn genuine_terminal_template_preserves_declared_workdir_for_call_and_linked_result() {
    let temp = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("/usr/bin/git")
        .args(["init", "-q"])
        .current_dir(temp.path())
        .status()
        .unwrap()
        .success());
    let workdir = temp.path().to_string_lossy();
    let payload = json!({
        "type": "custom_tool_call",
        "name": "exec",
        "call_id": "outer-template",
        "input": format!(
            "const r = await tools.exec_command({{\"cmd\":\"git diff --check && git diff && cargo test\",\"workdir\":{workdir:?},\"yield_time_ms\":30000}});\ntext(r.output);\ntext(`exit=${{r.exit_code}}`);\n"
        ),
    });
    let mut evidence = repository_tool_evidence(&payload);
    assert_eq!(evidence.len(), 1);
    let evidence = evidence.remove(0);
    assert_eq!(evidence.tool_name, "exec_command");
    assert_eq!(evidence.declared_workdir.as_deref(), Some(workdir.as_ref()));
    assert_eq!(
        evidence.command.as_deref(),
        Some("git diff --check && git diff && cargo test")
    );

    let call_annotation = attribute(AttributionInput {
        declared_tool_workdir: evidence.declared_workdir.clone(),
        command: evidence.command.clone(),
        ..AttributionInput::default()
    });
    assert_eq!(call_annotation.repository_bindings.len(), 1);

    let context = CodexToolCallContext {
        tool_name: evidence.tool_name,
        exact_command: evidence.command,
        declared_workdir: evidence.declared_workdir,
        origin_call_id: Some("outer-template".to_owned()),
        origin_event_sequence: Some(39),
        ..CodexToolCallContext::default()
    };
    let result = super::repository_result_evidence(
        &json!({"output": "Script completed\nWall time 0.1 seconds\nOutput:\n"}),
        &context,
        "outer-template",
        [7; 32],
        10,
        &OutputOutcomeMetadata {
            outcome: OutputOutcome::Success,
            exit_code: Some(0),
            duration_ms: Some(100),
        },
    )
    .unwrap();
    assert_eq!(result.declared_workdir.as_deref(), Some(workdir.as_ref()));
    let result_annotation = attribute(AttributionInput {
        declared_tool_workdir: result.declared_workdir,
        command: result.command,
        outcome_operation_repository_path: result.outcome_operation_repository_path,
        outcome_output_repository_path: result.outcome_output_repository_path,
        outcome_observations: result.outcomes,
        outcome_abstentions: result.abstentions,
        ..AttributionInput::default()
    });
    assert_eq!(result_annotation.repository_bindings.len(), 1);
    assert_eq!(
        result_annotation.repository_bindings[0].binding_id,
        call_annotation.repository_bindings[0].binding_id
    );
}

#[test]
fn genuine_bound_patch_wrapper_emits_exact_file_observations() {
    let payload = json!({
        "type": "custom_tool_call",
        "name": "exec",
        "call_id": "outer-patch",
        "input": r#"
            const patch = "*** Begin Patch\n*** Add File: /repo/src/new.rs\n*** Update File: /repo/src/lib.rs\n*** Move to: /repo/src/moved.rs\n*** Delete File: /repo/src/old.rs\n*** End Patch";
            text(await tools.apply_patch(patch));
        "#,
    });
    let evidence = repository_tool_evidence(&payload);
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].tool_name, "apply_patch");
    assert_eq!(evidence[0].file_observations.len(), 3);
    assert_eq!(evidence[0].file_observations[0].path, "/repo/src/new.rs");
    assert_eq!(
        evidence[0].file_observations[0].kind,
        RepositoryFileObservationKind::Created
    );
    assert_eq!(evidence[0].file_observations[1].path, "/repo/src/moved.rs");
    assert_eq!(
        evidence[0].file_observations[1].prior_path.as_deref(),
        Some("/repo/src/lib.rs")
    );
    assert_eq!(
        evidence[0].file_observations[1].kind,
        RepositoryFileObservationKind::Renamed
    );
    assert_eq!(evidence[0].file_observations[2].path, "/repo/src/old.rs");
    assert_eq!(
        evidence[0].file_observations[2].kind,
        RepositoryFileObservationKind::Deleted
    );
    assert_eq!(
        evidence[0].structured_content["provider_native_tool"]["argument_schema"],
        "codex_nested_apply_patch_literal_v3"
    );
}

#[test]
fn direct_native_patch_input_uses_the_same_bounded_parser() {
    let payload = json!({
        "type": "custom_tool_call",
        "name": "apply_patch",
        "call_id": "direct-patch",
        "input": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch\n",
    });
    let evidence = repository_tool_evidence(&payload);
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].tool_name, "apply_patch");
    assert_eq!(evidence[0].file_observations.len(), 1);
    assert_eq!(evidence[0].file_observations[0].path, "src/lib.rs");
    assert_eq!(
        evidence[0].file_observations[0].kind,
        RepositoryFileObservationKind::Modified
    );
}

#[test]
fn inert_or_dynamic_javascript_never_emits_executed_tool_evidence() {
    for source in [
        r#"const example = "tools.exec_command({cmd:\"git commit -m inert\",workdir:\"/repo\"})"; text(example);"#,
        r#"// tools.exec_command({cmd:"git commit -m comment",workdir:"/repo"})
            text("*** Add File: /repo/inert.rs");"#,
        r#"if (false) { await tools.exec_command({cmd:"git commit -m dead",workdir:"/repo"}); }"#,
        r#"const args = {cmd:"git commit -m dynamic",workdir:"/repo"};
            const result = await tools.exec_command(args);"#,
        r#"const result = await tools.exec_command({cmd:"git status",workdir:"/repo"});
            observeDynamically(result);"#,
        r#"text(prior.output);
            const result = await tools.exec_command({cmd:"git status",workdir:"/repo"});"#,
        r#"const patch = "*** Begin Patch\n*** Add File: /repo/inert.rs\n*** End Patch"; text(patch);"#,
        r#"const patch = choosePatch(); text(await tools.apply_patch(patch));"#,
        r#"const patch = "*** Begin Patch\n*** Add File: /repo/dynamic.rs\n*** End Patch";
            text(await tools.apply_patch(transform(patch)));"#,
        r#"const result = await tools.exec_command({cmd:"git status",workdir:"/repo"});
            text(`${sideEffect()}`);"#,
    ] {
        let payload = json!({
            "type": "custom_tool_call",
            "name": "exec",
            "call_id": "outer-inert",
            "input": source,
        });
        assert!(repository_tool_evidence(&payload).is_empty(), "{source}");
    }
}

#[test]
fn continuation_controls_require_exact_bounded_identifiers() {
    assert_eq!(
        super::running_continuation_cell_id(&json!({
            "output": "Script running with cell ID cell-7\n"
        }))
        .as_deref(),
        Some("cell-7")
    );
    assert!(super::running_continuation_cell_id(&json!({
        "output": "prose says Script running with cell ID cell-7"
    }))
    .is_none());
    assert!(super::terminal_continuation_result(&json!({
        "output": "Script completed\nFinal output:\nok"
    })));
    assert!(!super::terminal_continuation_result(&json!({
        "output": "Script completed",
        "result": "Process exited with code 0"
    })));
}
