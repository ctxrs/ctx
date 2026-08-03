use super::super::{git::EventProbeBudget, UnscopedPullRequestAssociationObservation};
use super::*;

fn association(repo: &Path, merged_as: String) -> UnscopedPullRequestAssociationObservation {
    UnscopedPullRequestAssociationObservation {
        repository_path: repo.to_string_lossy().into_owned(),
        pull_request: RepositoryPullRequestIdentity {
            forge_repository: forge("ctxrs", "ctx"),
            number: 203,
            provider_id: None,
        },
        merged_as: GitObjectId {
            format: GitObjectFormat::Sha1,
            hex: merged_as,
        },
        linkage: RepositoryOutcomeLinkage {
            provider: "codex".to_owned(),
            origin_call_id: "call-origin".to_owned(),
            result_call_id: "call-result".to_owned(),
            origin_event_sequence: 7,
            continuation_call_id_sha256: Vec::new(),
            result_record_sha256: [7; 32],
        },
    }
}

fn captured_association(
    repo: &Path,
    merged_as: String,
) -> ctx_history_core::RepositoryPullRequestAssociationObservation {
    let annotation = attribute(AttributionInput {
        activity_at_unix_ms: Some(10),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        provider_native_repository_aliases: vec![forge("ctxrs", "ctx")],
        outcome_operation_repository_path: Some(repo.to_string_lossy().into_owned()),
        pull_request_associations: vec![association(repo, merged_as)],
        ..AttributionInput::default()
    });
    annotation
        .repository_vcs_observations
        .into_iter()
        .find_map(|observation| match observation.kind {
            RepositoryVcsObservationKind::PullRequestAssociation(association) => Some(*association),
            _ => None,
        })
        .expect("source association remains admitted")
}

#[test]
fn exact_two_parent_merge_enriches_the_atomic_first_to_second_parent_range() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "merge-membership",
        Some("https://github.com/ctxrs/ctx.git"),
    );
    let primary = git_output(&repo, &["branch", "--show-current"]);
    run_git(&repo, &["checkout", "-qb", "feature"]);
    fs::write(repo.join("feature.txt"), "feature\n").unwrap();
    run_git(&repo, &["add", "feature.txt"]);
    run_git(&repo, &["commit", "-qm", "PR 203 change"]);
    let contains_commit = git_output(&repo, &["rev-parse", "HEAD"]);
    run_git(&repo, &["checkout", "-q", &primary]);
    run_git(
        &repo,
        &["merge", "--no-ff", "feature", "-m", "merge fixture"],
    );
    let merged_as = git_output(&repo, &["rev-parse", "HEAD"]);

    let association = captured_association(&repo, merged_as.clone());
    assert_eq!(association.merged_as.hex, merged_as);
    assert_eq!(
        association
            .contains_commits
            .iter()
            .map(|object_id| object_id.hex.as_str())
            .collect::<Vec<_>>(),
        [contains_commit.as_str()]
    );
}

#[test]
fn shallow_repository_never_publishes_false_pull_request_membership() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "shallow-membership",
        Some("https://github.com/ctxrs/ctx.git"),
    );
    let primary = git_output(&repo, &["branch", "--show-current"]);
    run_git(&repo, &["checkout", "-qb", "feature"]);
    run_git(&repo, &["commit", "--allow-empty", "-qm", "member"]);
    run_git(&repo, &["checkout", "-q", &primary]);
    run_git(
        &repo,
        &["merge", "--no-ff", "feature", "-m", "merge fixture"],
    );
    let merged_as = git_output(&repo, &["rev-parse", "HEAD"]);
    let first_parent = git_output(&repo, &["rev-parse", "HEAD^1"]);
    fs::write(repo.join(".git/shallow"), format!("{first_parent}\n")).unwrap();

    let annotation = attribute(AttributionInput {
        activity_at_unix_ms: Some(10),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        provider_native_repository_aliases: vec![forge("ctxrs", "ctx")],
        outcome_operation_repository_path: Some(repo.to_string_lossy().into_owned()),
        pull_request_associations: vec![association(&repo, merged_as)],
        ..AttributionInput::default()
    });
    let observed = annotation
        .repository_vcs_observations
        .iter()
        .find_map(|observation| match &observation.kind {
            RepositoryVcsObservationKind::PullRequestAssociation(association) => {
                Some(association.as_ref())
            }
            _ => None,
        })
        .expect("forge merge identity remains admitted");
    assert!(observed.contains_commits.is_empty());
    assert!(has_reason(
        &annotation,
        RepositoryAbstentionReason::OutcomeResultInadmissible
    ));
}

#[test]
fn object_mutation_between_dag_snapshots_never_publishes_membership() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "mutated-membership",
        Some("https://github.com/ctxrs/ctx.git"),
    );
    let primary = git_output(&repo, &["branch", "--show-current"]);
    run_git(&repo, &["checkout", "-qb", "feature"]);
    run_git(&repo, &["commit", "--allow-empty", "-qm", "member"]);
    let member = git_output(&repo, &["rev-parse", "HEAD"]);
    run_git(&repo, &["checkout", "-q", &primary]);
    run_git(
        &repo,
        &["merge", "--no-ff", "feature", "-m", "merge fixture"],
    );
    let merged_as = git_output(&repo, &["rev-parse", "HEAD"]);

    let certifier = GitCertifier::default();
    let certificate = certifier
        .certify(
            &repo,
            CandidateKind::Directory,
            RepositoryEvidenceKind::DeclaredToolWorkdir,
        )
        .unwrap();
    let object_path = repo
        .join(".git/objects")
        .join(&member[..2])
        .join(&member[2..]);
    let result = certifier.resolve_pull_request_merge_membership_for_test(
        &certificate,
        &GitObjectId {
            format: GitObjectFormat::Sha1,
            hex: merged_as,
        },
        &mut EventProbeBudget::new(),
        || fs::remove_file(&object_path).unwrap(),
    );
    assert!(result.is_err());
}

#[test]
fn shallow_state_created_between_dag_snapshots_never_publishes_membership() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "shallow-drift-membership",
        Some("https://github.com/ctxrs/ctx.git"),
    );
    let primary = git_output(&repo, &["branch", "--show-current"]);
    run_git(&repo, &["checkout", "-qb", "feature"]);
    run_git(&repo, &["commit", "--allow-empty", "-qm", "member"]);
    run_git(&repo, &["checkout", "-q", &primary]);
    run_git(
        &repo,
        &["merge", "--no-ff", "feature", "-m", "merge fixture"],
    );
    let merged_as = git_output(&repo, &["rev-parse", "HEAD"]);
    let first_parent = git_output(&repo, &["rev-parse", "HEAD^1"]);

    let certifier = GitCertifier::default();
    let certificate = certifier
        .certify(
            &repo,
            CandidateKind::Directory,
            RepositoryEvidenceKind::DeclaredToolWorkdir,
        )
        .unwrap();
    let result = certifier.resolve_pull_request_merge_membership_for_test(
        &certificate,
        &GitObjectId {
            format: GitObjectFormat::Sha1,
            hex: merged_as,
        },
        &mut EventProbeBudget::new(),
        || fs::write(repo.join(".git/shallow"), format!("{first_parent}\n")).unwrap(),
    );
    assert!(result.is_err());
}

#[test]
fn one_parent_and_octopus_commits_never_define_pull_request_membership() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "invalid-merge-geometry",
        Some("https://github.com/ctxrs/ctx.git"),
    );
    let primary = git_output(&repo, &["branch", "--show-current"]);
    run_git(
        &repo,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "ordinary one-parent commit",
        ],
    );
    let one_parent = git_output(&repo, &["rev-parse", "HEAD"]);
    for branch in ["feature-one", "feature-two"] {
        run_git(&repo, &["checkout", "-qb", branch, &primary]);
        run_git(
            &repo,
            &[
                "commit",
                "--allow-empty",
                "-qm",
                &format!("{branch} member"),
            ],
        );
    }
    run_git(&repo, &["checkout", "-q", &primary]);
    run_git(
        &repo,
        &[
            "merge",
            "--no-ff",
            "feature-one",
            "feature-two",
            "-m",
            "octopus fixture",
        ],
    );
    let octopus = git_output(&repo, &["rev-parse", "HEAD"]);

    let certifier = GitCertifier::default();
    let certificate = certifier
        .certify(
            &repo,
            CandidateKind::Directory,
            RepositoryEvidenceKind::DeclaredToolWorkdir,
        )
        .unwrap();
    for object_id in [one_parent, octopus] {
        assert!(certifier
            .resolve_pull_request_merge_membership_for_test(
                &certificate,
                &GitObjectId {
                    format: GitObjectFormat::Sha1,
                    hex: object_id,
                },
                &mut EventProbeBudget::new(),
                || {},
            )
            .is_err());
    }
}

#[test]
fn more_than_256_merge_members_never_publishes_partial_membership() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "membership-cap",
        Some("https://github.com/ctxrs/ctx.git"),
    );
    let primary = git_output(&repo, &["branch", "--show-current"]);
    run_git(&repo, &["checkout", "-qb", "feature"]);
    for index in 0..257 {
        run_git(
            &repo,
            &["commit", "--allow-empty", "-qm", &format!("member {index}")],
        );
    }
    run_git(&repo, &["checkout", "-q", &primary]);
    run_git(
        &repo,
        &["merge", "--no-ff", "feature", "-m", "merge fixture"],
    );
    let merged_as = git_output(&repo, &["rev-parse", "HEAD"]);

    let certifier = GitCertifier::default();
    let certificate = certifier
        .certify(
            &repo,
            CandidateKind::Directory,
            RepositoryEvidenceKind::DeclaredToolWorkdir,
        )
        .unwrap();
    assert!(certifier
        .resolve_pull_request_merge_membership_for_test(
            &certificate,
            &GitObjectId {
                format: GitObjectFormat::Sha1,
                hex: merged_as,
            },
            &mut EventProbeBudget::new(),
            || {},
        )
        .is_err());
}
