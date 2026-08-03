use super::{
    projection::{initialize_repository, outcome_for_sequence},
    *,
};
use serde_json::Value;

fn custom_tool_call(call_id: &str, name: &str, input: Value) -> String {
    serde_json::json!({
        "timestamp": "2026-07-28T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "custom_tool_call",
            "name": name,
            "call_id": call_id,
            "input": input,
        }
    })
    .to_string()
}

#[test]
fn codex_exact_wrapped_pull_request_result_captures_merge_membership() {
    use ctx_history_core::RepositoryVcsObservationKind;
    use std::process::Command;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let run_git = |arguments: &[&str]| {
        assert!(Command::new("/usr/bin/git")
            .arg("-C")
            .arg(&repository)
            .args(arguments)
            .status()
            .unwrap()
            .success());
    };
    let git_output = |arguments: &[&str]| {
        let output = Command::new("/usr/bin/git")
            .arg("-C")
            .arg(&repository)
            .args(arguments)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    run_git(&["branch", "-M", "main"]);
    run_git(&[
        "remote",
        "set-url",
        "origin",
        "https://github.com/ctxrs/ctx.git",
    ]);
    run_git(&["checkout", "-qb", "feature"]);
    run_git(&["commit", "--allow-empty", "-qm", "feature one"]);
    let feature_one = git_output(&["rev-parse", "HEAD"]);
    run_git(&["commit", "--allow-empty", "-qm", "feature two"]);
    let feature_two = git_output(&["rev-parse", "HEAD"]);
    run_git(&["checkout", "-q", "main"]);
    run_git(&["commit", "--allow-empty", "-qm", "main side"]);
    run_git(&[
        "merge",
        "--no-ff",
        "feature",
        "-m",
        "Merge pull request #203 from ctxrs/feature",
    ]);
    let merged_as = git_output(&["rev-parse", "HEAD"]);

    let command = "gh pr view 203 --json state,mergedAt,mergeCommit,url\n\
                   git fetch origin main\n\
                   git log -1 --oneline origin/main";
    let call_id = "call_5eCij3bzNa2V5SUxyUIR9INA";
    let wrapped_output = format!(
        concat!(
            "Chunk ID: 57a5b6\n",
            "Wall time: 0.3549 seconds\n",
            "Process exited with code 0\n",
            "Original token count: 95\n",
            "Output:\n",
            "{{\"mergeCommit\":{{\"oid\":\"{}\"}},",
            "\"mergedAt\":\"2026-07-28T01:19:34Z\",\"state\":\"MERGED\",",
            "\"url\":\"https://github.com/ctxrs/ctx/pull/203\"}}\n",
            "From https://github.com/ctxrs/ctx\n",
            "103c01056 merge\n",
        ),
        merged_as
    );
    let function_call = serde_json::json!({
        "timestamp": "2026-07-28T01:19:33Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": call_id,
            "arguments": serde_json::json!({
                "cmd": command,
                "workdir": repository.to_string_lossy(),
                "yield_time_ms": 10_000,
                "max_output_tokens": 30_000,
            }).to_string(),
        }
    })
    .to_string();
    let function_call_output = serde_json::json!({
        "timestamp": "2026-07-28T01:19:34Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "output": wrapped_output,
        }
    })
    .to_string();
    let native_session_id = "019f95c9-2671-79f3-bf13-e7e0dd7eb68c";
    write_session(
        &sessions,
        native_session_id,
        &[function_call, function_call_output],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let core = outcome_for_sequence(&verified, session_id, 2);
    let associations = core
        .repository_vcs_observations
        .iter()
        .filter_map(|observation| match &observation.kind {
            RepositoryVcsObservationKind::PullRequestAssociation(association) => {
                Some(association.as_ref())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let [association] = associations.as_slice() else {
        panic!(
            "expected one pull-request association: observations={:?} abstentions={:?}",
            core.repository_vcs_observations, core.repository_abstentions
        );
    };
    assert_eq!(association.pull_request.number, 203);
    assert_eq!(association.merged_as.hex, merged_as);
    let mut expected_feature_side = vec![feature_one, feature_two];
    expected_feature_side.sort();
    assert_eq!(
        association
            .contains_commits
            .iter()
            .map(|commit| commit.hex.clone())
            .collect::<Vec<_>>(),
        expected_feature_side
    );
    assert_eq!(
        core.content.structured_content.as_ref().unwrap()["provider_native_tool_activities"][0]
            ["provider_native_tool_result"]["captured_pull_request_associations"],
        1
    );
}

#[test]
fn codex_complete_patch_publishes_strict_provider_neutral_invocation_evidence() {
    use ctx_history_core::RepositoryFileInvocationKind;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let native_session_id = "019fa000-0000-7000-8000-000000000199";
    let new_path = repository.join("src/new.rs");
    let modified_path = repository.join("src/lib.rs");
    let old_path = repository.join("src/old.rs");
    let moved_path = repository.join("src/moved.rs");
    let patch = format!(
        "*** Begin Patch\n*** Add File: {}\n+new\n*** Update File: {}\n@@\n-old\n+new\n*** Update File: {}\n*** Move to: {}\n*** End Patch",
        new_path.display(),
        modified_path.display(),
        old_path.display(),
        moved_path.display(),
    );
    write_session(
        &sessions,
        native_session_id,
        &[custom_tool_call(
            "strict-complete-patch",
            "apply_patch",
            Value::String(patch),
        )],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let core = outcome_for_sequence(&verified, session_id, 1);
    let body = core.content.normalized_body.as_deref().unwrap();
    assert_eq!(core.repository_file_invocation_evidence.len(), 3);

    let invocation = |path: &str| {
        core.repository_file_invocation_evidence
            .iter()
            .find(|evidence| evidence.relative_path == path)
            .unwrap()
    };
    let created = invocation("src/new.rs");
    assert_eq!(created.kind, RepositoryFileInvocationKind::Create);
    assert_eq!(created.operation_ordinal, 0);
    assert_eq!(created.tool_name.as_deref(), Some("apply_patch"));
    let modified = invocation("src/lib.rs");
    assert_eq!(modified.kind, RepositoryFileInvocationKind::Modify);
    assert_eq!(modified.operation_ordinal, 1);
    let renamed = invocation("src/moved.rs");
    assert_eq!(renamed.kind, RepositoryFileInvocationKind::Rename);
    assert_eq!(renamed.operation_ordinal, 2);
    assert_eq!(renamed.prior_relative_path.as_deref(), Some("src/old.rs"));

    let selected = |evidence: &ctx_history_core::RepositoryFileInvocationEvidence| {
        let range = evidence.normalized_text_range.unwrap();
        &body[range.start as usize..range.end as usize]
    };
    assert_eq!(
        selected(created),
        format!("*** Add File: {}", new_path.display())
    );
    assert_eq!(
        selected(modified),
        format!("*** Update File: {}", modified_path.display())
    );
    assert_eq!(
        selected(renamed),
        format!(
            "*** Update File: {}\n*** Move to: {}",
            old_path.display(),
            moved_path.display()
        )
    );
    assert!(core
        .repository_file_invocation_evidence
        .iter()
        .all(|evidence| evidence.repository_binding_id == core.repository_bindings[0].binding_id));
}

#[test]
fn codex_patch_at_shared_ceiling_preserves_ordinary_and_strict_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let native_session_id = "019fa000-0000-7000-8000-000000000197";
    let mut patch = String::from("*** Begin Patch\n");
    for operation_ordinal in 0..32 {
        patch.push_str(&format!(
            "*** Add File: {}\n+value\n",
            repository
                .join(format!("src/generated-{operation_ordinal}.rs"))
                .display()
        ));
    }
    patch.push_str("*** End Patch");
    write_session(
        &sessions,
        native_session_id,
        &[custom_tool_call(
            "strict-ceiling-patch",
            "apply_patch",
            Value::String(patch),
        )],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let core = outcome_for_sequence(&verified, session_id, 1);
    assert_eq!(core.repository_file_observations.len(), 32);
    assert_eq!(core.repository_file_invocation_evidence.len(), 32);
    assert_eq!(
        core.repository_file_invocation_evidence
            .iter()
            .map(|evidence| evidence.operation_ordinal)
            .collect::<Vec<_>>(),
        (0..32).collect::<Vec<_>>()
    );
    assert!(!core.repository_abstentions.iter().any(|abstention| {
        abstention.reason == ctx_history_core::RepositoryAbstentionReason::CandidateLimitExceeded
    }));
}

#[test]
fn codex_patch_above_strict_bound_publishes_typed_abstention_without_partial_invocations() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let native_session_id = "019fa000-0000-7000-8000-000000000196";
    let mut patch = String::from("*** Begin Patch\n");
    for operation_ordinal in 0..33 {
        patch.push_str(&format!(
            "*** Add File: {}\n+value\n",
            repository
                .join(format!("src/overflow-{operation_ordinal}.rs"))
                .display()
        ));
    }
    patch.push_str("*** End Patch");
    write_session(
        &sessions,
        native_session_id,
        &[custom_tool_call(
            "strict-overflow-patch",
            "apply_patch",
            Value::String(patch),
        )],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let core = outcome_for_sequence(&verified, session_id, 1);
    assert!(core.repository_file_invocation_evidence.is_empty());
    assert!(core.repository_abstentions.iter().any(|abstention| {
        abstention.reason == ctx_history_core::RepositoryAbstentionReason::CandidateLimitExceeded
    }));
}

#[test]
fn codex_generic_recursive_file_touch_never_promotes_to_invocation_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let native_session_id = "019fa000-0000-7000-8000-000000000198";
    let generic_path = repository.join("src/generic.rs");
    write_session(
        &sessions,
        native_session_id,
        &[custom_tool_call(
            "generic-recursive-touch",
            "provider_file_tool",
            serde_json::json!({
                "operation": "modify",
                "nested": {"file_path": generic_path},
            }),
        )],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let core = outcome_for_sequence(&verified, session_id, 1);
    assert!(core.repository_file_invocation_evidence.is_empty());
    assert_eq!(core.repository_file_observations.len(), 1);
    assert_eq!(
        core.repository_file_observations[0].relative_path,
        "src/generic.rs"
    );
}
