use ctx_history_core::{
    RepositoryAbstentionReason, RepositoryCommitOperationKind, RepositoryOutcomeKind,
    RepositoryOutcomeObservation, MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS,
};
use serde_json::Value;

use super::super::{
    linked_outcome_evidence, BoundedCommitProducer, LinkedOutcomeInput, UnscopedOutcomeObservation,
};
use crate::OutputOutcome;

fn exact_outcome(value: &UnscopedOutcomeObservation) -> &RepositoryOutcomeObservation {
    match value {
        UnscopedOutcomeObservation::Exact(outcome) => outcome,
        UnscopedOutcomeObservation::DeferredCommit(_)
        | UnscopedOutcomeObservation::DeferredCommitOperation(_)
        | UnscopedOutcomeObservation::DeferredCherryPick(_) => {
            panic!("expected exact outcome")
        }
    }
}

fn input<'a>(command: &'a str, output: &'a Value) -> LinkedOutcomeInput<'a> {
    LinkedOutcomeInput {
        provider: "fixture",
        command,
        session_cwd: Some("/repo"),
        declared_workdir: Some("/repo"),
        origin_call_id: "call-origin",
        result_call_id: "call-result",
        origin_event_sequence: 7,
        continuation_call_id_sha256: &[],
        result_record_sha256: [9; 32],
        observed_at_unix_ms: 10,
        result_outcome: OutputOutcome::Success,
        result_output: output,
        structured_commit_oid: None,
        output_repository_path: Some("/repo"),
    }
}

fn full_sha1(index: usize) -> String {
    format!("{index:040x}")
}

#[test]
fn exact_result_and_structured_oid_precedence_are_fail_closed() {
    let oid = "0123456789abcdef0123456789abcdef01234567";
    let output = Value::String(oid.to_owned());
    let exact = linked_outcome_evidence(input(
        "git commit -m exact && git rev-parse --verify HEAD",
        &output,
    ))
    .unwrap();
    assert_eq!(
        exact_outcome(&exact.outcomes[0]).produced_object_ids[0].hex,
        oid
    );
    assert_eq!(
        serde_json::to_value(&exact.outcomes[0]).unwrap(),
        serde_json::json!({"Exact": exact_outcome(&exact.outcomes[0])})
    );

    let mut short = input("git commit -m exact", &output);
    short.structured_commit_oid = Some("0123456");
    let short = linked_outcome_evidence(short).unwrap();
    assert!(short.outcomes.is_empty());
    assert_eq!(
        short.abstentions[0].0,
        RepositoryAbstentionReason::OutcomeResultInadmissible
    );
}

#[test]
fn canonical_cross_provider_short_commit_results_are_deferred_not_guessed() {
    for (command, output, expected_prefix, expected_subject) in [
        (
            "git commit -m exact",
            serde_json::json!([
                {"type": "input_text", "text": "Script completed\nOutput:\n"},
                {"type": "input_text", "text": "[main 9747be9] Fail closed on invalid retry headers\n 2 files changed, 24 insertions(+), 5 deletions(-)\n"},
                {"type": "input_text", "text": "exit=0"}
            ]),
            "9747be9",
            "Fail closed on invalid retry headers",
        ),
        (
            "git commit -m exact",
            serde_json::json!({
                "status": "completed",
                "exitCode": 0,
                "aggregated": "## main\n M src/audit.js\n[main ee42c90] feat: summarize normalized delivery policies\n 2 files changed, 68 insertions(+), 1 deletion(-)",
                "cwd": "/repo"
            }),
            "ee42c90",
            "feat: summarize normalized delivery policies",
        ),
    ] {
        let evidence = linked_outcome_evidence(input(command, &output)).unwrap();
        let UnscopedOutcomeObservation::DeferredCommit(deferred) = &evidence.outcomes[0] else {
            panic!("expected deferred commit");
        };
        assert_eq!(deferred.oid_prefix, expected_prefix);
        assert_eq!(deferred.subject, expected_subject);
    }

    let amend = Value::String(
        "[main 1791cb3] Add bounded retry jitter normalization\n 2 files changed".to_owned(),
    );
    let evidence = linked_outcome_evidence(input("git commit --amend --no-edit", &amend)).unwrap();
    assert!(evidence.outcomes.is_empty());
    assert_eq!(
        evidence.abstentions[0].0,
        RepositoryAbstentionReason::HistoryRewriteUnlinked
    );
}

#[test]
fn exact_commit_receipt_survives_unrelated_output_after_exact_head() {
    let oid = "cbbccc92da81bbe173789b873b2e579327b7c2e1";
    let output = Value::String(format!(
        "[ctx/v026-locator-sidecar-envelope-backfill cbbccc92d] fix(pro): reserve result bytes before source admission\n 2 files changed, 24 insertions(+), 5 deletions(-)\n{oid}\npub const MAX_PAGE_BYTES: usize = 64 * 1024 * 1024;\n"
    ));
    let command = concat!(
        "git commit -m 'fix(pro): reserve result bytes before source admission' && ",
        "git status --short && git rev-parse HEAD && ",
        "sed -n '12,18p' crates/ctx-pro-host-protocol/src/lib.rs"
    );
    let evidence = linked_outcome_evidence(input(command, &output)).unwrap();
    let UnscopedOutcomeObservation::DeferredCommit(deferred) = &evidence.outcomes[0] else {
        panic!("expected certified-receipt candidate");
    };
    assert_eq!(deferred.oid_prefix, "cbbccc92d");
    assert_eq!(
        deferred.subject,
        "fix(pro): reserve result bytes before source admission"
    );

    let ambiguous = Value::String(format!(
        "[main cbbccc92d] fix(pro): reserve result bytes before source admission\n{oid}\n[main 1111111] unrelated second commit\n"
    ));
    let evidence = linked_outcome_evidence(input(command, &ambiguous)).unwrap();
    assert!(evidence.outcomes.is_empty());
    assert_eq!(
        evidence.abstentions[0].0,
        RepositoryAbstentionReason::OutcomeResultInadmissible
    );
}

#[test]
fn canonical_merge_graph_head_is_deferred_and_ambiguous_summaries_abstain() {
    let output = Value::String(
        "Merge made by the 'ort' strategy.\n README.md | 11 +++++++++++\n*   efdfa9e Merge retry validation documentation\n|\\  \n| * a69f7ff Document retry validation contract\n* | 9747be9 Fail closed on invalid retry headers\n"
            .to_owned(),
    );
    let evidence = linked_outcome_evidence(input("git merge --no-ff docs", &output)).unwrap();
    let UnscopedOutcomeObservation::DeferredCommit(deferred) = &evidence.outcomes[0] else {
        panic!("expected deferred merge");
    };
    assert_eq!(deferred.oid_prefix, "efdfa9e");
    assert_eq!(deferred.producer, BoundedCommitProducer::Merge);

    let ambiguous = Value::String("[main 1111111] first\n[main 2222222] second\n".to_owned());
    let evidence = linked_outcome_evidence(input("git commit -m exact", &ambiguous)).unwrap();
    assert!(evidence.outcomes.is_empty());
    assert_eq!(
        evidence.abstentions[0].0,
        RepositoryAbstentionReason::OutcomeResultInadmissible
    );
}

#[test]
fn dry_run_and_intervening_head_changes_never_produce_exact_outcomes() {
    let oid = "0123456789abcdef0123456789abcdef01234567";
    let output = Value::String(oid.to_owned());
    for command in [
        "git commit --dry-run && git rev-parse HEAD",
        "git commit -m exact && git reset --hard HEAD^ && git rev-parse HEAD",
        "git commit -m exact && git checkout other && git rev-parse HEAD",
        "git commit -m exact && custom-command && git rev-parse HEAD",
    ] {
        let evidence = linked_outcome_evidence(input(command, &output)).unwrap();
        assert!(evidence.outcomes.is_empty(), "{command}");
        assert!(!evidence.abstentions.is_empty(), "{command}");
    }

    let stable = linked_outcome_evidence(input(
        "git commit -m exact && git status --short && git rev-parse HEAD",
        &output,
    ))
    .unwrap();
    assert_eq!(stable.outcomes.len(), 1);
    assert_eq!(
        exact_outcome(&stable.outcomes[0]).produced_object_ids[0].hex,
        oid
    );
}

#[test]
fn exact_oids_from_inspection_commands_are_not_production_outcomes() {
    let oid = "d50d84a3e609d1ed30a435adbf2c19db35448b52";
    let output = Value::String(format!("{oid}\n"));

    for command in [
        format!("git show --no-patch --format=%H {oid}"),
        format!("git log -1 --format=%H {oid}"),
        format!("git rev-parse --verify {oid}^{{commit}}"),
        format!("git branch --contains {oid}"),
    ] {
        let evidence = linked_outcome_evidence(input(&command, &output));
        assert!(
            evidence
                .as_ref()
                .is_none_or(|evidence| evidence.outcomes.is_empty()),
            "inspection command emitted a production outcome: {command}"
        );
    }
}

#[test]
fn merge_head_is_exact_only_when_the_output_demonstrates_merge_creation() {
    let oid = "0123456789abcdef0123456789abcdef01234567";
    let command = "git merge --no-ff feature && git rev-parse --verify HEAD";
    let no_op = Value::String(format!("Already up to date.\n{oid}\n"));
    let no_op = linked_outcome_evidence(input(command, &no_op)).unwrap();
    assert!(no_op.outcomes.is_empty());
    assert_eq!(
        no_op.abstentions[0].0,
        RepositoryAbstentionReason::OutcomeResultInadmissible
    );

    let created_output = Value::String(format!("Merge made by the 'ort' strategy.\n{oid}\n"));
    let created = linked_outcome_evidence(input(command, &created_output)).unwrap();
    assert_eq!(created.outcomes.len(), 1);
    assert_eq!(
        exact_outcome(&created.outcomes[0]).produced_object_ids[0].hex,
        oid
    );

    let polluted = linked_outcome_evidence(input(
        "git log -1 && git merge --no-ff feature && git rev-parse HEAD",
        &created_output,
    ))
    .unwrap();
    assert!(polluted.outcomes.is_empty());

    let intervening = linked_outcome_evidence(input(
        "git merge --no-ff feature && git status --short && git rev-parse HEAD",
        &created_output,
    ))
    .unwrap();
    assert!(intervening.outcomes.is_empty());

    for non_producing in [
        "git merge --no-ff --no-commit feature && git rev-parse HEAD",
        "git merge --no-ff --squash feature && git rev-parse HEAD",
    ] {
        let evidence = linked_outcome_evidence(input(non_producing, &created_output)).unwrap();
        assert!(evidence.outcomes.is_empty(), "{non_producing}");
    }
}

#[test]
fn exact_rewrite_and_pull_request_schemas_are_supported() {
    let old = "1111111111111111111111111111111111111111";
    let new = "2222222222222222222222222222222222222222";
    let rewrite_output = serde_json::json!({"old_oid": old, "new_oid": new});
    let rewrite =
        linked_outcome_evidence(input("git commit --amend --no-edit", &rewrite_output)).unwrap();
    let UnscopedOutcomeObservation::DeferredCommitOperation(amend) = &rewrite.outcomes[0] else {
        panic!("expected deferred amend operation");
    };
    assert_eq!(amend.kind, RepositoryCommitOperationKind::Amend);
    assert_eq!(amend.mappings.len(), 1);
    assert_eq!(amend.command_pre_head.as_ref().unwrap().hex, old);
    assert_eq!(amend.command_post_head.hex, new);

    let rebase = linked_outcome_evidence(input("git rebase main", &rewrite_output)).unwrap();
    let UnscopedOutcomeObservation::DeferredCommitOperation(rebase) = &rebase.outcomes[0] else {
        panic!("expected deferred rebase operation");
    };
    assert_eq!(rebase.kind, RepositoryCommitOperationKind::Rebase);
    assert_eq!(rebase.mappings.len(), 1);
    assert_eq!(rebase.sequencer_pre_head.as_ref().unwrap().hex, old);

    let raw_rebase_oid = Value::String(new.to_owned());
    let raw_rebase = linked_outcome_evidence(input(
        "git rebase main && git rev-parse --verify HEAD",
        &raw_rebase_oid,
    ))
    .unwrap();
    assert!(raw_rebase.outcomes.is_empty());
    assert_eq!(
        raw_rebase.abstentions[0].0,
        RepositoryAbstentionReason::HistoryRewriteUnlinked
    );

    let amended = linked_outcome_evidence(input(
        "git commit --amend --no-edit && git rev-parse HEAD",
        &Value::String(new.to_owned()),
    ))
    .unwrap();
    let amended_outcome = exact_outcome(&amended.outcomes[0]);
    assert!(amended_outcome.produced_object_ids.is_empty());
    let operation = amended_outcome.commit_operation.as_ref().unwrap();
    assert_eq!(operation.kind, RepositoryCommitOperationKind::Amend);
    assert!(operation.mappings.is_empty());
    assert_eq!(operation.unlinked_results[0].hex, new);
    assert_eq!(
        amended.abstentions[0].0,
        RepositoryAbstentionReason::HistoryRewriteUnlinked
    );

    let create = Value::String("https://github.com/acme/repo/pull/42".to_owned());
    let created = linked_outcome_evidence(input("gh pr create --repo acme/repo", &create)).unwrap();
    assert_eq!(
        exact_outcome(&created.outcomes[0]).kind,
        RepositoryOutcomeKind::PullRequestCreated
    );

    let merged = serde_json::json!({
        "url": "https://github.com/acme/repo/pull/42",
        "number": 42,
        "id": "PR_42",
        "merge_commit_oid": "abcdefabcdefabcdefabcdefabcdefabcdefabcd"
    });
    let merged =
        linked_outcome_evidence(input("gh pr merge 42 --repo acme/repo", &merged)).unwrap();
    assert_eq!(
        exact_outcome(&merged.outcomes[0]).kind,
        RepositoryOutcomeKind::PullRequestMerged
    );
}

#[test]
fn plural_rebase_mapping_is_explicit_unambiguous_and_bounded() {
    let first_source = full_sha1(1);
    let second_source = full_sha1(2);
    let first_result = full_sha1(101);
    let second_result = full_sha1(102);
    let replacements = serde_json::json!([
        {"old_oid": first_source, "new_oid": first_result},
        {"old_oid": second_source, "new_oid": second_result},
    ]);
    let exact = serde_json::json!({
        "pre_head_oid": second_source,
        "post_head_oid": second_result,
        "replacements": replacements,
    });
    let evidence = linked_outcome_evidence(input("git rebase main", &exact)).unwrap();
    let [UnscopedOutcomeObservation::DeferredCommitOperation(operation)] =
        evidence.outcomes.as_slice()
    else {
        panic!("expected exact plural rebase mapping");
    };
    assert_eq!(operation.mappings.len(), 2);
    assert_eq!(
        operation.command_pre_head.as_ref().unwrap().hex,
        second_source
    );
    assert_eq!(operation.command_post_head.hex, second_result);
    let canonical_operation = operation.clone();

    let reordered = serde_json::json!({
        "pre_head_oid": second_source,
        "post_head_oid": second_result,
        "replacements": [
            {"old_oid": second_source, "new_oid": second_result},
            {"old_oid": first_source, "new_oid": first_result},
        ],
    });
    let reordered = linked_outcome_evidence(input("git rebase main", &reordered)).unwrap();
    let [UnscopedOutcomeObservation::DeferredCommitOperation(reordered)] =
        reordered.outcomes.as_slice()
    else {
        panic!("expected reordered exact plural rebase mapping");
    };
    assert_eq!(reordered, &canonical_operation);

    let headless = serde_json::json!({"replacements": replacements});
    let evidence = linked_outcome_evidence(input("git rebase main", &headless)).unwrap();
    assert!(evidence.outcomes.is_empty());
    assert_eq!(
        evidence.abstentions[0].0,
        RepositoryAbstentionReason::OutcomeResultInadmissible
    );

    for ambiguous in [
        serde_json::json!({
            "pre_head_oid": second_source,
            "post_head_oid": second_result,
            "replacements": [
                {"old_oid": first_source, "new_oid": first_result},
                {"old_oid": first_source, "new_oid": second_result},
            ],
        }),
        serde_json::json!({
            "pre_head_oid": second_source,
            "post_head_oid": second_result,
            "replacements": [
                {"old_oid": first_source, "new_oid": first_result},
                {"old_oid": second_source, "new_oid": first_result},
            ],
        }),
    ] {
        let evidence = linked_outcome_evidence(input("git rebase main", &ambiguous)).unwrap();
        assert!(evidence.outcomes.is_empty());
        assert_eq!(
            evidence.abstentions[0].0,
            RepositoryAbstentionReason::HistoryRewriteUnlinked
        );
    }

    let over_bound = (0..=MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS)
        .map(|index| {
            serde_json::json!({
                "old_oid": full_sha1(index + 1),
                "new_oid": full_sha1(index + 1_001),
            })
        })
        .collect::<Vec<_>>();
    let over_bound = serde_json::json!({
        "pre_head_oid": full_sha1(1),
        "post_head_oid": full_sha1(1_001),
        "replacements": over_bound,
    });
    let evidence = linked_outcome_evidence(input("git rebase main", &over_bound)).unwrap();
    assert!(evidence.outcomes.is_empty());
    assert_eq!(
        evidence.abstentions[0].0,
        RepositoryAbstentionReason::HistoryRewriteUnlinked
    );
}

#[test]
fn native_cherry_pick_stdout_is_deferred_but_failures_and_ambiguity_abstain() {
    let source = "0123456789abcdef0123456789abcdef01234567";
    let command = format!("git cherry-pick {source}");
    let output = Value::String(
        "[main a12bc34] Apply exact lineage\n 1 file changed, 1 insertion(+)\n".to_owned(),
    );
    let evidence = linked_outcome_evidence(input(&command, &output)).unwrap();
    let [UnscopedOutcomeObservation::DeferredCherryPick(deferred)] = evidence.outcomes.as_slice()
    else {
        panic!("expected deferred native cherry-pick");
    };
    assert_eq!(deferred.source.hex, source);
    assert_eq!(deferred.result_oid_prefix, "a12bc34");
    assert_eq!(deferred.result_subject, "Apply exact lineage");

    let mut failed = input(&command, &output);
    failed.result_outcome = OutputOutcome::Failure;
    let failed = linked_outcome_evidence(failed).unwrap();
    assert!(failed.outcomes.is_empty());
    assert_eq!(
        failed.abstentions[0].0,
        RepositoryAbstentionReason::OutcomeResultInadmissible
    );

    for output in [
        Value::String("error: could not apply 0123456... conflict\n".to_owned()),
        Value::String(
            "[main a12bc34] Apply exact lineage\n[main b23cd45] Another result\n".to_owned(),
        ),
    ] {
        let evidence = linked_outcome_evidence(input(&command, &output)).unwrap();
        assert!(evidence.outcomes.is_empty());
        assert_eq!(
            evidence.abstentions[0].0,
            RepositoryAbstentionReason::HistoryRewriteUnlinked
        );
    }
}
