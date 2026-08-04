use super::*;
use ctx_history_core::RepositoryAbstentionReason;
use serde_json::Value;

use super::projection::{
    exec_call, initialize_repository, outcome_for_sequence, successful_result,
};

fn assert_child_outcome_is_unproven(index: &VerifiedIndex, child_native_session_id: &str) {
    let source = codex_source_key(child_native_session_id).unwrap();
    let session = codex_session_identity(&source, child_native_session_id).unwrap();
    let result = outcome_for_sequence(index, session, 2);
    assert!(result.repository_vcs_observations.is_empty());
    assert!(result.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::ProviderOutputUnjoined
            && abstention.detail.as_deref() == Some("provider_execution_origin_lineage_unproven")
    }));
}

#[test]
fn checkpoint_replay_preserves_incomplete_tail_lineage_ambiguity() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let parent = "019fa000-0000-7000-8000-000000000200";
    let child = "019fa000-0000-7000-8000-000000000201";
    let incomplete =
        r#"{"type":"response_item","payload":{"type":"function_call","call_id":"unterminated"#;
    fs::write(
        session_path(&sessions, parent),
        format!("{}\n{incomplete}", session_meta(parent)),
    )
    .unwrap();

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    write_forked_session(
        &sessions,
        child,
        parent,
        &[
            exec_call(
                "child-after-incomplete-tail",
                "git commit -m child && git rev-parse --verify HEAD",
                &repository,
            ),
            successful_result(
                "child-after-incomplete-tail",
                Value::String(
                    "[main ccccccc] child\ncccccccccccccccccccccccccccccccccccccccc\n".to_owned(),
                ),
            ),
        ],
    );

    let refresh = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(refresh.counters.replayed_sources, 1);
    assert_child_outcome_is_unproven(&VerifiedIndex::open(&index).unwrap(), child);
}

#[test]
fn fully_escaped_duplicate_lineage_fields_cannot_publish_a_unique_child_outcome() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let parent = "019fa000-0000-7000-8000-000000000202";
    let child = "019fa000-0000-7000-8000-000000000203";
    let malformed = r#"{"\u0074\u0079\u0070\u0065":"\u0072\u0065\u0073\u0070\u006f\u006e\u0073\u0065\u005f\u0069\u0074\u0065\u006d","\u0070\u0061\u0079\u006c\u006f\u0061\u0064":{"\u0074\u0079\u0070\u0065":"\u0066\u0075\u006e\u0063\u0074\u0069\u006f\u006e\u005f\u0063\u0061\u006c\u006c","\u0063\u0061\u006c\u006c\u005f\u0069\u0064":"first","\u0063\u0061\u006c\u006c\u005f\u0069\u0064":"second"}}"#;
    write_session(&sessions, parent, &[malformed.to_owned()]);
    write_forked_session(
        &sessions,
        child,
        parent,
        &[
            exec_call(
                "child-after-escaped-duplicates",
                "git commit -m child && git rev-parse --verify HEAD",
                &repository,
            ),
            successful_result(
                "child-after-escaped-duplicates",
                Value::String(
                    "[main ddddddd] child\ndddddddddddddddddddddddddddddddddddddddd\n".to_owned(),
                ),
            ),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let verified = VerifiedIndex::open(&index).unwrap();
    let parent_source = codex_source_key(parent).unwrap();
    let parent_certificate = verified
        .manifest()
        .sources
        .iter()
        .find(|certificate| {
            certificate
                .observation()
                .source()
                .exact_descriptor_eq(&parent_source)
        })
        .unwrap();
    assert_eq!(parent_certificate.counts().rejected_records, 1);
    assert_child_outcome_is_unproven(&verified, child);
}

#[test]
fn fully_escaped_duplicate_call_id_after_non_string_cannot_publish_a_unique_child_outcome() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let parent = "019fa000-0000-7000-8000-000000000205";
    let child = "019fa000-0000-7000-8000-000000000206";
    let call_id = "escaped-after-non-string";
    let malformed = format!(
        r#"{{"\u0074\u0079\u0070\u0065":"\u0072\u0065\u0073\u0070\u006f\u006e\u0073\u0065\u005f\u0069\u0074\u0065\u006d","\u0070\u0061\u0079\u006c\u006f\u0061\u0064":{{"\u0074\u0079\u0070\u0065":"\u0066\u0075\u006e\u0063\u0074\u0069\u006f\u006e\u005f\u0063\u0061\u006c\u006c","\u0063\u0061\u006c\u006c\u005f\u0069\u0064":7,"\u0063\u0061\u006c\u006c\u005f\u0069\u0064":"{call_id}"}}}}"#
    );
    write_session(&sessions, parent, &[malformed]);
    write_forked_session(
        &sessions,
        child,
        parent,
        &[
            exec_call(
                call_id,
                "git commit -m child && git rev-parse --verify HEAD",
                &repository,
            ),
            successful_result(
                call_id,
                Value::String(
                    "[main eeeeeee] child\neeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\n".to_owned(),
                ),
            ),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_child_outcome_is_unproven(&VerifiedIndex::open(&index).unwrap(), child);
}

#[test]
fn completed_parent_checkpoint_tail_rebuilds_child_and_replays_without_stale_ambiguity() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    let repository = temp.path().join("repo");
    fs::create_dir_all(&sessions).unwrap();
    initialize_repository(&repository);
    let parent = "019fa000-0000-7000-8000-000000000207";
    let child = "019fa000-0000-7000-8000-000000000208";
    let call_id = "unique-after-tail-completion";
    let incomplete = r#"{"type":"event_msg","payload":{"type":"token_count""#;
    let parent_path = session_path(&sessions, parent);
    fs::write(
        &parent_path,
        format!("{}\n{incomplete}", session_meta(parent)),
    )
    .unwrap();
    write_forked_session(
        &sessions,
        child,
        parent,
        &[
            exec_call(
                call_id,
                "git commit -m child && git rev-parse --verify HEAD",
                &repository,
            ),
            successful_result(
                call_id,
                Value::String(
                    "[main fffffff] child\nffffffffffffffffffffffffffffffffffffffff\n".to_owned(),
                ),
            ),
        ],
    );

    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_child_outcome_is_unproven(&VerifiedIndex::open(&index).unwrap(), child);

    use std::io::Write;
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(&parent_path)
            .unwrap(),
        "}}}}"
    )
    .unwrap();
    let completed = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(completed.counters.appended_sources, 1);
    assert_eq!(completed.counters.replaced_sources, 1);
    let verified = VerifiedIndex::open(&index).unwrap();
    let child_source = codex_source_key(child).unwrap();
    let child_session = codex_session_identity(&child_source, child).unwrap();
    let result = outcome_for_sequence(&verified, child_session, 2);
    assert_eq!(result.repository_vcs_observations.len(), 1);
    assert!(!result.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::ProviderOutputUnjoined
            && abstention.detail.as_deref() == Some("provider_execution_origin_lineage_unproven")
    }));

    let replay = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(replay.counters.replayed_sources, 2);
    assert_eq!(replay.commit.generation_id, completed.commit.generation_id);
    let replayed = VerifiedIndex::open(&index).unwrap();
    assert_eq!(
        outcome_for_sequence(&replayed, child_session, 2)
            .repository_vcs_observations
            .len(),
        1
    );
}
