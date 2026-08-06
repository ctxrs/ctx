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
