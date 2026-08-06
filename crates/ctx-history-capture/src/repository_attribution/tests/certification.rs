use super::super::{git, shell};
use super::*;

#[test]
fn candidate_products_abstain_before_any_git_probe() {
    let file_observations = (0..33)
        .map(|index| UnscopedFileObservation {
            path: format!("/definitely-missing/ctx-candidate-{index}"),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        })
        .collect();
    let files = attribute(AttributionInput {
        file_observations,
        ..AttributionInput::default()
    });
    assert!(has_reason(
        &files,
        RepositoryAbstentionReason::CandidateLimitExceeded
    ));
    assert!(!has_reason(
        &files,
        RepositoryAbstentionReason::CandidateMissingBeforeCertification
    ));
    assert_eq!(
        candidate_paths(&files, RepositoryCandidateKind::FileActivityPath).len(),
        33
    );

    let command = (0..33)
        .map(|index| format!("git -C /definitely-missing/ctx-command-{index} status"))
        .collect::<Vec<_>>()
        .join(" && ");
    let commands = attribute(AttributionInput {
        command: Some(command),
        ..AttributionInput::default()
    });
    assert!(has_reason(
        &commands,
        RepositoryAbstentionReason::CandidateLimitExceeded
    ));
    assert!(!has_reason(
        &commands,
        RepositoryAbstentionReason::CandidateMissingBeforeCertification
    ));
}

#[test]
fn command_candidate_limit_preserves_independent_evidence() {
    let temp = TempDir::new().unwrap();
    let workdir = repository(
        temp.path(),
        "bounded-workdir",
        Some("https://github.com/acme/bounded-workdir.git"),
    );
    let activity = repository(
        temp.path(),
        "bounded-activity",
        Some("https://github.com/acme/bounded-activity.git"),
    );
    let command = (0..33)
        .map(|index| format!("git -C /definitely-missing/ctx-command-{index} status"))
        .collect::<Vec<_>>()
        .join(" && ");
    let independent = attribute(AttributionInput {
        declared_tool_workdir: Some(workdir.to_string_lossy().into_owned()),
        command: Some(command),
        file_observations: vec![UnscopedFileObservation {
            path: activity.join("tracked.txt").to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        }],
        ..AttributionInput::default()
    });
    assert_eq!(independent.repository_bindings.len(), 2);
    assert_eq!(independent.repository_file_observations.len(), 1);
    assert!(has_reason(
        &independent,
        RepositoryAbstentionReason::CandidateLimitExceeded
    ));
    assert!(!has_reason(
        &independent,
        RepositoryAbstentionReason::CandidateMissingBeforeCertification
    ));
}

#[test]
fn oversized_packed_refs_fails_closed() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "oversized-packed-refs",
        Some("https://github.com/ctxrs/ctx.git"),
    );
    let packed_refs = fs::File::create(repo.join(".git/packed-refs")).unwrap();
    packed_refs.set_len(8 * 1024 * 1024 + 1).unwrap();

    let annotation = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });

    assert!(annotation.repository_bindings.is_empty());
    assert!(annotation.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::GitProbeFailed
            && abstention.detail.as_deref() == Some("mutable_git_evidence_limit_exceeded")
    }));
}

#[test]
fn one_event_is_bounded_to_two_full_certificates_and_the_git_subprocess_budget() {
    let temp = TempDir::new().unwrap();
    let repositories = [
        repository(temp.path(), "first-budget", None),
        repository(temp.path(), "second-budget", None),
        repository(temp.path(), "third-budget", None),
    ];
    let command = repositories
        .iter()
        .map(|path| format!("git -C {} status", path.display()))
        .collect::<Vec<_>>()
        .join(" && ");
    let mut attributor = RepositoryAttributor::default();
    let annotation = attributor.attribute(AttributionInput {
        command: Some(command),
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_bindings.len(), 2);
    assert!(has_reason(
        &annotation,
        RepositoryAbstentionReason::ProbeBudgetExceeded
    ));
    assert_eq!(
        attributor.full_certification_probe_count(),
        git::MAX_FULL_CERTIFICATIONS_PER_EVENT
    );
    assert!(attributor.git_subprocess_count() > 0);
    assert!(attributor.git_subprocess_count() <= git::MAX_GIT_SUBPROCESSES_PER_EVENT);
}

#[test]
fn plural_operation_objects_are_batch_verified_with_an_explicit_unique_bound() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "plural-operation-objects", None);
    let mut object_ids = vec![GitObjectId {
        format: GitObjectFormat::Sha1,
        hex: git_output(&repo, &["rev-parse", "HEAD"]),
    }];
    for index in 0..3 {
        fs::write(
            repo.join(format!("mapped-{index}.txt")),
            format!("{index}\n"),
        )
        .unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", &format!("mapped {index}")]);
        object_ids.push(GitObjectId {
            format: GitObjectFormat::Sha1,
            hex: git_output(&repo, &["rev-parse", "HEAD"]),
        });
    }
    object_ids.sort();

    let certifier = GitCertifier::default();
    let certificate = certifier
        .certify(
            &repo,
            CandidateKind::Directory,
            RepositoryEvidenceKind::DeclaredToolWorkdir,
        )
        .unwrap();
    let subprocesses_before = certifier.git_subprocess_count();
    let mut budget = git::EventProbeBudget::new();
    let domain = certifier
        .verify_commit_operation_objects(&certificate, &object_ids, &mut budget)
        .unwrap();
    assert_ne!(domain, [0; 32]);
    assert_eq!(certifier.git_subprocess_count(), subprocesses_before + 1);

    let over_bound = (0..=git::MAX_VERIFIED_COMMIT_OPERATION_OBJECTS)
        .map(|index| GitObjectId {
            format: GitObjectFormat::Sha1,
            hex: format!("{:040x}", index + 1),
        })
        .collect::<Vec<_>>();
    let failure = certifier
        .verify_commit_operation_objects(&certificate, &over_bound, &mut budget)
        .unwrap_err();
    assert_eq!(
        failure,
        git::ProbeFailure::Failed("commit_operation_object_bound_exceeded")
    );
    assert_eq!(certifier.git_subprocess_count(), subprocesses_before + 1);
}

#[cfg(unix)]
#[test]
fn every_candidate_route_is_revalidated_in_both_ancestor_descendant_orders() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    let outside = repository(temp.path(), "outside", None);
    let descendant = repo.join("route");

    let mut ancestor_first = RepositoryAttributor::default();
    let root = ancestor_first.attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(root.repository_bindings.len(), 1);
    symlink(&outside, &descendant).unwrap();
    let unsafe_descendant = ancestor_first.attribute(AttributionInput {
        declared_tool_workdir: Some(descendant.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert!(unsafe_descendant.repository_bindings.is_empty());
    assert!(has_reason(
        &unsafe_descendant,
        RepositoryAbstentionReason::UnsafePath
    ));

    let mut descendant_first = RepositoryAttributor::default();
    let unsafe_descendant = descendant_first.attribute(AttributionInput {
        declared_tool_workdir: Some(descendant.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert!(unsafe_descendant.repository_bindings.is_empty());
    assert!(has_reason(
        &unsafe_descendant,
        RepositoryAbstentionReason::UnsafePath
    ));
    fs::remove_file(&descendant).unwrap();
    fs::create_dir(&descendant).unwrap();
    let safe_root = descendant_first.attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(safe_root.repository_bindings.len(), 1);
}

#[cfg(unix)]
#[test]
fn certification_cache_is_constant_probe_for_repeated_events_and_invalidates_safely() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let first = repository(temp.path(), "first", None);
    let second = repository(temp.path(), "second", None);
    let mut attributor = RepositoryAttributor::default();

    for _ in 0..1_000 {
        let annotation = attributor.attribute(AttributionInput {
            declared_tool_workdir: Some(first.to_string_lossy().into_owned()),
            ..AttributionInput::default()
        });
        assert_eq!(annotation.repository_bindings.len(), 1);
    }
    assert_eq!(attributor.full_certification_probe_count(), 1);

    let file_evidence = attributor.attribute(AttributionInput {
        file_observations: vec![UnscopedFileObservation {
            path: first.join("tracked.txt").to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        }],
        ..AttributionInput::default()
    });
    assert_eq!(attributor.full_certification_probe_count(), 1);
    assert_eq!(
        file_evidence.repository_bindings[0].evidence[0].kind,
        RepositoryEvidenceKind::FileActivity
    );

    for _ in 0..100 {
        for path in [&first, &second] {
            let annotation = attributor.attribute(AttributionInput {
                declared_tool_workdir: Some(path.to_string_lossy().into_owned()),
                ..AttributionInput::default()
            });
            assert_eq!(annotation.repository_bindings.len(), 1);
        }
    }
    assert_eq!(attributor.full_certification_probe_count(), 2);

    let moved = temp.path().join("moved-first");
    fs::rename(&first, &moved).unwrap();
    let moved_binding = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(moved.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(moved_binding.repository_bindings.len(), 1);
    assert_eq!(attributor.full_certification_probe_count(), 3);

    let route = moved.join("route");
    fs::create_dir(&route).unwrap();
    let safe_route = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(route.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(safe_route.repository_bindings.len(), 1);
    assert_eq!(attributor.full_certification_probe_count(), 3);
    fs::remove_dir(&route).unwrap();
    symlink(&second, &route).unwrap();
    let swapped_route = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(route.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert!(swapped_route.repository_bindings.is_empty());
    assert!(has_reason(
        &swapped_route,
        RepositoryAbstentionReason::UnsafePath
    ));
    assert_eq!(attributor.full_certification_probe_count(), 3);

    let later_repo = temp.path().join("later-repo");
    for _ in 0..2 {
        let negative = attributor.attribute(AttributionInput {
            declared_tool_workdir: Some(later_repo.to_string_lossy().into_owned()),
            ..AttributionInput::default()
        });
        assert!(negative.repository_bindings.is_empty());
    }
    assert_eq!(attributor.full_certification_probe_count(), 4);
    fs::create_dir(&later_repo).unwrap();
    run_git(&later_repo, &["init", "-q"]);
    run_git(&later_repo, &["config", "user.name", "ctx test"]);
    run_git(
        &later_repo,
        &["config", "user.email", "ctx@example.invalid"],
    );
    fs::write(later_repo.join("tracked.txt"), "tracked\n").unwrap();
    run_git(&later_repo, &["add", "tracked.txt"]);
    run_git(&later_repo, &["commit", "-qm", "created later"]);
    let discovered = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(later_repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(discovered.repository_bindings.len(), 1);
    assert_eq!(attributor.full_certification_probe_count(), 5);
}

#[cfg(unix)]
#[test]
fn independent_worker_caches_revalidate_replaced_git_identity() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    let mut workers = [
        RepositoryAttributor::default(),
        RepositoryAttributor::default(),
    ];
    for worker in &mut workers {
        let initial = worker.attribute(AttributionInput {
            activity_at_unix_ms: Some(100),
            declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
            ..AttributionInput::default()
        });
        assert_eq!(initial.repository_bindings.len(), 1);
        assert_eq!(worker.full_certification_probe_count(), 1);
    }

    fs::rename(repo.join(".git"), repo.join(".git-old")).unwrap();
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["config", "user.name", "ctx test"]);
    run_git(&repo, &["config", "user.email", "ctx@example.invalid"]);
    run_git(&repo, &["add", "tracked.txt"]);
    run_git(&repo, &["commit", "-qm", "replacement"]);

    for worker in &mut workers {
        let replaced = worker.attribute(AttributionInput {
            activity_at_unix_ms: Some(200),
            declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
            ..AttributionInput::default()
        });
        assert_eq!(replaced.repository_bindings.len(), 1);
        assert_eq!(worker.full_certification_probe_count(), 2);
        assert_eq!(
            replaced.repository_bindings[0]
                .local_root_authorization
                .as_ref()
                .unwrap()
                .observed_at_unix_ms,
            200
        );
    }
}

#[cfg(unix)]
#[test]
fn source_boundary_retains_certification_cache_but_clears_event_history() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    let mut attributor = RepositoryAttributor::default();
    let input = || AttributionInput {
        activity_at_unix_ms: Some(100),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    };

    assert_eq!(attributor.attribute(input()).repository_bindings.len(), 1);
    assert_eq!(attributor.full_certification_probe_count(), 1);
    assert_eq!(attributor.event_time_cache_len(), 1);

    attributor.begin_source();
    assert_eq!(attributor.event_time_cache_len(), 0);
    assert_eq!(attributor.attribute(input()).repository_bindings.len(), 1);
    assert_eq!(attributor.full_certification_probe_count(), 1);
    assert_eq!(attributor.event_time_cache_len(), 1);
}

#[cfg(unix)]
#[test]
fn provider_activity_time_is_exact_on_probe_and_cache_reuse() {
    use std::cmp::Ordering;

    let temp = TempDir::new().unwrap();
    let old = repository(temp.path(), "old", None);
    let mut attributor = RepositoryAttributor::default();
    let older = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(100),
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    let cached = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(150),
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(attributor.full_certification_probe_count(), 1);
    assert_eq!(
        cached.repository_bindings[0]
            .local_root_authorization
            .as_ref()
            .unwrap()
            .observed_at_unix_ms,
        150
    );

    let moved = temp.path().join("moved");
    fs::rename(&old, &moved).unwrap();
    let newer = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(200),
        declared_tool_workdir: Some(moved.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    let older_root = older.repository_bindings[0]
        .local_root_authorization
        .as_ref()
        .unwrap();
    let newer_root = newer.repository_bindings[0]
        .local_root_authorization
        .as_ref()
        .unwrap();
    assert_eq!(
        newer_root.provider_activity_order(older_root),
        Some(Ordering::Greater)
    );
    assert_eq!(
        older_root.provider_activity_order(newer_root),
        Some(Ordering::Less)
    );
    let mut same_time = newer_root.clone();
    same_time.local_root = "/different/root".to_owned();
    assert_eq!(newer_root.provider_activity_order(&same_time), None);

    let missing_time_a = attribute(AttributionInput {
        declared_tool_workdir: Some(moved.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    let missing_time_b = attribute(AttributionInput {
        declared_tool_workdir: Some(moved.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(missing_time_a, missing_time_b);
    let missing_a = missing_time_a.repository_bindings[0]
        .local_root_authorization
        .as_ref()
        .unwrap();
    let missing_b = missing_time_b.repository_bindings[0]
        .local_root_authorization
        .as_ref()
        .unwrap();
    assert_eq!(
        missing_a.observed_at_unix_ms,
        ctx_history_core::CORE_MISSING_ACTIVITY_TIME_UNIX_MS
    );
    assert_eq!(missing_a.provider_activity_order(missing_b), None);
}

#[cfg(unix)]
#[test]
fn symlink_deep_path_drift_timeout_and_output_bounds_fail_closed() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    let link = temp.path().join("link");
    symlink(&repo, &link).unwrap();
    let unsafe_link = attribute(AttributionInput {
        declared_tool_workdir: Some(link.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert!(has_reason(
        &unsafe_link,
        RepositoryAbstentionReason::UnsafePath
    ));

    let deep = format!("/{}", vec!["x"; 65].join("/"));
    let deep_result = attribute(AttributionInput {
        declared_tool_workdir: Some(deep),
        ..AttributionInput::default()
    });
    assert!(has_reason(
        &deep_result,
        RepositoryAbstentionReason::UnsafePath
    ));

    let certifier = GitCertifier::default();
    let drift = certifier.certify_with_between_probe(
        &repo,
        CandidateKind::Directory,
        RepositoryEvidenceKind::DeclaredToolWorkdir,
        || {
            run_git(
                &repo,
                &[
                    "remote",
                    "add",
                    "later",
                    "https://github.com/acme/later.git",
                ],
            );
        },
    );
    assert!(matches!(drift, Err(ProbeFailure::ConcurrentDrift)));

    for (name, body, expected, timeout) in [
        (
            "slow-git",
            "#!/bin/sh\nsleep 1\n",
            "git_timeout",
            Duration::from_millis(30),
        ),
        (
            "loud-git",
            "#!/bin/sh\n/usr/bin/head -c 70000 /dev/zero | /usr/bin/tr '\\0' x\n",
            "git_output_limit_exceeded",
            Duration::from_secs(2),
        ),
    ] {
        let script = temp.path().join(name);
        fs::write(&script, body).unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&script, permissions).unwrap();
        let certifier = GitCertifier::for_test(&script, timeout);
        let result = certifier.certify(
            &repo,
            CandidateKind::Directory,
            RepositoryEvidenceKind::DeclaredToolWorkdir,
        );
        assert!(matches!(result, Err(ProbeFailure::Failed(detail)) if detail == expected));
    }

    let too_large = attribute(AttributionInput {
        command: Some("x".repeat(shell::MAX_COMMAND_BYTES + 1)),
        ..AttributionInput::default()
    });
    assert!(has_reason(
        &too_large,
        RepositoryAbstentionReason::CommandTooLarge
    ));
}
