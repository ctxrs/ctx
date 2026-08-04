use super::*;
use crate::provider::codex::nativepath::tests::discover_one;
use serde_json::Value;

pub(super) fn initialize_repository(path: &Path) {
    use std::process::Command;

    fs::create_dir(path).unwrap();
    for arguments in [
        vec!["init", "-q"],
        vec!["config", "user.name", "ctx test"],
        vec!["config", "user.email", "ctx@example.invalid"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/acme/codex-fixture.git",
        ],
    ] {
        assert!(Command::new("/usr/bin/git")
            .arg("-C")
            .arg(path)
            .args(arguments)
            .status()
            .unwrap()
            .success());
    }
    fs::write(path.join("tracked.txt"), "tracked\n").unwrap();
    for arguments in [vec!["add", "tracked.txt"], vec!["commit", "-qm", "fixture"]] {
        assert!(Command::new("/usr/bin/git")
            .arg("-C")
            .arg(path)
            .args(arguments)
            .status()
            .unwrap()
            .success());
    }
}

fn exec_call(call_id: &str, command: &str, workdir: &Path) -> String {
    serde_json::json!({
        "timestamp": "2026-07-28T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": call_id,
            "arguments": serde_json::json!({
                "cmd": command,
                "workdir": workdir,
                "yield_time_ms": 10000
            }).to_string()
        }
    })
    .to_string()
}

fn successful_result(call_id: &str, output: Value) -> String {
    serde_json::json!({
        "timestamp": "2026-07-28T12:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "status": "success",
            "output": output
        }
    })
    .to_string()
}

fn wait_call(call_id: &str, cell_id: &str) -> String {
    serde_json::json!({
        "timestamp": "2026-07-28T12:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "wait",
            "call_id": call_id,
            "arguments": serde_json::json!({"cell_id": cell_id}).to_string()
        }
    })
    .to_string()
}

fn running_result(call_id: &str, cell_id: &str) -> String {
    successful_result(
        call_id,
        Value::String(format!("Script running with cell ID {cell_id}\n")),
    )
}

pub(super) fn outcome_for_sequence(
    index: &VerifiedIndex,
    session_id: StableEntityId,
    sequence: u64,
) -> ctx_history_core::CoreRecord {
    let event = index
        .events_for_session(session_id.as_uuid())
        .unwrap()
        .into_iter()
        .find(|event| event.event_sequence == sequence)
        .unwrap();
    index
        .core_record_by_id(event.event_id.as_uuid())
        .unwrap()
        .unwrap()
}

#[test]
fn codex_exact_commit_result_publishes_scoped_outcome_and_complete_raw_output() {
    use ctx_history_core::{RepositoryOutcomeKind, RepositoryVcsObservationKind};

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let native_session_id = "019fa000-0000-7000-8000-000000000100";
    let oid = "0123456789abcdef0123456789abcdef01234567";
    write_session(
        &sessions,
        native_session_id,
        &[
            exec_call(
                "commit-call",
                "git commit -m exact && git rev-parse --verify HEAD",
                &repository,
            ),
            successful_result(
                "commit-call",
                Value::String(format!(
                    "Process exited with code 0\nFinal output:\n[main abc1234] exact\n{oid}\n"
                )),
            ),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let core = outcome_for_sequence(&verified, session_id, 2);
    assert_eq!(core.repository_bindings.len(), 1);
    let RepositoryVcsObservationKind::Outcome(outcome) = &core.repository_vcs_observations[0].kind
    else {
        panic!("expected repository outcome");
    };
    assert_eq!(outcome.kind, RepositoryOutcomeKind::Commit);
    assert_eq!(outcome.produced_object_ids[0].hex, oid);
    assert_eq!(outcome.linkage.origin_call_id, "commit-call");
    assert_eq!(outcome.linkage.result_call_id, "commit-call");
    assert_eq!(outcome.linkage.origin_event_sequence, 1);
    let structured = core.content.structured_content.as_ref().unwrap();
    assert_eq!(
        structured["provider_content"]["provider_native_tool_result"]["result_content_location"],
        "normalized_body"
    );
    assert_eq!(
        structured["provider_content"]["provider_native_tool_result"]["result_content_complete"],
        true
    );
    assert!(core
        .content
        .normalized_body
        .as_deref()
        .unwrap()
        .contains(oid));
    assert_eq!(
        structured["provider_native_tool_activities"][0]["provider_native_tool_result"]
            ["raw_output_retained"],
        false
    );
    assert!(!serde_json::to_string(structured).unwrap().contains(oid));
}

#[test]
fn codex_forked_history_attributes_one_canonical_execution_origin() {
    use ctx_history_core::{RepositoryAbstentionReason, RepositoryVcsObservationKind};

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let parent_native_session_id = "019fa000-0000-7000-8000-000000000190";
    let child_native_session_id = "019fa000-0000-7000-8000-000000000191";
    let copied_oid = "518dedb053f04ab0b529c7d2e8dafb322974fbf6";
    let child_oid = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let copied_call = exec_call(
        "call-canonical-execution",
        "git commit -m exact && git rev-parse --verify HEAD",
        &repository,
    );
    let copied_result = successful_result(
        "call-canonical-execution",
        Value::String(format!("[main 518dedb] exact\n{copied_oid}\n")),
    );
    write_session(
        &sessions,
        parent_native_session_id,
        &[
            copied_call.clone(),
            copied_result.clone(),
            "{malformed unrelated record".to_owned(),
        ],
    );
    write_forked_session(
        &sessions,
        child_native_session_id,
        parent_native_session_id,
        &[
            copied_call,
            copied_result,
            exec_call(
                "call-child-execution",
                "git commit -m child && git rev-parse --verify HEAD",
                &repository,
            ),
            successful_result(
                "call-child-execution",
                Value::String(format!("[main aaaaaaa] child\n{child_oid}\n")),
            ),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let parent_source = codex_source_key(parent_native_session_id).unwrap();
    let parent_session = codex_session_identity(&parent_source, parent_native_session_id).unwrap();
    let child_source = codex_source_key(child_native_session_id).unwrap();
    let child_session = codex_session_identity(&child_source, child_native_session_id).unwrap();

    let parent_result = outcome_for_sequence(&verified, parent_session, 2);
    assert_eq!(parent_result.repository_vcs_observations.len(), 1);
    let copied_child_result = outcome_for_sequence(&verified, child_session, 2);
    assert!(copied_child_result.repository_vcs_observations.is_empty());
    assert!(copied_child_result
        .repository_abstentions
        .iter()
        .any(|abstention| {
            abstention.reason == RepositoryAbstentionReason::ProviderOutputUnjoined
                && abstention.detail.as_deref()
                    == Some("copied_provider_history_has_ancestor_execution")
        }));

    let unique_child_result = outcome_for_sequence(&verified, child_session, 4);
    let RepositoryVcsObservationKind::Outcome(outcome) =
        &unique_child_result.repository_vcs_observations[0].kind
    else {
        panic!("expected unique child outcome");
    };
    assert_eq!(outcome.produced_object_ids[0].hex, child_oid);
    assert_eq!(outcome.linkage.origin_call_id, "call-child-execution");
}

#[test]
fn codex_active_parent_append_during_lineage_publishes_prefix_then_imports_suffix_once() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let parent_native_session_id = "019fa000-0000-7000-8000-000000000194";
    let child_native_session_id = "019fa000-0000-7000-8000-000000000195";
    let copied_call = exec_call(
        "call-active-parent",
        "git commit -m exact && git rev-parse --verify HEAD",
        &repository,
    );
    let copied_result = successful_result(
        "call-active-parent",
        Value::String(
            "[main 518dedb] exact\n518dedb053f04ab0b529c7d2e8dafb322974fbf6\n".to_owned(),
        ),
    );
    write_session(
        &sessions,
        parent_native_session_id,
        &[copied_call.clone(), copied_result.clone()],
    );
    write_forked_session(
        &sessions,
        child_native_session_id,
        parent_native_session_id,
        &[copied_call, copied_result],
    );

    let deferred = message("user", "deferred active-parent suffix");
    let parent_path = session_path(&sessions, parent_native_session_id);
    install_after_codex_catalog_authority_hook(move || {
        let mut file = OpenOptions::new().append(true).open(&parent_path).unwrap();
        writeln!(file, "{deferred}").unwrap();
    });
    let cold = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(cold.counters.cold_sources, 2);

    let parent_source = codex_source_key(parent_native_session_id).unwrap();
    let parent_session = codex_session_identity(&parent_source, parent_native_session_id).unwrap();
    let first = VerifiedIndex::open(&index).unwrap();
    assert_eq!(
        first
            .events_for_session(parent_session.as_uuid())
            .unwrap()
            .len(),
        2
    );
    let first_generation = first.generation_id().to_owned();

    let catch_up = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(catch_up.counters.appended_sources, 1);
    assert_eq!(catch_up.counters.replaced_sources, 1);
    let second = VerifiedIndex::open(&index).unwrap();
    assert_ne!(second.generation_id(), first_generation);
    assert_eq!(
        second
            .events_for_session(parent_session.as_uuid())
            .unwrap()
            .len(),
        3
    );

    let replay = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(replay.counters.replayed_sources, 2);
    assert_eq!(replay.commit.generation_id, catch_up.commit.generation_id);
    let third = VerifiedIndex::open(&index).unwrap();
    assert_eq!(
        third
            .events_for_session(parent_session.as_uuid())
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn codex_discovery_prefix_rewrite_and_append_rolls_back_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let parent_native_session_id = "019fa000-0000-7000-8000-000000000198";
    let child_native_session_id = "019fa000-0000-7000-8000-000000000199";
    let copied_call = exec_call(
        "call-prefix-race",
        "git commit -m exact && git rev-parse --verify HEAD",
        &repository,
    );
    let copied_result = successful_result(
        "call-prefix-race",
        Value::String(
            "[main 518dedb] exact\n518dedb053f04ab0b529c7d2e8dafb322974fbf6\n".to_owned(),
        ),
    );
    write_session(
        &sessions,
        parent_native_session_id,
        &[copied_call.clone(), copied_result.clone()],
    );
    write_forked_session(
        &sessions,
        child_native_session_id,
        parent_native_session_id,
        &[copied_call, copied_result],
    );
    let cold = ingest_codex_source_backed_v0(&sessions, &index).unwrap();

    let parent_path = session_path(&sessions, parent_native_session_id);
    writeln!(
        OpenOptions::new().append(true).open(&parent_path).unwrap(),
        "{}",
        message("user", "prefix race dependency change")
    )
    .unwrap();
    let marker = b"git commit -m exact";
    let marker_offset = fs::read(&parent_path)
        .unwrap()
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap();
    install_after_codex_catalog_authority_hook(move || {
        let mut file = OpenOptions::new().write(true).open(&parent_path).unwrap();
        file.seek(SeekFrom::Start(marker_offset as u64)).unwrap();
        file.write_all(b"Git commit -m exact").unwrap();
        drop(file);
        writeln!(
            OpenOptions::new().append(true).open(&parent_path).unwrap(),
            "{}",
            message("user", "rewrite-plus-append race suffix")
        )
        .unwrap();
    });

    assert!(ingest_codex_source_backed_v0(&sessions, &index).is_err());
    let after = VerifiedIndex::open(&index).unwrap();
    assert_eq!(after.generation_id(), cold.commit.generation_id);
}

#[test]
fn codex_forked_history_abstains_when_parent_origin_is_unavailable() {
    use ctx_history_core::RepositoryAbstentionReason;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let child_native_session_id = "019fa000-0000-7000-8000-000000000192";
    let missing_parent = "019fa000-0000-7000-8000-000000000193";
    let oid = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    write_forked_session(
        &sessions,
        child_native_session_id,
        missing_parent,
        &[
            exec_call(
                "call-unproven-origin",
                "git commit -m exact && git rev-parse --verify HEAD",
                &repository,
            ),
            successful_result(
                "call-unproven-origin",
                Value::String(format!("[main bbbbbbb] exact\n{oid}\n")),
            ),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(child_native_session_id).unwrap();
    let session = codex_session_identity(&source, child_native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let result = outcome_for_sequence(&verified, session, 2);
    assert!(result.repository_vcs_observations.is_empty());
    assert!(result.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::ProviderOutputUnjoined
            && abstention.detail.as_deref() == Some("provider_execution_origin_lineage_unproven")
    }));
}

#[test]
fn codex_forked_history_limits_malformed_lineage_ambiguity_to_matching_call_id() {
    use ctx_history_core::RepositoryAbstentionReason;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let parent_native_session_id = "019fa000-0000-7000-8000-000000000196";
    let child_native_session_id = "019fa000-0000-7000-8000-000000000197";
    let call_id = "call-malformed-lineage";
    write_session(
        &sessions,
        parent_native_session_id,
        &[format!(r#"{{"call_id":"{call_id}", malformed"#)],
    );
    write_forked_session(
        &sessions,
        child_native_session_id,
        parent_native_session_id,
        &[
            exec_call(
                call_id,
                "git commit -m exact && git rev-parse --verify HEAD",
                &repository,
            ),
            successful_result(
                call_id,
                Value::String(
                    "[main bbbbbbb] exact\nbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n".to_owned(),
                ),
            ),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let child_source = codex_source_key(child_native_session_id).unwrap();
    let child_session = codex_session_identity(&child_source, child_native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let result = outcome_for_sequence(&verified, child_session, 2);
    assert!(result.repository_vcs_observations.is_empty());
    assert!(result.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::ProviderOutputUnjoined
            && abstention.detail.as_deref() == Some("provider_execution_origin_lineage_unproven")
    }));
}

#[test]
fn codex_success_without_binding_and_failed_or_mismatched_results_fail_closed() {
    use ctx_history_core::RepositoryAbstentionReason;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let missing = temp.path().join("not-a-repository");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(&missing, b"not a directory\n").unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000101";
    let oid = "1111111111111111111111111111111111111111";
    write_session(
        &sessions,
        native_session_id,
        &[
            exec_call("unbound", "git commit -m exact", &missing),
            successful_result("unbound", serde_json::json!({"commit_oid": oid})),
            exec_call("failed", "git commit -m failed", &missing),
            serde_json::json!({
                "timestamp": "2026-07-28T12:00:03Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "failed",
                    "output": "Process exited with code 1\ncommit failed"
                }
            })
            .to_string(),
            exec_call("prose", "git commit -m prose", &missing),
            successful_result(
                "prose",
                Value::String(format!("commit completed near diagnostic token {oid}")),
            ),
            exec_call("mismatch-origin", "git commit -m mismatch", &missing),
            successful_result(
                "different-result-id",
                serde_json::json!({"commit_oid": oid}),
            ),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let unbound = outcome_for_sequence(&verified, session_id, 2);
    assert!(unbound.repository_vcs_observations.is_empty());
    assert!(unbound.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::OutcomeRepositoryUnbound
    }));
    let failed = outcome_for_sequence(&verified, session_id, 4);
    assert!(failed.repository_vcs_observations.is_empty());
    assert!(failed.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::OutcomeResultInadmissible
    }));
    let prose = outcome_for_sequence(&verified, session_id, 6);
    assert!(prose.repository_vcs_observations.is_empty());
    assert!(prose.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::OutcomeResultInadmissible
    }));
    let mismatched = outcome_for_sequence(&verified, session_id, 8);
    assert!(mismatched.repository_vcs_observations.is_empty());
    assert!(mismatched
        .content
        .normalized_body
        .as_deref()
        .unwrap()
        .contains(oid));
}

#[test]
fn codex_structured_pr_create_scopes_exact_identity_without_local_route() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let control = temp.path().join("control");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir(&control).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000102";
    write_session(
        &sessions,
        native_session_id,
        &[
            exec_call("pr-call", "gh pr create", &control),
            successful_result(
                "pr-call",
                serde_json::json!({
                    "number": 42,
                    "url": "https://github.com/acme/codex-fixture/pull/42",
                    "id": "PR_42"
                }),
            ),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let core = outcome_for_sequence(&verified, session_id, 2);
    assert_eq!(
        core.repository_bindings[0].logical_repository_id,
        "forge:github.com/acme/codex-fixture"
    );
    assert!(core.repository_bindings[0]
        .local_root_authorization
        .is_none());
    assert_eq!(core.repository_vcs_observations.len(), 1);
    assert_eq!(
        core.repository_vcs_observations[0].repository_binding_id,
        core.repository_bindings[0].binding_id
    );
    assert!(!core.repository_abstentions.iter().any(|abstention| {
        abstention.reason == ctx_history_core::RepositoryAbstentionReason::OutcomeRepositoryUnbound
    }));
}

#[test]
fn codex_continuation_linkage_survives_checkpoint_resume() {
    use ctx_history_core::RepositoryVcsObservationKind;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let native_session_id = "019fa000-0000-7000-8000-000000000103";
    let oid = "2222222222222222222222222222222222222222";
    write_session(
        &sessions,
        native_session_id,
        &[
            exec_call(
                "origin-call",
                "git commit -m exact && git rev-parse HEAD",
                &repository,
            ),
            successful_result(
                "origin-call",
                Value::String("Script running with cell ID cell-7\n".to_owned()),
            ),
        ],
    );
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();

    let wait_call = serde_json::json!({
        "timestamp": "2026-07-28T12:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "wait",
            "call_id": "wait-call",
            "arguments": serde_json::json!({"cell_id": "cell-7"}).to_string()
        }
    })
    .to_string();
    let terminal = successful_result(
        "wait-call",
        Value::String(format!(
            "Script completed\nProcess exited with code 0\nFinal output:\n[main abc1234] exact\n{oid}\n"
        )),
    );
    OpenOptions::new()
        .append(true)
        .open(session_path(&sessions, native_session_id))
        .unwrap()
        .write_all(format!("{wait_call}\n{terminal}\n").as_bytes())
        .unwrap();
    let append = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(append.counters.appended_sources, 1);

    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let core = outcome_for_sequence(&verified, session_id, 4);
    let RepositoryVcsObservationKind::Outcome(outcome) = &core.repository_vcs_observations[0].kind
    else {
        panic!("expected resumed outcome");
    };
    assert_eq!(outcome.produced_object_ids[0].hex, oid);
    assert_eq!(outcome.linkage.origin_call_id, "origin-call");
    assert_eq!(outcome.linkage.result_call_id, "wait-call");
    assert_eq!(outcome.linkage.origin_event_sequence, 1);
    assert_eq!(outcome.linkage.continuation_call_id_sha256.len(), 1);
}

#[test]
fn codex_outcome_routes_are_operation_local_across_two_repositories() {
    use ctx_history_core::{RepositoryAbstentionReason, RepositoryVcsObservationKind};

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&first);
    initialize_repository(&second);
    let native_session_id = "019fa000-0000-7000-8000-000000000104";
    let oid = "3333333333333333333333333333333333333333";
    write_session(
        &sessions,
        native_session_id,
        &[
            exec_call(
                "route-mismatch",
                &format!(
                    "git -C {} commit -m exact && git rev-parse HEAD",
                    first.display()
                ),
                &second,
            ),
            successful_result("route-mismatch", Value::String(oid.to_owned())),
            exec_call(
                "route-match",
                &format!(
                    "git -C {} commit -m exact && git -C {} rev-parse HEAD",
                    first.display(),
                    first.display()
                ),
                &second,
            ),
            successful_result("route-match", Value::String(oid.to_owned())),
            exec_call(
                "route-cd",
                &format!(
                    "cd {} && git commit -m exact && git rev-parse HEAD",
                    first.display()
                ),
                &second,
            ),
            successful_result("route-cd", Value::String(oid.to_owned())),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let mismatch = outcome_for_sequence(&verified, session_id, 2);
    assert!(mismatch.repository_vcs_observations.is_empty());
    assert!(mismatch.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::ConflictingIdentity
    }));

    for sequence in [4, 6] {
        let core = outcome_for_sequence(&verified, session_id, sequence);
        let RepositoryVcsObservationKind::Outcome(_) = &core.repository_vcs_observations[0].kind
        else {
            panic!("expected repository outcome at {sequence}");
        };
        assert_eq!(
            core.repository_bindings[0]
                .local_root_authorization
                .as_ref()
                .unwrap()
                .local_root,
            first.to_string_lossy()
        );
    }
}

#[test]
fn codex_provider_identity_stays_complete_while_conflicting_route_abstains() {
    use ctx_history_core::RepositoryAbstentionReason;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let native_session_id = "019fa000-0000-7000-8000-000000000105";
    write_session(
        &sessions,
        native_session_id,
        &[
            exec_call("pr-conflict", "gh pr create", &repository),
            successful_result(
                "pr-conflict",
                Value::String("https://github.com/other/repository/pull/9".to_owned()),
            ),
        ],
    );
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let core = outcome_for_sequence(&verified, session_id, 2);
    assert_eq!(
        core.content.normalized_body.as_deref(),
        Some("https://github.com/other/repository/pull/9")
    );
    assert_eq!(
        core.content.structured_content.as_ref().unwrap()["provider_content"]
            ["provider_native_tool_result"]["result_content_location"],
        "normalized_body"
    );
    assert_eq!(core.repository_bindings.len(), 2);
    let provider_binding = core
        .repository_bindings
        .iter()
        .find(|binding| binding.logical_repository_id == "forge:github.com/other/repository")
        .unwrap();
    assert!(provider_binding.local_root_authorization.is_none());
    assert!(core.repository_bindings.iter().any(|binding| {
        binding.logical_repository_id == "forge:github.com/acme/codex-fixture"
            && binding.local_root_authorization.is_some()
    }));
    assert_eq!(core.repository_vcs_observations.len(), 1);
    assert_eq!(
        core.repository_vcs_observations[0].repository_binding_id,
        provider_binding.binding_id
    );
    assert!(!core.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::OutcomeRepositoryUnbound
    }));
    assert!(core.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::ConflictingIdentity
    }));
}

#[test]
fn codex_duplicate_and_reordered_linkage_abstains_without_positive_outcomes() {
    use ctx_history_core::RepositoryAbstentionReason;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let oid = "4444444444444444444444444444444444444444";

    let duplicate_session = "019fa000-0000-7000-8000-000000000106";
    let duplicate_call = exec_call(
        "duplicate-call",
        "git commit -m exact && git rev-parse HEAD",
        &repository,
    );
    write_session(
        &sessions,
        duplicate_session,
        &[
            duplicate_call.clone(),
            duplicate_call,
            successful_result("duplicate-call", Value::String(oid.to_owned())),
        ],
    );

    let reordered_session = "019fa000-0000-7000-8000-000000000107";
    write_session(
        &sessions,
        reordered_session,
        &[
            exec_call(
                "origin-call",
                "git commit -m exact && git rev-parse HEAD",
                &repository,
            ),
            running_result("origin-call", "cell-reorder"),
            wait_call("wait-a", "cell-reorder"),
            wait_call("wait-b", "cell-reorder"),
            successful_result(
                "wait-b",
                Value::String(format!("Script completed\nFinal output:\n{oid}\n")),
            ),
            successful_result(
                "wait-a",
                Value::String(format!("Script completed\nFinal output:\n{oid}\n")),
            ),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    for (native_session_id, sequences) in [
        (duplicate_session, vec![3]),
        (reordered_session, vec![5, 6]),
    ] {
        let source = codex_source_key(native_session_id).unwrap();
        let session_id = codex_session_identity(&source, native_session_id).unwrap();
        for sequence in sequences {
            let core = outcome_for_sequence(&verified, session_id, sequence);
            assert!(core.repository_vcs_observations.is_empty());
            assert!(core.repository_abstentions.iter().any(|abstention| {
                abstention.reason == RepositoryAbstentionReason::ProviderOutputUnjoined
            }));
        }
    }
}

#[test]
fn codex_pending_cache_evicts_by_raw_ordinal_without_rejoining_old_result_identity() {
    use ctx_history_core::RepositoryVcsObservationKind;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let native_session_id = "019fa000-0000-7000-8000-000000000108";
    let command = "git commit -m exact && git rev-parse HEAD";
    let mut records = vec![exec_call("z-oldest", command, &repository)];
    for index in 0..23 {
        records.push(exec_call(&format!("mid-{index:02}"), command, &repository));
    }
    records.push(exec_call("a-newest", command, &repository));
    let oid = "5555555555555555555555555555555555555555";
    records.push(successful_result("z-oldest", Value::String(oid.to_owned())));
    records.push(successful_result("a-newest", Value::String(oid.to_owned())));
    write_session(&sessions, native_session_id, &records);

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let oldest = outcome_for_sequence(&verified, session_id, 26);
    assert!(oldest.repository_vcs_observations.is_empty());
    assert_eq!(oldest.content.normalized_body.as_deref(), Some(oid));
    let newest = outcome_for_sequence(&verified, session_id, 27);
    let RepositoryVcsObservationKind::Outcome(outcome) =
        &newest.repository_vcs_observations[0].kind
    else {
        panic!("expected newest pending call to remain linked");
    };
    assert_eq!(outcome.linkage.origin_call_id, "a-newest");
}

#[test]
fn codex_continuation_overflow_survives_checkpoint_and_typed_abstains() {
    use ctx_history_core::RepositoryAbstentionReason;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let native_session_id = "019fa000-0000-7000-8000-000000000109";
    let oid = "6666666666666666666666666666666666666666";
    write_session(
        &sessions,
        native_session_id,
        &[
            exec_call(
                "overflow-origin",
                "git commit -m exact && git rev-parse HEAD",
                &repository,
            ),
            running_result("overflow-origin", "cell-overflow"),
        ],
    );
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();

    let mut intermediate = String::new();
    for index in 0..25 {
        intermediate.push_str(&wait_call(
            &format!("overflow-wait-{index:02}"),
            "cell-overflow",
        ));
        intermediate.push('\n');
        intermediate.push_str(&running_result(
            &format!("overflow-wait-{index:02}"),
            "cell-overflow",
        ));
        intermediate.push('\n');
    }
    OpenOptions::new()
        .append(true)
        .open(session_path(&sessions, native_session_id))
        .unwrap()
        .write_all(intermediate.as_bytes())
        .unwrap();
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();

    let final_wait = wait_call("overflow-final", "cell-overflow");
    let terminal = successful_result(
        "overflow-final",
        Value::String(format!("Script completed\nFinal output:\n{oid}\n")),
    );
    OpenOptions::new()
        .append(true)
        .open(session_path(&sessions, native_session_id))
        .unwrap()
        .write_all(format!("{final_wait}\n{terminal}\n").as_bytes())
        .unwrap();
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();

    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let core = outcome_for_sequence(&verified, session_id, 54);
    assert!(core.repository_vcs_observations.is_empty());
    assert!(core.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::LinkageCapacityExceeded
    }));
}

#[test]
fn codex_production_path_persists_complete_native_input_and_certified_binding() {
    use std::process::Command;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let control = temp.path().join("control");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(&control, b"not a directory\n").unwrap();
    fs::create_dir(&repository).unwrap();
    for arguments in [
        vec!["init", "-q"],
        vec!["config", "user.name", "ctx test"],
        vec!["config", "user.email", "ctx@example.invalid"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/acme/codex-fixture.git",
        ],
    ] {
        assert!(Command::new("/usr/bin/git")
            .arg("-C")
            .arg(&repository)
            .args(arguments)
            .status()
            .unwrap()
            .success());
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
    for arguments in [vec!["add", "tracked.txt"], vec!["commit", "-qm", "fixture"]] {
        assert!(Command::new("/usr/bin/git")
            .arg("-C")
            .arg(&repository)
            .args(arguments)
            .status()
            .unwrap()
            .success());
    }
    let native_session_id = "019fa000-0000-7000-8000-000000000099";
    let secret = "CTX_SECRET_TOKEN_7f3a9d";
    let arguments = serde_json::json!({
        "cmd": format!("SECRET_TOKEN={secret} git status"),
        "workdir": repository,
        "yield_time_ms": 10000,
    });
    let records = [
        serde_json::json!({
            "timestamp": "2026-07-28T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": native_session_id,
                "timestamp": "2026-07-28T12:00:00Z",
                "cwd": control,
                "originator": "codex_cli_rs",
                "cli_version": "0.1.0",
                "source": "cli",
                "model_provider": "openai"
            }
        })
        .to_string(),
        serde_json::json!({
            "timestamp": "2026-07-28T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "exec_command",
                "call_id": "call-repository",
                "arguments": arguments.to_string()
            }
        })
        .to_string(),
    ];
    fs::write(
        session_path(&sessions, native_session_id),
        format!("{}\n", records.join("\n")),
    )
    .unwrap();

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let event = verified
        .events_for_session(session_id.as_uuid())
        .unwrap()
        .remove(0);
    let core = verified
        .core_record_by_id(event.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        core.repository_candidate_evidence
            .paths(ctx_history_core::RepositoryCandidateKind::SessionCwd)
            .collect::<Vec<_>>(),
        vec![control.to_string_lossy().as_ref()]
    );
    assert_eq!(
        core.repository_candidate_evidence
            .paths(ctx_history_core::RepositoryCandidateKind::DeclaredToolWorkdir)
            .collect::<Vec<_>>(),
        vec![repository.to_string_lossy().as_ref()]
    );
    assert_eq!(core.repository_bindings.len(), 1);
    assert_eq!(
        core.repository_bindings[0].logical_repository_id,
        "forge:github.com/acme/codex-fixture"
    );
    let structured = core.content.structured_content.as_ref().unwrap();
    assert_eq!(
        structured["provider_content"]["provider_native_tool_call"]["arguments"]["cmd"],
        arguments["cmd"]
    );
    assert_eq!(
        structured["provider_native_tool_activities"][0]["provider_native_tool"]
            ["raw_arguments_retained"],
        false
    );
    assert_eq!(
        structured["provider_native_tool_activities"][0]["provider_native_tool"]["argument_schema"],
        "codex_exec_command_args_v1"
    );
    let encoded = core.encode_stored().unwrap();
    assert!(encoded
        .windows(secret.len())
        .any(|window| window == secret.as_bytes()));
    assert!(encoded
        .windows(arguments.to_string().len())
        .any(|window| window == arguments.to_string().as_bytes()));
    assert!(core.repository_vcs_observations.is_empty());
}

#[test]
fn multi_source_cold_generation_is_deterministic() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let first_index = temp.path().join("first-index");
    let second_index = temp.path().join("second-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);

    let native_session_ids = (0..32)
        .map(|index| format!("019fa000-0000-7000-8000-{index:012}"))
        .collect::<Vec<_>>();
    for (index, native_session_id) in native_session_ids.iter().enumerate() {
        write_session(
            &sessions,
            native_session_id,
            &[exec_call(
                &format!("status-{index}"),
                "git status",
                &repository,
            )],
        );
    }

    let first = ingest_codex_source_backed_v0(&sessions, &first_index).unwrap();
    assert!(first.counters.scanner_workers >= 1);

    let second = ingest_codex_source_backed_v0(&sessions, &second_index).unwrap();
    assert_eq!(first.commit.generation_id, second.commit.generation_id);

    let first_verified = VerifiedIndex::open(&first_index).unwrap();
    let second_verified = VerifiedIndex::open(&second_index).unwrap();
    for native_session_id in native_session_ids {
        let source = codex_source_key(&native_session_id).unwrap();
        let session_id = codex_session_identity(&source, &native_session_id).unwrap();
        let first_event = first_verified
            .events_for_session(session_id.as_uuid())
            .unwrap()
            .remove(0);
        let second_event = second_verified
            .events_for_session(session_id.as_uuid())
            .unwrap()
            .remove(0);
        let first_core = first_verified
            .core_record_by_id(first_event.event_id.as_uuid())
            .unwrap()
            .unwrap();
        let second_core = second_verified
            .core_record_by_id(second_event.event_id.as_uuid())
            .unwrap()
            .unwrap();
        assert_eq!(
            first_core.encode_stored().unwrap(),
            second_core.encode_stored().unwrap()
        );
        assert_eq!(
            first_core.repository_bindings[0]
                .local_root_authorization
                .as_ref()
                .unwrap()
                .observed_at_unix_ms,
            1_785_240_001_000
        );
    }
}

#[test]
fn source_backed_projection_preserves_semantics_without_legacy_operations() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000002";
    let session_path = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    let long_message = "long-message-sentinel complete-message-tail".to_owned();
    let tool_record = tool_call_with_patch("touch-call");
    let failed_record = failed_tool_output("touch-call");
    fs::write(
        &session_path,
        format!(
            "{}\n{}\n{tool_record}\n{failed_record}\n",
            session_meta(native_session_id),
            message("assistant", &long_message)
        ),
    )
    .unwrap();

    let receipt = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_no_legacy_operations(receipt.counters);
    assert_eq!(receipt.counters.complete_records_scanned, 4);
    assert_eq!(receipt.counters.retained_records_scanned, 3);
    assert_eq!(receipt.counters.staged_documents, 3);
    assert_eq!(receipt.counters.structural_json_parses, 4);
    assert_eq!(receipt.counters.typed_json_parses, 4);

    let source_key = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source_key, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let events = verified.events_for_session(session_id.as_uuid()).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(events[0].event_type, EventType::Message.as_str());
    assert_eq!(events[1].event_type, EventType::ToolCall.as_str());
    assert_eq!(events[2].event_type, EventType::ToolOutput.as_str());
    assert_eq!(events[2].role.as_deref(), Some("tool"));
    assert!(verified
        .search_event_candidates("long message sentinel", 10)
        .unwrap()
        .iter()
        .any(|candidate| candidate.event.event_id == events[0].event_id));
    let stored = verified
        .core_record_by_id(events[0].event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.content.normalized_body.as_deref(),
        Some(long_message.as_str())
    );
}

#[test]
fn source_backed_scanner_keeps_full_message_tail_and_exact_display_text() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000022";
    let full_text = format!("codex-full-{}-codex-tail-sentinel", "m".repeat(16_512));
    write_session(
        &sessions,
        native_session_id,
        &[message("assistant", &full_text)],
    );

    let catalog_source = discover_one(
        &session_path(&sessions, native_session_id),
        native_session_id,
    );
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let mut scanner =
        CodexNativeScanner::new_source_backed_v0(catalog_source.clone(), None).unwrap();
    let mut records = Vec::new();
    let mut repository_attributor = crate::repository_attribution::RepositoryAttributor::default();
    let mut event_identity_state = CodexEventIdentityStateV0::default();
    let outcome_lineage = CodexOutcomeLineageAuthorityV0::unscoped();
    while let Some(page) = scanner.next_page().unwrap() {
        let CodexNativeOwnedPage::Core(page) = page;
        let owner = page.owner.unwrap();
        for row in page.source_backed_rows {
            records.push(
                codex_core_record(
                    &source,
                    session_id,
                    &owner,
                    row,
                    &mut event_identity_state,
                    &mut repository_attributor,
                    &outcome_lineage,
                )
                .unwrap(),
            );
        }
    }
    let scan = scanner.finish().unwrap();
    assert!(scan.source.opened.is_some());
    let evidence = CodexTerminalSourceEvidenceV0::new(
        scan.source,
        scan.after_observation,
        scan.before_observation.len,
        scan.full_revision_sha256,
    );
    assert!(evidence.source.opened.is_none());
    assert!(evidence.revalidate());

    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].content.normalized_body.as_deref(),
        Some(full_text.as_str())
    );
    assert!(records[0]
        .content
        .normalized_body
        .as_deref()
        .unwrap()
        .ends_with("codex-tail-sentinel"));
}

#[test]
fn codex_large_tool_arguments_keep_both_complete_core_representations() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000024";
    let tail = "codex-large-tool-argument-tail";
    let full_argument = format!("{}{tail}", "x".repeat(8 * 1024 * 1024));
    let tool_call = serde_json::json!({
        "timestamp": "2026-07-28T12:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "custom_complete_tool",
            "call_id": "large-complete-call",
            "arguments": serde_json::json!({"prompt": &full_argument}).to_string(),
        }
    })
    .to_string();
    assert!(tool_call.len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);
    write_session(&sessions, native_session_id, &[tool_call]);

    let catalog_source = discover_one(
        &session_path(&sessions, native_session_id),
        native_session_id,
    );
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let mut scanner = CodexNativeScanner::new_source_backed_v0(catalog_source, None).unwrap();
    let mut records = Vec::new();
    let mut repository_attributor = crate::repository_attribution::RepositoryAttributor::default();
    let mut event_identity_state = CodexEventIdentityStateV0::default();
    let outcome_lineage = CodexOutcomeLineageAuthorityV0::unscoped();
    while let Some(page) = scanner.next_page().unwrap() {
        let CodexNativeOwnedPage::Core(page) = page;
        let owner = page.owner.unwrap();
        for row in page.source_backed_rows {
            records.push(
                codex_core_record(
                    &source,
                    session_id,
                    &owner,
                    row,
                    &mut event_identity_state,
                    &mut repository_attributor,
                    &outcome_lineage,
                )
                .unwrap(),
            );
        }
    }
    scanner.finish().unwrap();

    let [record] = records.as_slice() else {
        panic!("expected exactly one Codex tool-call record");
    };
    let normalized = record.content.normalized_body.as_deref().unwrap();
    let structured = record.content.structured_content.as_ref().unwrap();
    assert!(normalized.contains(tail));
    assert_eq!(
        structured["provider_native_tool_call"]["arguments"]["prompt"].as_str(),
        Some(full_argument.as_str())
    );
    assert!(
        normalized.len() + serde_json::to_vec(structured).unwrap().len()
            > ctx_history_core::MAX_CORE_CONTENT_BYTES
    );
    record.validate_contract().unwrap();
    record.encode_stored().unwrap();
}

#[test]
fn indexed_core_keeps_over_16k_message_tool_arguments_and_successful_result() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000023";
    let message_tail = "message_tail_complete_contract";
    let argument_tail = "tool_argument_tail_complete_contract";
    let result_tail = "tool_result_tail_complete_contract";
    let full_message = format!("{} {message_tail}", "message body ".repeat(1_400));
    let full_argument = format!("{} {argument_tail}", "tool argument ".repeat(1_400));
    let full_result = format!("{} {result_tail}", "successful result ".repeat(1_200));
    assert!(full_message.len() > 16_000);
    assert!(full_argument.len() > 16_000);
    assert!(full_result.len() > 16_000);
    let tool_call = serde_json::json!({
        "timestamp": "2026-07-28T12:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "custom_complete_tool",
            "call_id": "complete-call",
            "arguments": serde_json::json!({"prompt": full_argument}).to_string(),
        }
    })
    .to_string();
    write_session(
        &sessions,
        native_session_id,
        &[
            message("assistant", &full_message),
            tool_call,
            successful_result("complete-call", Value::String(full_result.clone())),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let events = verified.events_for_session(session_id.as_uuid()).unwrap();
    assert_eq!(events.len(), 3);
    for (query, event) in [message_tail, argument_tail, result_tail]
        .into_iter()
        .zip(events.iter())
    {
        assert!(verified
            .search_event_candidates(query, 10)
            .unwrap()
            .iter()
            .any(|candidate| candidate.event.event_id == event.event_id));
    }

    let message_core = outcome_for_sequence(&verified, session_id, 1);
    assert_eq!(
        message_core.content.normalized_body.as_deref(),
        Some(full_message.as_str())
    );
    let call_core = outcome_for_sequence(&verified, session_id, 2);
    assert!(call_core
        .content
        .normalized_body
        .as_deref()
        .unwrap()
        .contains(argument_tail));
    assert_eq!(
        call_core.content.structured_content.as_ref().unwrap()["provider_native_tool_call"]
            ["arguments"]["prompt"],
        full_argument
    );
    let result_core = outcome_for_sequence(&verified, session_id, 3);
    assert_eq!(
        result_core.content.normalized_body.as_deref(),
        Some(full_result.as_str())
    );
    let structured_result = result_core.content.structured_content.as_ref().unwrap();
    assert_eq!(
        structured_result["provider_native_tool_result"]["result_content_location"],
        "normalized_body"
    );
    assert!(!serde_json::to_string(structured_result)
        .unwrap()
        .contains(result_tail));
}
