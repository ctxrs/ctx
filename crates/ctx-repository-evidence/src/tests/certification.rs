use super::super::{git, shell};
use super::*;

#[test]
fn candidate_products_abstain_before_any_git_probe() {
    let file_observations = (0..33)
        .map(|index| LiteralFileObservation {
            path: format!("/definitely-missing/ctx-candidate-{index}"),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        })
        .collect();
    let files = evaluate(NeutralRepositoryFacts {
        file_observations,
        ..NeutralRepositoryFacts::default()
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
    let commands = evaluate(NeutralRepositoryFacts {
        command: Some(command),
        ..NeutralRepositoryFacts::default()
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
    let independent = evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(workdir.to_string_lossy().into_owned()),
        command: Some(command),
        file_observations: vec![LiteralFileObservation {
            path: activity.join("tracked.txt").to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        }],
        ..NeutralRepositoryFacts::default()
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

    let annotation = evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
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
    let mut resolver = RepositoryEvidenceResolver::default();
    let annotation = resolver.evaluate(NeutralRepositoryFacts {
        command: Some(command),
        ..NeutralRepositoryFacts::default()
    });

    assert_eq!(annotation.repository_bindings.len(), 2);
    assert!(has_reason(
        &annotation,
        RepositoryAbstentionReason::ProbeBudgetExceeded
    ));
    assert_eq!(
        resolver.full_certification_probe_count(),
        git::MAX_FULL_CERTIFICATIONS_PER_EVENT
    );
    assert!(resolver.git_subprocess_count() > 0);
    assert!(resolver.git_subprocess_count() <= git::MAX_GIT_SUBPROCESSES_PER_EVENT);
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
    assert_eq!(certifier.git_subprocess_count(), subprocesses_before + 2);

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
    assert_eq!(certifier.git_subprocess_count(), subprocesses_before + 2);
}

#[cfg(unix)]
#[test]
fn mapped_object_removed_after_first_operation_read_fails_recertification() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "mapped-object-removal", None);
    let removed_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("next.txt"), "next\n").unwrap();
    run_git(&repo, &["add", "next.txt"]);
    run_git(&repo, &["commit", "-qm", "next mapped object"]);
    let next_oid = git_output(&repo, &["rev-parse", "HEAD"]);
    let mut object_ids = [removed_oid.clone(), next_oid]
        .into_iter()
        .map(|hex| GitObjectId {
            format: GitObjectFormat::Sha1,
            hex,
        })
        .collect::<Vec<_>>();
    object_ids.sort();

    let removed_object = loose_object_path(&repo, &removed_oid);
    assert!(removed_object.is_file());
    let certifier = GitCertifier::for_test(
        delegating_git_with_object_mutation(temp.path(), &removed_object, None),
        Duration::from_secs(2),
    );
    let certificate = certifier
        .certify(
            &repo,
            CandidateKind::Directory,
            RepositoryEvidenceKind::DeclaredToolWorkdir,
        )
        .unwrap();
    let subprocesses_before = certifier.git_subprocess_count();

    let failure = certifier
        .verify_commit_operation_objects(
            &certificate,
            &object_ids,
            &mut git::EventProbeBudget::new(),
        )
        .unwrap_err();

    assert_eq!(failure, ProbeFailure::Failed("git_command_failed"));
    assert_eq!(certifier.git_subprocess_count(), subprocesses_before + 2);
    assert!(!removed_object.exists());
    assert!(temp.path().join("mapped-object-first-read").is_dir());
}

#[cfg(unix)]
#[test]
fn every_candidate_route_is_revalidated_in_both_ancestor_descendant_orders() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    let outside = repository(temp.path(), "outside", None);
    let descendant = repo.join("route");

    let mut ancestor_first = RepositoryEvidenceResolver::default();
    let root = ancestor_first.evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    });
    assert_eq!(root.repository_bindings.len(), 1);
    symlink(&outside, &descendant).unwrap();
    let unsafe_descendant = ancestor_first.evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(descendant.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    });
    assert!(unsafe_descendant.repository_bindings.is_empty());
    assert!(has_reason(
        &unsafe_descendant,
        RepositoryAbstentionReason::UnsafePath
    ));

    let mut descendant_first = RepositoryEvidenceResolver::default();
    let unsafe_descendant = descendant_first.evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(descendant.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    });
    assert!(unsafe_descendant.repository_bindings.is_empty());
    assert!(has_reason(
        &unsafe_descendant,
        RepositoryAbstentionReason::UnsafePath
    ));
    fs::remove_file(&descendant).unwrap();
    fs::create_dir(&descendant).unwrap();
    let safe_root = descendant_first.evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
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
    let mut resolver = RepositoryEvidenceResolver::default();

    for _ in 0..1_000 {
        let annotation = resolver.evaluate(NeutralRepositoryFacts {
            declared_tool_workdir: Some(first.to_string_lossy().into_owned()),
            ..NeutralRepositoryFacts::default()
        });
        assert_eq!(annotation.repository_bindings.len(), 1);
    }
    assert_eq!(resolver.full_certification_probe_count(), 1);

    let file_evidence = resolver.evaluate(NeutralRepositoryFacts {
        file_observations: vec![LiteralFileObservation {
            path: first.join("tracked.txt").to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        }],
        ..NeutralRepositoryFacts::default()
    });
    assert_eq!(resolver.full_certification_probe_count(), 1);
    assert_eq!(
        file_evidence.repository_bindings[0].evidence[0].kind,
        RepositoryEvidenceKind::FileActivity
    );

    for _ in 0..100 {
        for path in [&first, &second] {
            let annotation = resolver.evaluate(NeutralRepositoryFacts {
                declared_tool_workdir: Some(path.to_string_lossy().into_owned()),
                ..NeutralRepositoryFacts::default()
            });
            assert_eq!(annotation.repository_bindings.len(), 1);
        }
    }
    assert_eq!(resolver.full_certification_probe_count(), 2);

    let moved = temp.path().join("moved-first");
    fs::rename(&first, &moved).unwrap();
    let moved_binding = resolver.evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(moved.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    });
    assert_eq!(moved_binding.repository_bindings.len(), 1);
    assert_eq!(resolver.full_certification_probe_count(), 3);

    let route = moved.join("route");
    fs::create_dir(&route).unwrap();
    let safe_route = resolver.evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(route.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    });
    assert_eq!(safe_route.repository_bindings.len(), 1);
    assert_eq!(resolver.full_certification_probe_count(), 3);
    fs::remove_dir(&route).unwrap();
    symlink(&second, &route).unwrap();
    let swapped_route = resolver.evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(route.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    });
    assert!(swapped_route.repository_bindings.is_empty());
    assert!(has_reason(
        &swapped_route,
        RepositoryAbstentionReason::UnsafePath
    ));
    assert_eq!(resolver.full_certification_probe_count(), 3);

    let later_repo = temp.path().join("later-repo");
    for _ in 0..2 {
        let negative = resolver.evaluate(NeutralRepositoryFacts {
            declared_tool_workdir: Some(later_repo.to_string_lossy().into_owned()),
            ..NeutralRepositoryFacts::default()
        });
        assert!(negative.repository_bindings.is_empty());
    }
    assert_eq!(resolver.full_certification_probe_count(), 4);
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
    let discovered = resolver.evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(later_repo.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    });
    assert_eq!(discovered.repository_bindings.len(), 1);
    assert_eq!(resolver.full_certification_probe_count(), 5);
}

#[cfg(unix)]
#[test]
fn unsafe_candidate_is_frozen_only_for_the_current_source() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let repository = repository(temp.path(), "repository", None);
    let candidate = temp.path().join("candidate");
    symlink(&repository, &candidate).unwrap();
    let mut resolver = RepositoryEvidenceResolver::default();
    let input = || NeutralRepositoryFacts {
        declared_tool_workdir: Some(candidate.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    };

    let unsafe_candidate = resolver.evaluate(input());
    assert!(unsafe_candidate.repository_bindings.is_empty());
    assert!(has_reason(
        &unsafe_candidate,
        RepositoryAbstentionReason::UnsafePath
    ));
    assert_eq!(resolver.full_certification_probe_count(), 1);

    fs::remove_file(&candidate).unwrap();
    fs::rename(&repository, &candidate).unwrap();
    for _ in 0..100 {
        let frozen = resolver.evaluate(input());
        assert!(frozen.repository_bindings.is_empty());
        assert!(has_reason(&frozen, RepositoryAbstentionReason::UnsafePath));
    }
    assert_eq!(resolver.full_certification_probe_count(), 1);

    resolver.begin_source();
    let revalidated = resolver.evaluate(input());
    assert_eq!(revalidated.repository_bindings.len(), 1);
    assert_eq!(resolver.full_certification_probe_count(), 2);
}

#[cfg(unix)]
#[test]
fn cached_unsafe_candidate_still_consumes_the_event_probe_budget() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let symlink_target = repository(temp.path(), "symlink-target", None);
    let unsafe_candidate = temp.path().join("unsafe-candidate");
    symlink(&symlink_target, &unsafe_candidate).unwrap();
    let first = repository(temp.path(), "first-certified", None);
    let second = repository(temp.path(), "second-budget-exceeded", None);
    let mut resolver = RepositoryEvidenceResolver::default();

    let initial = resolver.evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(unsafe_candidate.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    });
    assert!(initial.repository_bindings.is_empty());
    assert_eq!(resolver.full_certification_probe_count(), 1);

    let cached = resolver.evaluate(NeutralRepositoryFacts {
        command: Some(format!(
            "git -C {} status && git -C {} status && git -C {} status",
            unsafe_candidate.display(),
            first.display(),
            second.display()
        )),
        ..NeutralRepositoryFacts::default()
    });
    assert_eq!(cached.repository_bindings.len(), 1);
    assert!(has_reason(
        &cached,
        RepositoryAbstentionReason::ProbeBudgetExceeded
    ));
    assert_eq!(resolver.full_certification_probe_count(), 2);
}

#[cfg(unix)]
#[test]
fn independent_worker_caches_revalidate_replaced_git_identity() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    let mut workers = [
        RepositoryEvidenceResolver::default(),
        RepositoryEvidenceResolver::default(),
    ];
    for worker in &mut workers {
        let initial = worker.evaluate(NeutralRepositoryFacts {
            activity_at_unix_ms: Some(100),
            declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
            ..NeutralRepositoryFacts::default()
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
        let replaced = worker.evaluate(NeutralRepositoryFacts {
            activity_at_unix_ms: Some(200),
            declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
            ..NeutralRepositoryFacts::default()
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
    let mut resolver = RepositoryEvidenceResolver::default();
    let input = || NeutralRepositoryFacts {
        activity_at_unix_ms: Some(100),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    };

    assert_eq!(resolver.evaluate(input()).repository_bindings.len(), 1);
    assert_eq!(resolver.full_certification_probe_count(), 1);
    assert_eq!(resolver.event_time_cache_len(), 1);

    resolver.begin_source();
    assert_eq!(resolver.event_time_cache_len(), 0);
    assert_eq!(resolver.evaluate(input()).repository_bindings.len(), 1);
    assert_eq!(resolver.full_certification_probe_count(), 1);
    assert_eq!(resolver.event_time_cache_len(), 1);
}

#[cfg(unix)]
#[test]
fn provider_activity_time_is_exact_on_probe_and_cache_reuse() {
    use std::cmp::Ordering;

    let temp = TempDir::new().unwrap();
    let old = repository(temp.path(), "old", None);
    let mut resolver = RepositoryEvidenceResolver::default();
    let older = resolver.evaluate(NeutralRepositoryFacts {
        activity_at_unix_ms: Some(100),
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    });
    let cached = resolver.evaluate(NeutralRepositoryFacts {
        activity_at_unix_ms: Some(150),
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    });
    assert_eq!(resolver.full_certification_probe_count(), 1);
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
    let newer = resolver.evaluate(NeutralRepositoryFacts {
        activity_at_unix_ms: Some(200),
        declared_tool_workdir: Some(moved.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
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

    let missing_time_a = evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(moved.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    });
    let missing_time_b = evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(moved.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
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
        crate::model::MISSING_ACTIVITY_TIME_UNIX_MS
    );
    assert_eq!(missing_a.provider_activity_order(missing_b), None);
}

#[cfg(unix)]
#[test]
fn symlink_deep_path_drift_timeout_and_output_bounds_fail_closed() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    let link = temp.path().join("link");
    symlink(&repo, &link).unwrap();
    let unsafe_link = evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(link.to_string_lossy().into_owned()),
        ..NeutralRepositoryFacts::default()
    });
    assert!(has_reason(
        &unsafe_link,
        RepositoryAbstentionReason::UnsafePath
    ));

    let deep = format!("/{}", vec!["x"; 65].join("/"));
    let deep_result = evaluate(NeutralRepositoryFacts {
        declared_tool_workdir: Some(deep),
        ..NeutralRepositoryFacts::default()
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
            // This probes bounded-output classification, not a two-second
            // scheduling budget. Allow contention in the surrounding suite
            // without weakening the production Git timeout.
            Duration::from_secs(10),
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

    let too_large = evaluate(NeutralRepositoryFacts {
        command: Some("x".repeat(shell::MAX_COMMAND_BYTES + 1)),
        ..NeutralRepositoryFacts::default()
    });
    assert!(has_reason(
        &too_large,
        RepositoryAbstentionReason::CommandTooLarge
    ));
}
