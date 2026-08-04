use super::*;

#[test]
fn codex_commit_receipt_with_trailing_command_publishes_certified_outcome() {
    use std::process::Command;

    use ctx_history_core::{RepositoryOutcomeKind, RepositoryVcsObservationKind};

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    fs::write(repository.join("tracked.txt"), "changed\n").unwrap();
    for arguments in [
        vec!["add", "tracked.txt"],
        vec![
            "commit",
            "-qm",
            "fix(pro): reserve result bytes before source admission",
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
    let oid = String::from_utf8(
        Command::new("/usr/bin/git")
            .arg("-C")
            .arg(&repository)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let oid = oid.trim();
    let short = &oid[..9];
    let native_session_id = "019fa000-0000-7000-8000-000000000110";
    let command = concat!(
        "git commit -m 'fix(pro): reserve result bytes before source admission' && ",
        "git status --short && git rev-parse HEAD && sed -n '1,2p' tracked.txt"
    );
    write_session(
        &sessions,
        native_session_id,
        &[
            exec_call("commit-with-tail", command, &repository),
            successful_result(
                "commit-with-tail",
                Value::String(format!(
                    "[main {short}] fix(pro): reserve result bytes before source admission\n 1 file changed, 1 insertion(+), 1 deletion(-)\n{oid}\nchanged\n"
                )),
            ),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(native_session_id).unwrap();
    let session_id = codex_session_identity(&source, native_session_id).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let core = outcome_for_sequence(&verified, session_id, 2);
    assert!(
        !core.repository_vcs_observations.is_empty(),
        "abstentions: {:?}",
        core.repository_abstentions
    );
    let outcome = core
        .repository_vcs_observations
        .iter()
        .find_map(|observation| match &observation.kind {
            RepositoryVcsObservationKind::Outcome(outcome) => Some(outcome),
            _ => None,
        })
        .expect("expected repository outcome");
    assert_eq!(outcome.kind, RepositoryOutcomeKind::Commit);
    assert_eq!(outcome.produced_object_ids[0].hex, oid);
    assert_eq!(outcome.linkage.origin_call_id, "commit-with-tail");
}

#[test]
fn codex_post_fork_execution_survives_an_unavailable_older_ancestor() {
    use ctx_history_core::RepositoryVcsObservationKind;

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let missing_root = "019fa000-0000-7000-8000-000000000198";
    let parent = "019fa000-0000-7000-8000-000000000199";
    let child = "019fa000-0000-7000-8000-000000000200";
    let call_id = "call-post-fork-child";
    let oid = "cccccccccccccccccccccccccccccccccccccccc";
    write_forked_session_at(&sessions, parent, missing_root, "2026-07-28T12:00:00Z", &[]);
    write_forked_session_at(
        &sessions,
        child,
        parent,
        "2026-07-28T12:30:00Z",
        &[
            exec_call_at(
                "2026-07-28T12:31:00Z",
                call_id,
                "git commit -m child && git rev-parse HEAD",
                &repository,
            ),
            successful_result_at(
                "2026-07-28T12:31:01Z",
                call_id,
                Value::String(format!("[main ccccccc] child\n{oid}\n")),
            ),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let source = codex_source_key(child).unwrap();
    let session = codex_session_identity(&source, child).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let result = outcome_for_sequence(&verified, session, 2);
    let RepositoryVcsObservationKind::Outcome(outcome) =
        &result.repository_vcs_observations[0].kind
    else {
        panic!("expected exact post-fork commit outcome");
    };
    assert_eq!(outcome.produced_object_ids[0].hex, oid);
    assert_eq!(outcome.linkage.origin_call_id, call_id);
    assert!(result.repository_abstentions.is_empty());
}
