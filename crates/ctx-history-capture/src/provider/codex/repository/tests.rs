use ctx_history_core::{
    RepositoryAbstentionReason, RepositoryFileInvocationKind, RepositoryFileObservationKind,
};
use serde_json::json;

use super::{repository_tool_evidence, repository_tool_evidence_for_core};
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
fn strict_patch_invocations_preserve_multiple_exact_operations_and_targets() {
    let patch = "*** Begin Patch\n*** Add File: src/new.rs\n+new\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** Delete File: src/old.rs\n*** End Patch";
    let normalized_body = format!("apply_patch: {patch}");
    let payload = json!({
        "type": "custom_tool_call",
        "name": "apply_patch",
        "call_id": "strict-multi",
        "input": patch,
    });

    let evidence = repository_tool_evidence_for_core(&payload, Some(&normalized_body));
    assert_eq!(evidence.len(), 1);
    let invocations = &evidence[0].file_invocations;
    assert_eq!(invocations.len(), 3);
    assert_eq!(
        invocations
            .iter()
            .map(|invocation| (
                invocation.path.as_str(),
                invocation.kind,
                invocation.operation_ordinal,
                invocation.tool_name.as_deref(),
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "src/new.rs",
                RepositoryFileInvocationKind::Create,
                0,
                Some("apply_patch"),
            ),
            (
                "src/lib.rs",
                RepositoryFileInvocationKind::Modify,
                1,
                Some("apply_patch"),
            ),
            (
                "src/old.rs",
                RepositoryFileInvocationKind::Delete,
                2,
                Some("apply_patch"),
            ),
        ]
    );
    assert_eq!(
        invocations
            .iter()
            .map(|invocation| {
                normalized_body
                    .get({
                        let range = invocation.normalized_text_range.unwrap();
                        range.start as usize..range.end as usize
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>(),
        vec![
            "*** Add File: src/new.rs",
            "*** Update File: src/lib.rs",
            "*** Delete File: src/old.rs",
        ]
    );
}

#[test]
fn strict_patch_rename_keeps_both_paths_in_one_complete_body_range() {
    let patch = "*** Begin Patch\n*** Update File: src/old.rs\n*** Move to: src/new.rs\n@@\n-old\n+new\n*** End Patch";
    let normalized_body = format!("apply_patch: {patch}");
    let payload = json!({
        "type": "custom_tool_call",
        "name": "apply_patch",
        "call_id": "strict-rename",
        "input": patch,
    });

    let evidence = repository_tool_evidence_for_core(&payload, Some(&normalized_body));
    let invocation = &evidence[0].file_invocations[0];
    assert_eq!(invocation.path, "src/new.rs");
    assert_eq!(invocation.prior_path.as_deref(), Some("src/old.rs"));
    assert_eq!(invocation.kind, RepositoryFileInvocationKind::Rename);
    let range = invocation.normalized_text_range.unwrap();
    let range = range.start as usize..range.end as usize;
    assert!(normalized_body.is_char_boundary(range.start));
    assert!(normalized_body.is_char_boundary(range.end));
    assert_eq!(
        &normalized_body[range],
        "*** Update File: src/old.rs\n*** Move to: src/new.rs"
    );
}

#[test]
fn strict_ranges_are_utf8_byte_exact_and_exclude_crlf_edges() {
    let patch =
        "*** Begin Patch\r\n*** Update File: src/é.rs\r\n@@\r\n-old\r\n+new\r\n*** End Patch\r\n";
    let normalized_body = format!("apply_patch: {}", patch.trim());
    let payload = json!({
        "type": "custom_tool_call",
        "name": "apply_patch",
        "call_id": "strict-utf8-range",
        "input": patch,
    });

    let evidence = repository_tool_evidence_for_core(&payload, Some(&normalized_body));
    let range = evidence[0].file_invocations[0]
        .normalized_text_range
        .unwrap();
    let range = range.start as usize..range.end as usize;
    assert!(normalized_body.is_char_boundary(range.start));
    assert!(normalized_body.is_char_boundary(range.end));
    assert_eq!(&normalized_body[range], "*** Update File: src/é.rs");
}

#[test]
fn nested_multi_operation_patches_have_distinct_deterministic_ordinals_without_ranges() {
    let payload = json!({
        "type": "custom_tool_call",
        "name": "exec",
        "call_id": "strict-nested",
        "input": r#"
            text(await tools.apply_patch("*** Begin Patch\n*** Add File: src/first.rs\n+first\n*** Update File: src/second.rs\n@@\n-old\n+new\n*** End Patch"));
            const command = await tools.exec_command({cmd:"git status",workdir:"/repo"});
            text(await tools.apply_patch("*** Begin Patch\n*** Delete File: src/third.rs\n*** Add File: src/fourth.rs\n+fourth\n*** End Patch"));
            text(command.output);
        "#,
    });

    let evidence = repository_tool_evidence(&payload);
    assert_eq!(evidence.len(), 3);
    assert_eq!(
        evidence[0]
            .file_invocations
            .iter()
            .map(|invocation| invocation.operation_ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        evidence[2]
            .file_invocations
            .iter()
            .map(|invocation| invocation.operation_ordinal)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(evidence
        .iter()
        .flat_map(|item| &item.file_invocations)
        .all(|invocation| invocation.normalized_text_range.is_none()));
}

#[test]
fn nested_patch_calls_share_one_strict_bound_and_never_publish_a_partial_prefix() {
    let patch = |prefix: &str, count: usize| {
        let mut patch = String::from("*** Begin Patch\n");
        for index in 0..count {
            patch.push_str(&format!("*** Add File: src/{prefix}-{index}.rs\n+value\n"));
        }
        patch.push_str("*** End Patch");
        serde_json::to_string(&patch).unwrap()
    };
    let payload = json!({
        "type": "custom_tool_call",
        "name": "exec",
        "call_id": "strict-nested-overflow",
        "input": format!(
            "text(await tools.apply_patch({})); text(await tools.apply_patch({}));",
            patch("first", 17),
            patch("second", 16),
        ),
    });

    let evidence = repository_tool_evidence(&payload);
    assert_eq!(evidence.len(), 2);
    assert_eq!(
        evidence
            .iter()
            .map(|item| item.file_observations.len())
            .sum::<usize>(),
        33
    );
    assert!(evidence.iter().all(|item| item.file_invocations.is_empty()));
    assert!(evidence.iter().any(|item| {
        item.abstentions
            .iter()
            .any(|(reason, _)| *reason == RepositoryAbstentionReason::CandidateLimitExceeded)
    }));
}

#[test]
fn strict_invocations_abstain_on_ambiguous_or_unknown_native_shapes() {
    let conflicting_patch = "*** Begin Patch\n*** Add File: src/same.rs\n+new\n*** Delete File: src/same.rs\n*** End Patch";
    let delayed_move = "*** Begin Patch\n*** Update File: src/old.rs\n@@\n-old\n+new\n*** Move to: src/new.rs\n*** End Patch";
    for payload in [
        json!({
            "type": "custom_tool_call",
            "name": "apply_patch",
            "call_id": "ambiguous-patch",
            "input": conflicting_patch,
        }),
        json!({
            "type": "custom_tool_call",
            "name": "apply_patch",
            "call_id": "ambiguous-delayed-move",
            "input": delayed_move,
        }),
        json!({
            "type": "function_call",
            "name": "provider_file_tool",
            "call_id": "generic-recursive-touch",
            "arguments": json!({
                "operation": "modify",
                "nested": {"file_path": "src/generic.rs"},
            }).to_string(),
        }),
    ] {
        assert!(repository_tool_evidence_for_core(&payload, None).is_empty());
    }
    for verb in ["unknown", "Unknown", "edit", "created", ""] {
        assert!(super::exact_file_operation(verb).is_none(), "{verb}");
    }
}

#[test]
fn exact_file_verbs_map_without_alias_or_case_inference() {
    for (verb, operation) in [
        ("read", RepositoryFileInvocationKind::Read),
        ("create", RepositoryFileInvocationKind::Create),
        ("modify", RepositoryFileInvocationKind::Modify),
        ("delete", RepositoryFileInvocationKind::Delete),
        ("rename", RepositoryFileInvocationKind::Rename),
        ("write", RepositoryFileInvocationKind::Write),
    ] {
        assert_eq!(super::exact_file_operation(verb), Some(operation));
    }
}

#[test]
fn exact_native_file_schema_maps_every_verb_and_complete_argument_range() {
    for (tool_name, kind) in [
        ("read", RepositoryFileInvocationKind::Read),
        ("create", RepositoryFileInvocationKind::Create),
        ("modify", RepositoryFileInvocationKind::Modify),
        ("delete", RepositoryFileInvocationKind::Delete),
        ("write", RepositoryFileInvocationKind::Write),
    ] {
        let arguments = json!({"path": format!("src/{tool_name}.rs")});
        let argument_unit = serde_json::to_string(&arguments).unwrap();
        let normalized_body = format!("{tool_name}: {argument_unit}");
        let payload = json!({
            "type": "function_call",
            "name": tool_name,
            "call_id": format!("exact-{tool_name}"),
            "arguments": argument_unit,
        });
        let evidence = repository_tool_evidence_for_core(&payload, Some(&normalized_body));
        let invocation = &evidence[0].file_invocations[0];
        assert_eq!(invocation.kind, kind);
        assert_eq!(invocation.tool_name.as_deref(), Some(tool_name));
        assert_eq!(invocation.operation_ordinal, 0);
        let range = invocation.normalized_text_range.unwrap();
        assert_eq!(
            &normalized_body[range.start as usize..range.end as usize],
            serde_json::to_string(&arguments).unwrap()
        );
    }

    let arguments = json!({"prior_path": "src/old.rs", "path": "src/new.rs"});
    let argument_unit = serde_json::to_string(&arguments).unwrap();
    let normalized_body = format!("rename: {argument_unit}");
    let payload = json!({
        "type": "function_call",
        "name": "rename",
        "call_id": "exact-rename",
        "arguments": argument_unit,
    });
    let evidence = repository_tool_evidence_for_core(&payload, Some(&normalized_body));
    let invocation = &evidence[0].file_invocations[0];
    assert_eq!(invocation.kind, RepositoryFileInvocationKind::Rename);
    assert_eq!(invocation.path, "src/new.rs");
    assert_eq!(invocation.prior_path.as_deref(), Some("src/old.rs"));
}

#[test]
fn exact_native_file_schema_rejects_alias_and_verb_ambiguity() {
    for payload in [
        json!({
            "type": "function_call",
            "name": "Read",
            "call_id": "wrong-case",
            "arguments": json!({"path": "src/lib.rs"}).to_string(),
        }),
        json!({
            "type": "function_call",
            "name": "read",
            "call_id": "ambiguous-path",
            "arguments": json!({
                "path": "src/lib.rs",
                "file_path": "src/decoy.rs",
            }).to_string(),
        }),
        json!({
            "type": "function_call",
            "name": "modify",
            "call_id": "conflicting-operation",
            "arguments": json!({
                "operation": "read",
                "path": "src/lib.rs",
            }).to_string(),
        }),
    ] {
        assert!(repository_tool_evidence_for_core(&payload, None).is_empty());
    }
}

#[test]
fn strict_patch_overflow_abstains_instead_of_publishing_a_prefix() {
    let mut patch = String::from("*** Begin Patch\n");
    for index in 0..=crate::repository_attribution::MAX_REPOSITORY_CANDIDATES {
        patch.push_str(&format!("*** Add File: src/generated/{index}.rs\n+value\n"));
    }
    patch.push_str("*** End Patch");
    let payload = json!({
        "type": "custom_tool_call",
        "name": "apply_patch",
        "call_id": "strict-overflow",
        "input": patch,
    });

    let evidence = repository_tool_evidence_for_core(&payload, None);
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].file_observations.len(), 33);
    assert!(evidence[0].file_invocations.is_empty());
    assert_eq!(
        evidence[0].abstentions,
        vec![(
            RepositoryAbstentionReason::CandidateLimitExceeded,
            "codex_patch_operation_candidate_limit_exceeded",
        )]
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
