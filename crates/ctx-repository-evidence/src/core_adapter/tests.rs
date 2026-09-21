use std::{fs, path::Path, process::Command};

use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, AgentScope,
    CORE_ACTIVITY_REVISION, CoreActivity, EventIdentityInput, NativeItemKey, NativeSessionKey,
    ProviderDeclaredFact, ProviderNativeSessionRelationship, SessionIdentityInput, SourceAnchor,
    SourceKey, TypedKey, derive_event_id, derive_session_id,
};

use super::*;

fn record(sequence: u64, facts: Vec<ProviderDeclaredFact>, invocation: bool) -> CoreRecord {
    let source = SourceKey::derive(
        "adapter-test",
        "adapter-jsonl",
        "adapter-v1",
        1,
        SourceAnchor::provider_native(
            "adapter-test",
            TypedKey::utf8("source").expect("source key"),
        )
        .expect("source anchor"),
    )
    .expect("source");
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &NativeSessionKey::native_id(
            "session",
            TypedKey::utf8("session").expect("session key"),
        )
        .expect("native session"),
    })
    .expect("session");
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(sequence))
            .expect("native event"),
        subrecord_selector: None,
    })
    .expect("event");
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source,
        sequence,
        "tool_call",
        "adapter-test-v1",
        "body",
    )
    .expect("record");
    record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8("call").expect("call id")),
        invocation: invocation.then_some(ActivityInvocation {
            protocol: None,
            server: None,
            tool: "shell".to_owned(),
            arguments: ActivityJsonCapture::Absent,
            started_at_unix_ms: None,
        }),
        result: None,
        facts,
    });
    record
}

fn fact(kind: LiteralFactKind, value: &str) -> ProviderDeclaredFact {
    ProviderDeclaredFact {
        kind,
        value: value.to_owned(),
    }
}

fn codex_record(
    sequence: u64,
    call_id: &str,
    invocation: Option<ActivityInvocation>,
    result: Option<ActivityResult>,
    facts: Vec<ProviderDeclaredFact>,
) -> CoreRecord {
    let source = SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "codex-nativepath-jsonl-v0",
        1,
        SourceAnchor::provider_native(
            "codex.session",
            TypedKey::utf8("codex-session").expect("source key"),
        )
        .expect("source anchor"),
    )
    .expect("Codex source");
    let session = |native: &str| {
        derive_session_id(SessionIdentityInput {
            source: &source,
            logical_session_kind: "thread",
            native_session_key: &NativeSessionKey::native_id(
                "session",
                TypedKey::utf8(native).expect("session key"),
            )
            .expect("native session"),
        })
        .expect("session")
    };
    let session_id = session("child");
    let parent_session_id = session("parent");
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "tool_call",
        native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(sequence))
            .expect("native event"),
        subrecord_selector: None,
    })
    .expect("event");
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source,
        sequence,
        if result.is_some() {
            "command_output"
        } else {
            "tool_call"
        },
        "codex-nativepath-core-activity-v14-literal-patch-file-facts",
        "body",
    )
    .expect("record");
    record.provider_session_id = Some("codex-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(sequence));
    record.parent_session_id = Some(parent_session_id);
    record.root_session_id = Some(parent_session_id);
    record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    record.agent_scope = Some(AgentScope::Subagent);
    record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8(call_id).expect("call ID")),
        invocation,
        result,
        facts,
    });
    record.validate_contract().expect("Codex record");
    record
}

fn codex_exec_invocation(arguments: impl Into<String>) -> ActivityInvocation {
    ActivityInvocation {
        protocol: None,
        server: None,
        tool: "exec_command".to_owned(),
        arguments: ActivityJsonCapture::Present {
            value: Value::String(arguments.into()),
        },
        started_at_unix_ms: Some(1_787_126_403_000),
    }
}

fn run_git(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

fn initialize_repository() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let temporary = tempfile::tempdir().expect("temporary repository");
    let repository = temporary.path().join("repository");
    fs::create_dir(&repository).expect("create repository");
    run_git(&repository, &["init", "-q", "-b", "main"]);
    run_git(&repository, &["config", "user.name", "ctx test"]);
    run_git(
        &repository,
        &["config", "user.email", "ctx@example.invalid"],
    );
    fs::write(repository.join("README.md"), "fixture\n").expect("write initial file");
    run_git(&repository, &["add", "README.md"]);
    run_git(&repository, &["commit", "-qm", "initial"]);
    fs::write(repository.join("fixture.txt"), "dogfood fixture\n").expect("write committed file");
    run_git(&repository, &["add", "fixture.txt"]);
    run_git(&repository, &["commit", "-qm", "dogfood fixture"]);
    let short = run_git(&repository, &["rev-parse", "--short=8", "HEAD"]);
    (temporary, repository, short)
}

#[test]
fn neutral_adapter_preserves_exact_literals_and_rejects_ambiguous_context() {
    let exact = record(
        1,
        vec![
            fact(LiteralFactKind::SessionCwd, "/repo"),
            fact(LiteralFactKind::SessionCwd, "/repo"),
            fact(LiteralFactKind::ToolWorkdir, "/repo/work"),
            fact(LiteralFactKind::Command, "git status"),
            fact(LiteralFactKind::File, "src/lib.rs"),
            fact(
                LiteralFactKind::Commit,
                "0123456789abcdef0123456789abcdef01234567",
            ),
            fact(
                LiteralFactKind::Commit,
                "0123456789ABCDEF0123456789ABCDEF01234567",
            ),
        ],
        false,
    );
    let facts = neutral_facts(&exact);
    assert_eq!(facts.session_cwd.as_deref(), Some("/repo"));
    assert_eq!(facts.declared_tool_workdir.as_deref(), Some("/repo/work"));
    assert_eq!(facts.command.as_deref(), Some("git status"));
    assert!(!facts.provider_native_context_ambiguous);
    assert_eq!(facts.file_observations.len(), 1);
    assert_eq!(
        facts
            .vcs_observations
            .iter()
            .filter(|observation| observation.object_id.is_some())
            .count(),
        1
    );

    let ambiguous = record(
        2,
        vec![
            fact(LiteralFactKind::Command, "git status"),
            fact(LiteralFactKind::Command, "git commit"),
        ],
        false,
    );
    let facts = neutral_facts(&ambiguous);
    assert!(facts.command.is_none());
    assert!(facts.provider_native_context_ambiguous);
    assert_eq!(
        facts.command_disposition,
        CommandLiteralDisposition::CommandTooLarge
    );
}

#[test]
fn duplicate_call_ids_fail_closed_and_source_reset_clears_linkage() {
    let mut adapter = CoreRepositoryEvidenceAdapter::default();
    let invocation = record(1, Vec::new(), true);
    let _ = adapter.evaluate_record(&invocation, &"a".repeat(64));
    assert_eq!(adapter.pending.len(), 1);
    let duplicate = record(2, Vec::new(), true);
    let _ = adapter.evaluate_record(&duplicate, &"b".repeat(64));
    assert!(adapter.pending.values().all(Option::is_none));
    adapter.begin_source();
    assert!(adapter.pending.is_empty());
}

#[test]
fn exact_codex_exec_command_string_links_the_matching_commit_result() {
    for revision in [
        "codex-nativepath-core-activity-v11-item-call-identity",
        "codex-nativepath-core-activity-v14-literal-patch-file-facts",
        "codex-nativepath-core-activity-v15-revert-lineage",
    ] {
        let (_temporary, repository, short) = initialize_repository();
        let workdir = repository.to_string_lossy();
        let arguments = serde_json::json!({
            "cmd": "git commit -m dogfood-fixture",
            "workdir": workdir,
            "yield_time_ms": 10_000,
        })
        .to_string();
        let mut invocation = codex_record(
            1,
            "dogfood-commit",
            Some(codex_exec_invocation(arguments)),
            None,
            vec![fact(LiteralFactKind::SessionCwd, &workdir)],
        );
        invocation.parser_revision = revision.to_owned();
        let output = format!(
            "Chunk ID: dogfood\nWall time: 0.1 seconds\nProcess exited with code 0\nFinal output:\n[main {short}] dogfood fixture\n 1 file changed, 1 insertion(+)\n"
        );
        let mut result = codex_record(
            2,
            "dogfood-commit",
            None,
            Some(ActivityResult {
                status: None,
                completed_at_unix_ms: Some(1_787_126_404_000),
                duration_ns: None,
                text: ActivityTextCapture::Present {
                    value: output.clone(),
                },
                structured_content: ActivityJsonCapture::Present {
                    value: Value::String(output),
                },
            }),
            vec![fact(LiteralFactKind::SessionCwd, &workdir)],
        );
        result.parser_revision = revision.to_owned();

        let mut adapter = CoreRepositoryEvidenceAdapter::default();
        adapter.begin_source();
        let invocation_evaluation = adapter.evaluate_record(&invocation, &"a".repeat(64));
        assert_eq!(invocation_evaluation.repository_bindings.len(), 1);
        let result_evaluation = adapter.evaluate_record(&result, &"b".repeat(64));
        assert_eq!(result_evaluation.repository_bindings.len(), 1);
        assert!(
            result_evaluation
                .repository_vcs_observations
                .iter()
                .any(|observation| {
                    matches!(observation.kind, RepositoryVcsObservationKind::Outcome(_))
                })
        );

        let mut reverse = CoreRepositoryEvidenceAdapter::default();
        reverse.begin_source();
        let early_result = reverse.evaluate_record(&result, &"b".repeat(64));
        assert!(early_result.repository_vcs_observations.is_empty());
        let linked_invocation = reverse.evaluate_record(&invocation, &"a".repeat(64));
        assert!(
            linked_invocation
                .repository_vcs_observations
                .iter()
                .any(|observation| {
                    matches!(observation.kind, RepositoryVcsObservationKind::Outcome(_))
                })
        );

        let mut ambiguous_reverse = CoreRepositoryEvidenceAdapter::default();
        ambiguous_reverse.begin_source();
        let _ = ambiguous_reverse.evaluate_record(&result, &"b".repeat(64));
        let _ = ambiguous_reverse.evaluate_record(&result, &"c".repeat(64));
        let ambiguous_invocation = ambiguous_reverse.evaluate_record(&invocation, &"a".repeat(64));
        assert!(ambiguous_invocation.repository_vcs_observations.is_empty());
    }
}

#[test]
fn codex_exec_command_near_misses_abstain_without_overriding_literal_facts() {
    for revision in [
        "codex-nativepath-core-activity-v11-item-call-identity",
        "codex-nativepath-core-activity-v14-literal-patch-file-facts",
        "codex-nativepath-core-activity-v15-revert-lineage",
    ] {
        let mut exact = codex_record(
            1,
            "call",
            Some(codex_exec_invocation(
                r#"{"cmd":"git status","workdir":"/repo"}"#,
            )),
            None,
            vec![
                fact(LiteralFactKind::Command, "git status"),
                fact(LiteralFactKind::ToolWorkdir, "/repo"),
                fact(LiteralFactKind::File, "src/mentioned.rs"),
            ],
        );
        exact.parser_revision = revision.to_owned();
        let invocation = exact
            .content
            .activity
            .as_ref()
            .and_then(|activity| activity.invocation.as_ref())
            .expect("invocation");
        assert_eq!(
            exact_codex_exec_command(&exact, invocation),
            CodexExecCommandExtraction::Exact {
                command: "git status".to_owned(),
                workdir: Some("/repo".to_owned()),
            }
        );
        // v14 retains the argument string and adds neutral literal facts. Exact
        // agreement is allowed; a file mention does not assert an operation.
        let mut facts = neutral_facts(&exact);
        reconcile_codex_exec_command(&exact, invocation, &mut facts);
        assert_eq!(facts.command.as_deref(), Some("git status"));
        assert_eq!(facts.declared_tool_workdir.as_deref(), Some("/repo"));
        assert!(!facts.provider_native_context_ambiguous);
        assert_eq!(facts.file_observations.len(), 1);
        assert_eq!(
            facts.file_observations[0].kind,
            RepositoryFileObservationKind::Unknown
        );

        for revision in [
            "codex-nativepath-core-activity-v10-item-completed-plan",
            "codex-nativepath-core-activity-v11-item-call-identity-unknown",
            "codex-nativepath-core-activity-v13-bounded-json-argument-facts",
            "codex-nativepath-core-activity-v14-literal-patch-file-facts-unknown",
            "codex-nativepath-core-activity-v15-revert-lineage-unknown",
        ] {
            let mut old_or_unknown = exact.clone();
            old_or_unknown.parser_revision = revision.to_owned();
            assert_eq!(
                exact_codex_exec_command(&old_or_unknown, invocation),
                CodexExecCommandExtraction::NotApplicable
            );
        }

        for arguments in [
            r#"{"cmd":"git status","cmd":"git commit"}"#.to_owned(),
            "{not-json".to_owned(),
            serde_json::json!({"cmd": "x".repeat(MAX_CODEX_EXEC_COMMAND_BYTES + 1)}).to_string(),
        ] {
            let mut record = codex_record(
                2,
                "call",
                Some(codex_exec_invocation(arguments)),
                None,
                Vec::new(),
            );
            record.parser_revision = revision.to_owned();
            let invocation = record
                .content
                .activity
                .as_ref()
                .and_then(|activity| activity.invocation.as_ref())
                .expect("invocation");
            assert_eq!(
                exact_codex_exec_command(&record, invocation),
                CodexExecCommandExtraction::Rejected
            );
        }

        let mut wrong_tool = exact.clone();
        wrong_tool
            .content
            .activity
            .as_mut()
            .and_then(|activity| activity.invocation.as_mut())
            .expect("invocation")
            .tool = "shell".to_owned();
        assert_eq!(
            exact_codex_exec_command(
                &wrong_tool,
                wrong_tool
                    .content
                    .activity
                    .as_ref()
                    .and_then(|activity| activity.invocation.as_ref())
                    .expect("invocation")
            ),
            CodexExecCommandExtraction::NotApplicable
        );

        let wrong_provider = record(3, Vec::new(), true);
        assert_eq!(
            exact_codex_exec_command(
                &wrong_provider,
                wrong_provider
                    .content
                    .activity
                    .as_ref()
                    .and_then(|activity| activity.invocation.as_ref())
                    .expect("invocation")
            ),
            CodexExecCommandExtraction::NotApplicable
        );

        let mut conflicting = exact.clone();
        conflicting
            .content
            .activity
            .as_mut()
            .expect("activity")
            .facts = vec![fact(LiteralFactKind::Command, "git commit")];
        let mut facts = neutral_facts(&conflicting);
        reconcile_codex_exec_command(
            &conflicting,
            conflicting
                .content
                .activity
                .as_ref()
                .and_then(|activity| activity.invocation.as_ref())
                .expect("invocation"),
            &mut facts,
        );
        assert!(facts.command.is_none());
        assert!(facts.provider_native_context_ambiguous);

        let mut ambiguous = exact;
        ambiguous.content.activity.as_mut().expect("activity").facts = vec![
            fact(LiteralFactKind::SessionCwd, "/repo-a"),
            fact(LiteralFactKind::SessionCwd, "/repo-b"),
        ];
        let mut facts = neutral_facts(&ambiguous);
        reconcile_codex_exec_command(
            &ambiguous,
            ambiguous
                .content
                .activity
                .as_ref()
                .and_then(|activity| activity.invocation.as_ref())
                .expect("invocation"),
            &mut facts,
        );
        assert!(facts.command.is_none());
        assert!(facts.declared_tool_workdir.is_none());
        assert!(facts.provider_native_context_ambiguous);
    }
}
