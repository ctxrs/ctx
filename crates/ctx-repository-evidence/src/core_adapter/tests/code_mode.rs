//! Synthetic wrappers and real disposable Git receipts; no captured transcripts.
use super::*;

fn invocation(input: String) -> ActivityInvocation {
    let mut invocation = codex_exec_invocation(input);
    invocation.tool = "exec".to_owned();
    invocation
}

#[test]
fn code_mode_exec_links_a_real_commit_receipt() {
    let (_temp, repository, short) = initialize_repository();
    let workdir = serde_json::to_string(&repository).unwrap();
    let output = format!("[main {short}] dogfood fixture\n");
    for (input, body) in [
        (
            format!(
                "const r = await tools.exec_command({{cmd: 'git commit -m dogfood-fixture', workdir: {workdir}, yield_time_ms: 1000}}); text(r.output);"
            ),
            output.clone(),
        ),
        (
            format!(
                "text(await tools.exec_command({{cmd: 'git commit -m dogfood-fixture', workdir: {workdir}}}));"
            ),
            serde_json::json!({"output":output,"exit_code":0,"wall_time_seconds":0.1}).to_string(),
        ),
    ] {
        let call = codex_record(1, "code-mode", Some(invocation(input)), None, vec![]);
        let result = codex_record(
            2,
            "code-mode",
            None,
            Some(ActivityResult {
                status: None,
                completed_at_unix_ms: Some(1_787_126_404_000),
                duration_ns: None,
                text: ActivityTextCapture::Absent,
                structured_content: ActivityJsonCapture::Present {
                    value: serde_json::json!([
                        {"type":"input_text", "text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},
                        {"type":"input_text", "text":body}
                    ]),
                },
            }),
            vec![],
        );
        for records in [[&call, &result], [&result, &call]] {
            let mut adapter = CoreRepositoryEvidenceAdapter::default();
            adapter.begin_source();
            let _ = adapter.evaluate_record(records[0], &"a".repeat(64));
            let evaluated = adapter.evaluate_record(records[1], &"b".repeat(64));
            assert!(
                evaluated
                    .repository_vcs_observations
                    .iter()
                    .any(|observation| matches!(
                        observation.kind,
                        RepositoryVcsObservationKind::Outcome(_)
                    )),
                "code-mode receipt must attribute in either arrival order"
            );
        }
    }
}

#[test]
fn code_mode_recognizes_only_complete_literal_wrappers() {
    for input in [
        "const result = await tools.exec_command({cmd: 'git status', workdir: '/repo', max_output_tokens: 1000}); text(result.output);",
        "// @exec: {\"yield_time_ms\": 1000}\ntext(await tools.exec_command({\"cmd\": \"git status\", \"workdir\": \"/repo\"}));",
        "text((await tools.exec_command({cmd: `git status`, workdir: `/repo`, tty: false})).output);",
    ] {
        let call = codex_record(
            1,
            "literal",
            Some(invocation(input.to_owned())),
            None,
            vec![],
        );
        let invocation = call
            .content
            .activity
            .as_ref()
            .unwrap()
            .invocation
            .as_ref()
            .unwrap();
        let mut facts = neutral_facts(&call);
        let _ = reconcile_codex_exec_command(&call, invocation, &mut facts);
        assert_eq!(facts.command.as_deref(), Some("git status"), "{input}");
        assert_eq!(facts.declared_tool_workdir.as_deref(), Some("/repo"));
        assert!(!facts.provider_native_context_ambiguous);
    }
    for input in [
        "// await tools.exec_command({cmd: 'git commit'});\ntext('receipt');",
        "text(\"await tools.exec_command({cmd: 'git commit'});\");",
        "if (false) { text(await tools.exec_command({cmd: 'git commit'})); }",
        "const r = await tools.exec_command({cmd: 'git commit'}); text(r.output); text('forged');",
        "const r = await tools.exec_command({cmd: 'git commit'}); r.output = 'forged'; text(r.output);",
        "const r = await tools.exec_command({cmd: 'git commit'}); text(r.output + 'forged');",
        "text(await tools.exec_command({cmd: 'git commit'})); text(await tools.exec_command({cmd: 'git status'}));",
        "text(await Promise.all([tools.exec_command({cmd: 'git commit'})]));",
        "const tools = await tools.exec_command({cmd: 'git commit'}); text(tools.output);",
        "const text = await tools.exec_command({cmd: 'git commit'}); text(text.output);",
        "const cmd = 'git commit'; text(await tools.exec_command({cmd}));",
        "text(await tools.exec_command({cmd: `git ${action}`}));",
        "text(await tools.exec_command({cmd: 'git ' + 'commit'}));",
        "text(await tools.exec_command({cmd: 'git status', cmd: 'git commit'}));",
        r#"text(await tools.exec_command({cmd: 'git status', '\u0063md': 'git commit'}));"#,
        "text(await tools.exec_command({cmd: 'git commit', ...options}));",
        "text(await tools.exec_command({['cmd']: 'git commit'}));",
        "text(await tools.exec_command({get cmd() { return 'git commit' }}));",
        "text(await tools.exec_command({cmd: 'git commit', other: sideEffect()}));",
        "text(await tools.exec_command({cmd: 'git commit', __proto__: null}));",
        "text(tools.exec_command({cmd: 'git commit'}));",
        "text(await tools.exec_command?.({cmd: 'git commit'}));",
        "text(await other.exec_command({cmd: 'git commit'}));",
        r#"text(await tools.exec_command({cmd: 'git \uD800'}));"#,
        r#"text(await tools.exec_command({cmd: `git \uD800`}));"#,
        "text(await tools.exec_command({cmd: ''}));",
        "text(await tools.exec_command({cmd: 'git\\0commit'}));",
        "text(await tools.exec_command({cmd: 'git commit', workdir: ''}));",
        "text(await tools.exec_command({cmd: 'git commit', workdir: null}));",
        "text(await tools.exec_command({cmd: 'git commit'})); broken syntax",
    ] {
        let call = codex_record(
            1,
            "ambiguous",
            Some(invocation(input.to_owned())),
            None,
            vec![fact(LiteralFactKind::Command, "git commit")],
        );
        let invocation = call
            .content
            .activity
            .as_ref()
            .unwrap()
            .invocation
            .as_ref()
            .unwrap();
        let mut facts = neutral_facts(&call);
        let _ = reconcile_codex_exec_command(&call, invocation, &mut facts);
        assert!(facts.command.is_none(), "{input}");
        assert!(facts.declared_tool_workdir.is_none(), "{input}");
    }
}

#[test]
fn code_mode_retains_standard_bounds_and_exact_tool_identity() {
    for (cmd, workdir) in [
        (
            "x".repeat(MAX_CODEX_EXEC_COMMAND_ARGUMENT_BYTES),
            "/repo".to_owned(),
        ),
        (
            "git status".to_owned(),
            "x".repeat(MAX_CODEX_EXEC_COMMAND_WORKDIR_BYTES + 1),
        ),
    ] {
        let input = format!(
            "text(await tools.exec_command({}));",
            serde_json::json!({"cmd":cmd,"workdir":workdir})
        );
        let call = codex_record(1, "oversized", Some(invocation(input)), None, vec![]);
        let invocation = call
            .content
            .activity
            .as_ref()
            .unwrap()
            .invocation
            .as_ref()
            .unwrap();
        assert_eq!(
            exact_codex_exec_command(&call, invocation),
            CodexExecCommandExtraction::Rejected
        );
    }
    let deeply_nested = format!("{}0{}", "(".repeat(20_000), ")".repeat(20_000));
    assert!(super::super::code_mode::arguments(&deeply_nested).is_none());
    let input = "text(await tools.exec_command({cmd: 'git status'}));";
    for (protocol, server) in [(Some("mcp"), None), (None, Some("remote"))] {
        let mut invocation = invocation(input.to_owned());
        invocation.protocol = protocol.map(str::to_owned);
        invocation.server = server.map(str::to_owned);
        let call = codex_record(1, "mcp", Some(invocation.clone()), None, vec![]);
        assert_eq!(
            exact_codex_exec_command(&call, &invocation),
            CodexExecCommandExtraction::Rejected
        );
    }
}

#[test]
fn code_mode_does_not_promote_failed_or_ambiguous_result_envelopes() {
    use super::super::code_mode::Output;
    let wrap = |body: &str| {
        serde_json::json!([
            {"type":"input_text", "text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},
            {"type":"input_text", "text":body}
        ])
    };
    for body in [
        r#"{"exit_code":1,"output":"receipt"}"#,
        r#"{"output":"receipt"}"#,
        r#"{"exit_code":0,"exit_code":1,"output":"receipt"}"#,
        r#"{"exit_code":0,"output":"receipt","output":"different"}"#,
        r#"{"exit_code":0,"output":"receipt","session_id":123}"#,
        r#"{"exit_code":0,"output":["receipt"]}"#,
        r#"{"exit_code":0,"output":"receipt","stdout":"different"}"#,
    ] {
        assert!(Output::Envelope.result(&wrap(body)).is_none(), "{body}");
    }
    let good = wrap("receipt");
    for output in [
        serde_json::json!("receipt"),
        serde_json::json!([good[0], good[1], good[1]]),
        serde_json::json!([{"type":"input_text","text":"Script failed\nOutput:\n"},good[1]]),
        serde_json::json!([good[0],{"type":"input_image","text":"receipt"}]),
    ] {
        assert!(Output::Text.result(&output).is_none());
    }
}

#[test]
fn code_mode_command_abstention_preserves_existing_patch_evidence() {
    let (_temp, repository, _) = initialize_repository();
    let patch = serde_json::to_string(
        "*** Begin Patch\n*** Add File: added.txt\n+synthetic\n*** End Patch",
    )
    .unwrap();
    for script in [
        format!("text(await tools.apply_patch({patch}));"),
        format!(
            "await tools.exec_command({{cmd: 'git status'}}); text(await tools.apply_patch({patch}));"
        ),
    ] {
        let call = codex_record(
            1,
            "patch",
            Some(invocation(script)),
            None,
            vec![fact(
                LiteralFactKind::SessionCwd,
                &repository.to_string_lossy(),
            )],
        );
        let evaluated =
            CoreRepositoryEvidenceAdapter::default().evaluate_record(&call, &"a".repeat(64));
        assert_eq!(evaluated.repository_file_invocation_evidence.len(), 1);
        assert!(evaluated.repository_vcs_observations.is_empty());
    }
}
