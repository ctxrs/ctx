fn exact_commit_outcome() -> RepositoryOutcomeObservation {
    RepositoryOutcomeObservation {
        kind: RepositoryOutcomeKind::Commit,
        produced_object_ids: vec![GitObjectId {
            format: GitObjectFormat::Sha1,
            hex: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        }],
        commit_operation: None,
        pull_request: None,
        pull_request_merge_commit: None,
        observed_at_unix_ms: 1,
        linkage: RepositoryOutcomeLinkage {
            provider: "fixture".to_owned(),
            origin_call_id: "origin".to_owned(),
            result_call_id: "result".to_owned(),
            origin_event_sequence: 1,
            continuation_call_id_sha256: Vec::new(),
            result_record_sha256: [7; 32],
        },
        outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
    }
}

fn exact_pull_request_outcome(alias: RepositoryAlias) -> RepositoryOutcomeObservation {
    RepositoryOutcomeObservation {
        kind: RepositoryOutcomeKind::PullRequestCreated,
        produced_object_ids: Vec::new(),
        commit_operation: None,
        pull_request: Some(RepositoryPullRequestIdentity {
            forge_repository: alias,
            number: 42,
            provider_id: Some("PR_42".to_owned()),
        }),
        pull_request_merge_commit: None,
        observed_at_unix_ms: 1,
        linkage: RepositoryOutcomeLinkage {
            provider: "fixture".to_owned(),
            origin_call_id: "origin".to_owned(),
            result_call_id: "result".to_owned(),
            origin_event_sequence: 1,
            continuation_call_id_sha256: Vec::new(),
            result_record_sha256: [7; 32],
        },
        outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
    }
}

fn linked_short_commit(
    repository: &Path,
    command: &str,
    output: String,
) -> super::LinkedOutcomeEvidence {
    let repository = repository.to_string_lossy().into_owned();
    let output = serde_json::Value::String(output);
    linked_outcome_evidence(LinkedOutcomeInput {
        provider: "fixture",
        command,
        session_cwd: Some(&repository),
        declared_workdir: Some(&repository),
        origin_call_id: "call-origin",
        result_call_id: "call-result",
        origin_event_sequence: 7,
        continuation_call_id_sha256: &[],
        result_record_sha256: [9; 32],
        observed_at_unix_ms: 10,
        result_outcome: OutputOutcome::Success,
        result_output: &output,
        structured_commit_oid: None,
        output_repository_path: Some(&repository),
    })
    .unwrap()
}

fn linked_exact_result(
    repository: &Path,
    command: &str,
    output: &serde_json::Value,
) -> super::LinkedOutcomeEvidence {
    let repository = repository.to_string_lossy().into_owned();
    linked_outcome_evidence(LinkedOutcomeInput {
        provider: "fixture",
        command,
        session_cwd: Some(&repository),
        declared_workdir: Some(&repository),
        origin_call_id: "call-origin",
        result_call_id: "call-result",
        origin_event_sequence: 7,
        continuation_call_id_sha256: &[],
        result_record_sha256: [9; 32],
        observed_at_unix_ms: 10,
        result_outcome: OutputOutcome::Success,
        result_output: output,
        structured_commit_oid: None,
        output_repository_path: Some(&repository),
    })
    .unwrap()
}

#[test]
fn public_ctx_short_commit_resolves_full_oid_parents_and_changed_files() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "public-ctx",
        Some("https://github.com/ctxrs/ctx.git"),
    );
    run_git(
        &repo,
        &[
            "remote",
            "add",
            "internal",
            "https://github.com/ctxrs/ctx-internal.git",
        ],
    );
    fs::create_dir(repo.join("src")).unwrap();
    fs::write(repo.join("tracked.txt"), "changed\n").unwrap();
    fs::write(repo.join("src/new.rs"), "pub fn added() {}\n").unwrap();
    run_git(&repo, &["add", "tracked.txt", "src/new.rs"]);
    run_git(&repo, &["commit", "-qm", "Capture public ctx outcome"]);
    let oid = git_output(&repo, &["rev-parse", "HEAD"]);
    let short = git_output(&repo, &["rev-parse", "--short=7", "HEAD"]);
    let parent = git_output(&repo, &["rev-parse", "HEAD^"]);
    let mut packed_refs = b"# pack-refs with: sorted\n".to_vec();
    for index in 0..3_000 {
        packed_refs.extend_from_slice(
            format!("{parent} refs/heads/packed-fixture-{index:04}\n").as_bytes(),
        );
    }
    fs::write(repo.join(".git/packed-refs"), packed_refs).unwrap();
    let linked = linked_short_commit(
        &repo,
        "git commit -m 'Capture public ctx outcome'",
        format!(
            "[main {short}] Capture public ctx outcome\n 2 files changed, 2 insertions(+), 1 deletion(-)\n"
        ),
    );
    let annotation = attribute(AttributionInput {
        activity_at_unix_ms: Some(10),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        command: Some("git commit -m 'Capture public ctx outcome'".to_owned()),
        outcome_operation_repository_path: linked.outcome_operation_repository_path,
        outcome_output_repository_path: linked.outcome_output_repository_path,
        outcome_observations: linked.outcomes,
        outcome_abstentions: linked.abstentions,
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_bindings.len(), 1);
    assert_eq!(
        annotation.repository_bindings[0].logical_repository_id,
        "forge:github.com/ctxrs/ctx"
    );
    let commit = annotation
        .repository_vcs_observations
        .iter()
        .find(|observation| observation.kind == RepositoryVcsObservationKind::Commit)
        .unwrap();
    assert_eq!(commit.object_id.as_ref().unwrap().hex, oid);
    assert_eq!(commit.parent_object_ids.len(), 1);
    assert_eq!(commit.parent_object_ids[0].hex, parent);
    let mut files = annotation
        .repository_file_observations
        .iter()
        .map(|observation| observation.relative_path.as_str())
        .collect::<Vec<_>>();
    files.sort_unstable();
    assert_eq!(files, ["src/new.rs", "tracked.txt"]);
    assert!(annotation.repository_abstentions.is_empty());
}

#[test]
fn short_amend_without_an_exact_source_result_map_abstains() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "amended", None);
    fs::write(repo.join("tracked.txt"), "amended\n").unwrap();
    run_git(&repo, &["add", "tracked.txt"]);
    run_git(&repo, &["commit", "--amend", "-qm", "Amended outcome"]);
    let short = git_output(&repo, &["rev-parse", "--short=7", "HEAD"]);
    let linked = linked_short_commit(
        &repo,
        "git commit --amend --no-edit",
        format!("[main {short}] Amended outcome\n 1 file changed, 1 insertion(+), 1 deletion(-)\n"),
    );
    let annotation = attribute(AttributionInput {
        activity_at_unix_ms: Some(10),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        command: Some("git commit --amend --no-edit".to_owned()),
        outcome_operation_repository_path: linked.outcome_operation_repository_path,
        outcome_output_repository_path: linked.outcome_output_repository_path,
        outcome_observations: linked.outcomes,
        outcome_abstentions: linked.abstentions,
        ..AttributionInput::default()
    });
    assert!(annotation
        .repository_vcs_observations
        .iter()
        .all(|observation| {
            !matches!(&observation.kind, RepositoryVcsObservationKind::Outcome(_))
        }));
    assert!(has_reason(
        &annotation,
        RepositoryAbstentionReason::HistoryRewriteUnlinked
    ));
}

#[test]
fn exact_plural_rebase_receipt_certifies_all_source_result_mappings() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "plural-rebase", None);
    let primary = git_output(&repo, &["branch", "--show-current"]);
    run_git(&repo, &["checkout", "-qb", "feature"]);
    fs::write(repo.join("first.txt"), "first\n").unwrap();
    run_git(&repo, &["add", "first.txt"]);
    run_git(&repo, &["commit", "-qm", "First rebased commit"]);
    let first_source = git_output(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("second.txt"), "second\n").unwrap();
    run_git(&repo, &["add", "second.txt"]);
    run_git(&repo, &["commit", "-qm", "Second rebased commit"]);
    let second_source = git_output(&repo, &["rev-parse", "HEAD"]);

    run_git(&repo, &["checkout", "-q", &primary]);
    fs::write(repo.join("primary.txt"), "primary\n").unwrap();
    run_git(&repo, &["add", "primary.txt"]);
    run_git(&repo, &["commit", "-qm", "Advance primary"]);
    run_git(&repo, &["checkout", "-q", "feature"]);
    run_git(&repo, &["rebase", &primary]);
    let second_result = git_output(&repo, &["rev-parse", "HEAD"]);
    let first_result = git_output(&repo, &["rev-parse", "HEAD^"]);
    assert_ne!(first_source, first_result);
    assert_ne!(second_source, second_result);

    let receipt = serde_json::json!({
        "pre_head_oid": second_source,
        "post_head_oid": second_result,
        "replacements": [
            {"old_oid": second_source, "new_oid": second_result},
            {"old_oid": first_source, "new_oid": first_result},
        ],
    });
    let command = format!("git rebase {primary}");
    let linked = linked_exact_result(&repo, &command, &receipt);
    let annotation = attribute(AttributionInput {
        activity_at_unix_ms: Some(10),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        command: Some(command),
        outcome_operation_repository_path: linked.outcome_operation_repository_path,
        outcome_output_repository_path: linked.outcome_output_repository_path,
        outcome_observations: linked.outcomes,
        outcome_abstentions: linked.abstentions,
        ..AttributionInput::default()
    });

    let outcome = annotation
        .repository_vcs_observations
        .iter()
        .find_map(|observation| match &observation.kind {
            RepositoryVcsObservationKind::Outcome(outcome) => Some(outcome.as_ref()),
            _ => None,
        })
        .expect("expected exact plural rebase outcome");
    let operation = outcome.commit_operation.as_ref().unwrap();
    assert_eq!(
        operation.kind,
        ctx_history_core::RepositoryCommitOperationKind::Rebase
    );
    assert_eq!(operation.mappings.len(), 2);
    let actual = operation
        .mappings
        .iter()
        .map(|mapping| (mapping.source.hex.as_str(), mapping.result.hex.as_str()))
        .collect::<Vec<_>>();
    let mut expected = vec![
        (first_source.as_str(), first_result.as_str()),
        (second_source.as_str(), second_result.as_str()),
    ];
    expected.sort_unstable();
    assert_eq!(actual, expected);
    assert_eq!(
        operation
            .repository_verified_yields()
            .map(|oid| oid.hex.as_str())
            .collect::<Vec<_>>(),
        operation
            .mappings
            .iter()
            .map(|mapping| mapping.result.hex.as_str())
            .collect::<Vec<_>>()
    );
    let ctx_history_core::RepositoryCommitOperationProof::RepositoryVerifiedYield(proof) =
        &operation.proof
    else {
        panic!("expected repository-verified plural rebase proof");
    };
    assert_eq!(proof.command_pre_head.as_ref().unwrap().hex, second_source);
    assert_eq!(proof.sequencer_pre_head, proof.command_pre_head);
    assert_eq!(proof.command_post_head.hex, second_result);
    assert!(annotation.repository_abstentions.is_empty());
}

#[test]
fn native_cherry_pick_receipt_admits_exact_derivation_mapping() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "cherry-pick", None);
    let primary = git_output(&repo, &["branch", "--show-current"]);
    run_git(&repo, &["checkout", "-qb", "source"]);
    fs::write(repo.join("picked.txt"), "picked\n").unwrap();
    run_git(&repo, &["add", "picked.txt"]);
    run_git(&repo, &["commit", "-qm", "Apply exact lineage"]);
    let source = git_output(&repo, &["rev-parse", "HEAD"]);
    run_git(&repo, &["checkout", "-q", &primary]);
    fs::write(repo.join("primary.txt"), "diverge\n").unwrap();
    run_git(&repo, &["add", "primary.txt"]);
    run_git(&repo, &["commit", "-qm", "Diverge primary"]);
    run_git(&repo, &["cherry-pick", &source]);
    let result = git_output(&repo, &["rev-parse", "HEAD"]);
    let short = git_output(&repo, &["rev-parse", "--short=7", "HEAD"]);
    assert_ne!(source, result);

    let command = format!("git cherry-pick {source}");
    let linked = linked_short_commit(
        &repo,
        &command,
        format!("[{primary} {short}] Apply exact lineage\n 1 file changed, 1 insertion(+)\n"),
    );
    let annotation = attribute(AttributionInput {
        activity_at_unix_ms: Some(10),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        command: Some(command),
        outcome_operation_repository_path: linked.outcome_operation_repository_path,
        outcome_output_repository_path: linked.outcome_output_repository_path,
        outcome_observations: linked.outcomes,
        outcome_abstentions: linked.abstentions,
        ..AttributionInput::default()
    });

    let outcome = annotation
        .repository_vcs_observations
        .iter()
        .find_map(|observation| match &observation.kind {
            RepositoryVcsObservationKind::Outcome(outcome) => Some(outcome.as_ref()),
            _ => None,
        })
        .expect("expected exact cherry-pick outcome");
    let operation = outcome.commit_operation.as_ref().unwrap();
    assert_eq!(
        operation.kind,
        ctx_history_core::RepositoryCommitOperationKind::CherryPick
    );
    assert_eq!(
        operation.operation_class(),
        ctx_history_core::RepositoryCommitOperationClass::Derivation
    );
    assert_eq!(operation.mappings[0].source.hex, source);
    assert_eq!(operation.mappings[0].result.hex, result);
    assert_eq!(
        operation
            .repository_verified_yields()
            .map(|oid| oid.hex.as_str())
            .collect::<Vec<_>>(),
        [result.as_str()]
    );
    assert!(annotation.repository_abstentions.is_empty());
}

#[cfg(unix)]
#[test]
fn native_cherry_pick_substituted_after_source_read_abstains() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "cherry-pick-substitution", None);
    let primary = git_output(&repo, &["branch", "--show-current"]);
    run_git(&repo, &["checkout", "-qb", "source"]);
    fs::write(repo.join("picked.txt"), "picked\n").unwrap();
    run_git(&repo, &["add", "picked.txt"]);
    run_git(&repo, &["commit", "-qm", "Apply substituted lineage"]);
    let source = git_output(&repo, &["rev-parse", "HEAD"]);

    run_git(&repo, &["checkout", "-q", &primary]);
    fs::write(repo.join("primary.txt"), "diverge\n").unwrap();
    run_git(&repo, &["add", "primary.txt"]);
    run_git(&repo, &["commit", "-qm", "Diverge substitution primary"]);
    let substitute = git_output(&repo, &["rev-parse", "HEAD"]);
    run_git(&repo, &["cherry-pick", &source]);
    let short = git_output(&repo, &["rev-parse", "--short=7", "HEAD"]);
    run_git(&repo, &["branch", "-D", "source"]);

    let source_object = loose_object_path(&repo, &source);
    let substitute_object = loose_object_path(&repo, &substitute);
    assert!(source_object.is_file());
    assert!(substitute_object.is_file());
    let wrapper =
        delegating_git_with_object_mutation(temp.path(), &source_object, Some(&substitute_object));
    let command = format!("git cherry-pick {source}");
    let linked = linked_short_commit(
        &repo,
        &command,
        format!("[{primary} {short}] Apply substituted lineage\n 1 file changed, 1 insertion(+)\n"),
    );
    let mut attributor = RepositoryAttributor::default();
    attributor.certifier = GitCertifier::for_test(wrapper, Duration::from_secs(2));
    let annotation = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(10),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        command: Some(command),
        outcome_operation_repository_path: linked.outcome_operation_repository_path,
        outcome_output_repository_path: linked.outcome_output_repository_path,
        outcome_observations: linked.outcomes,
        outcome_abstentions: linked.abstentions,
        ..AttributionInput::default()
    });

    assert!(annotation
        .repository_vcs_observations
        .iter()
        .all(|observation| {
            !matches!(&observation.kind, RepositoryVcsObservationKind::Outcome(_))
        }));
    assert!(annotation.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::OutcomeResultInadmissible
            && abstention.detail.as_deref()
                == Some("cherry_pick_source_or_result_did_not_resolve_exactly")
    }));
    assert!(attributor.git_subprocess_count() <= super::git::MAX_GIT_SUBPROCESSES_PER_EVENT);
    assert_eq!(
        fs::read(&source_object).unwrap(),
        fs::read(&substitute_object).unwrap()
    );
    assert!(temp.path().join("mapped-object-first-read").is_dir());
}

#[test]
fn plural_mapping_model_preserves_native_sha256_cherry_pick_certification() {
    let temp = TempDir::new().unwrap();
    let Some(repo) = sha256_repository(temp.path(), "sha256-cherry-pick") else {
        eprintln!("host Git does not support git init --object-format=sha256; skipping fixture");
        return;
    };
    let primary = git_output(&repo, &["branch", "--show-current"]);
    run_git(&repo, &["checkout", "-qb", "source"]);
    fs::write(repo.join("picked.txt"), "picked\n").unwrap();
    run_git(&repo, &["add", "picked.txt"]);
    run_git(&repo, &["commit", "-qm", "Apply SHA-256 lineage"]);
    let source = git_output(&repo, &["rev-parse", "HEAD"]);
    run_git(&repo, &["checkout", "-q", &primary]);
    fs::write(repo.join("primary.txt"), "diverge\n").unwrap();
    run_git(&repo, &["add", "primary.txt"]);
    run_git(&repo, &["commit", "-qm", "Diverge SHA-256 primary"]);
    run_git(&repo, &["cherry-pick", &source]);
    let result = git_output(&repo, &["rev-parse", "HEAD"]);
    let short = git_output(&repo, &["rev-parse", "--short=7", "HEAD"]);
    assert_ne!(source, result);
    assert_eq!(source.len(), 64);
    assert_eq!(result.len(), 64);
    assert!(source.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(result.bytes().all(|byte| byte.is_ascii_hexdigit()));

    let command = format!("git cherry-pick {source}");
    let linked = linked_short_commit(
        &repo,
        &command,
        format!("[{primary} {short}] Apply SHA-256 lineage\n 1 file changed, 1 insertion(+)\n"),
    );
    let annotation = attribute(AttributionInput {
        activity_at_unix_ms: Some(10),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        command: Some(command),
        outcome_operation_repository_path: linked.outcome_operation_repository_path,
        outcome_output_repository_path: linked.outcome_output_repository_path,
        outcome_observations: linked.outcomes,
        outcome_abstentions: linked.abstentions,
        ..AttributionInput::default()
    });

    let outcome = annotation
        .repository_vcs_observations
        .iter()
        .find_map(|observation| match &observation.kind {
            RepositoryVcsObservationKind::Outcome(outcome) => Some(outcome.as_ref()),
            _ => None,
        })
        .expect("expected exact SHA-256 cherry-pick outcome");
    let operation = outcome.commit_operation.as_ref().unwrap();
    assert_eq!(operation.mappings.len(), 1);
    assert_eq!(operation.mappings[0].source.hex, source);
    assert_eq!(operation.mappings[0].result.hex, result);
    assert_eq!(operation.mappings[0].source.format, GitObjectFormat::Sha256);
    assert_eq!(operation.mappings[0].result.format, GitObjectFormat::Sha256);
    let ctx_history_core::RepositoryCommitOperationProof::RepositoryVerifiedYield(proof) =
        &operation.proof
    else {
        panic!("expected repository-verified SHA-256 yield proof");
    };
    assert_eq!(
        proof.repository_geometry_before_sha256,
        proof.repository_geometry_after_sha256
    );
    assert_ne!(proof.repository_geometry_before_sha256, [0; 32]);
    assert_eq!(
        proof.exact_source_oids,
        [operation.mappings[0].source.clone()]
    );
    assert!(annotation.repository_abstentions.is_empty());
}

#[test]
fn short_merge_resolves_ordered_parents_and_first_parent_files() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "merge", None);
    let primary = git_output(&repo, &["branch", "--show-current"]);
    run_git(&repo, &["checkout", "-qb", "docs"]);
    fs::write(repo.join("docs.md"), "docs\n").unwrap();
    run_git(&repo, &["add", "docs.md"]);
    run_git(&repo, &["commit", "-qm", "Add docs"]);
    run_git(&repo, &["checkout", "-q", &primary]);
    run_git(&repo, &["merge", "--no-ff", "docs", "-m", "Merge docs"]);
    let oid = git_output(&repo, &["rev-parse", "HEAD"]);
    let short = git_output(&repo, &["rev-parse", "--short=7", "HEAD"]);
    let parents = git_output(&repo, &["show", "-s", "--format=%P", "HEAD"]);
    let linked = linked_short_commit(
        &repo,
        "git merge --no-ff docs",
        format!("Merge made by the 'ort' strategy.\n*   {short} Merge docs\n"),
    );
    let annotation = attribute(AttributionInput {
        activity_at_unix_ms: Some(10),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        command: Some("git merge --no-ff docs".to_owned()),
        outcome_operation_repository_path: linked.outcome_operation_repository_path,
        outcome_output_repository_path: linked.outcome_output_repository_path,
        outcome_observations: linked.outcomes,
        outcome_abstentions: linked.abstentions,
        ..AttributionInput::default()
    });
    let commit = annotation
        .repository_vcs_observations
        .iter()
        .find(|observation| observation.kind == RepositoryVcsObservationKind::Commit)
        .unwrap();
    assert_eq!(commit.object_id.as_ref().unwrap().hex, oid);
    assert_eq!(
        commit
            .parent_object_ids
            .iter()
            .map(|parent| parent.hex.as_str())
            .collect::<Vec<_>>(),
        parents.split(' ').collect::<Vec<_>>()
    );
    assert_eq!(annotation.repository_file_observations.len(), 1);
    assert_eq!(
        annotation.repository_file_observations[0].relative_path,
        "docs.md"
    );
}

#[test]
fn rewritten_unreachable_short_commit_remains_inadmissible() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "rewritten", None);
    fs::write(repo.join("tracked.txt"), "transient\n").unwrap();
    run_git(&repo, &["add", "tracked.txt"]);
    run_git(&repo, &["commit", "-qm", "Transient outcome"]);
    let short = git_output(&repo, &["rev-parse", "--short=7", "HEAD"]);
    run_git(&repo, &["commit", "--amend", "-qm", "Final outcome"]);
    let linked = linked_short_commit(
        &repo,
        "git commit -m 'Transient outcome'",
        format!("[main {short}] Transient outcome\n"),
    );
    let annotation = attribute(AttributionInput {
        activity_at_unix_ms: Some(10),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        command: Some("git commit -m 'Transient outcome'".to_owned()),
        outcome_operation_repository_path: linked.outcome_operation_repository_path,
        outcome_output_repository_path: linked.outcome_output_repository_path,
        outcome_observations: linked.outcomes,
        outcome_abstentions: linked.abstentions,
        ..AttributionInput::default()
    });
    assert!(annotation
        .repository_vcs_observations
        .iter()
        .all(|observation| observation.kind != RepositoryVcsObservationKind::Commit));
    assert!(annotation.repository_file_observations.is_empty());
    assert!(has_reason(
        &annotation,
        RepositoryAbstentionReason::OutcomeResultInadmissible
    ));
}

#[test]
fn opaque_command_routes_block_outcomes_but_retain_structured_workdir() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    for (command, reason) in [
        (
            "safe-wrap git commit -m exact",
            RepositoryAbstentionReason::UnknownWrapper,
        ),
        (
            "bash -lc 'git commit -m exact'",
            RepositoryAbstentionReason::ProfileDependent,
        ),
        (
            "git -C $REPO commit -m exact",
            RepositoryAbstentionReason::DynamicPath,
        ),
        (
            "git ci -m exact",
            RepositoryAbstentionReason::UnknownWrapper,
        ),
    ] {
        let path = repo.to_string_lossy().into_owned();
        let annotation = attribute(AttributionInput {
            declared_tool_workdir: Some(path.clone()),
            command: Some(command.to_owned()),
            outcome_operation_repository_path: Some(path.clone()),
            outcome_output_repository_path: Some(path.clone()),
            outcome_observations: vec![exact_commit_outcome().into()],
            ..AttributionInput::default()
        });
        assert_eq!(annotation.repository_bindings.len(), 1, "{command}");
        assert!(annotation.repository_bindings[0]
            .evidence
            .iter()
            .any(|evidence| evidence.kind == RepositoryEvidenceKind::DeclaredToolWorkdir));
        assert!(
            annotation.repository_vcs_observations.is_empty(),
            "{command}"
        );
        assert!(has_reason(&annotation, reason), "{command}");
        assert!(has_reason(
            &annotation,
            RepositoryAbstentionReason::OutcomeRepositoryUnbound
        ));
        assert_eq!(
            candidate_paths(&annotation, RepositoryCandidateKind::DeclaredToolWorkdir),
            vec![path.as_str()]
        );
    }
}

#[test]
fn failed_explicit_outcome_route_never_uses_sole_provider_logical_binding() {
    let temp = TempDir::new().unwrap();
    let missing = temp.path().join("missing");
    let path = missing.to_string_lossy().into_owned();
    let annotation = attribute(AttributionInput {
        provider_native_repository_aliases: vec![forge("acme", "provider-only")],
        command: Some(format!(
            "git -C {path} commit -m exact && git -C {path} rev-parse HEAD"
        )),
        outcome_operation_repository_path: Some(path.clone()),
        outcome_output_repository_path: Some(path),
        outcome_observations: vec![exact_commit_outcome().into()],
        ..AttributionInput::default()
    });
    assert_eq!(annotation.repository_bindings.len(), 1);
    assert!(annotation.repository_bindings[0]
        .local_root_authorization
        .is_none());
    assert!(annotation.repository_vcs_observations.is_empty());
    assert!(has_reason(
        &annotation,
        RepositoryAbstentionReason::OutcomeRepositoryUnbound
    ));
}

#[test]
fn exact_pull_request_outcome_uses_provider_binding_without_a_live_local_route() {
    let temp = TempDir::new().unwrap();
    let missing = temp.path().join("moved-or-absent");
    let alias = forge("acme", "provider-only");
    let annotation = attribute(AttributionInput {
        provider_native_repository_aliases: vec![alias.clone()],
        command: Some("gh pr create --repo acme/provider-only".to_owned()),
        outcome_operation_repository_path: Some(missing.to_string_lossy().into_owned()),
        outcome_output_repository_path: Some(missing.to_string_lossy().into_owned()),
        outcome_observations: vec![exact_pull_request_outcome(alias).into()],
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_bindings.len(), 1);
    assert_eq!(
        annotation.repository_bindings[0].logical_repository_id,
        "forge:github.com/acme/provider-only"
    );
    assert!(annotation.repository_bindings[0]
        .local_root_authorization
        .is_none());
    assert_eq!(annotation.repository_vcs_observations.len(), 1);
    assert_eq!(
        annotation.repository_vcs_observations[0].repository_binding_id,
        annotation.repository_bindings[0].binding_id
    );
    assert!(!has_reason(
        &annotation,
        RepositoryAbstentionReason::OutcomeRepositoryUnbound
    ));
}
