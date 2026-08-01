use sha2::{Digest, Sha256};

use super::*;

fn local_root_authorization_fingerprint(
    annotation: &ctx_history_core::CoreRecordAnnotation,
) -> [u8; 32] {
    annotation.repository_bindings[0]
        .local_root_authorization
        .as_ref()
        .unwrap()
        .local_root_authorization_fingerprint
}

#[cfg(unix)]
#[test]
fn local_root_authorization_fingerprint_ignores_mutable_git_state_and_path_renames() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "repo",
        Some("https://github.com/acme/repo.git"),
    );
    let initial = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(
        initial.metadata["repository_association"]["local_root_authorization_fingerprint_revision"],
        ctx_history_core::CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION
    );
    let fingerprint = local_root_authorization_fingerprint(&initial);

    fs::write(repo.join("tracked.txt"), "changed\n").unwrap();
    run_git(&repo, &["add", "tracked.txt"]);
    run_git(&repo, &["commit", "-qm", "mutable state"]);
    run_git(&repo, &["checkout", "-q", "-b", "fingerprint-branch"]);
    run_git(
        &repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/other/renamed.git",
        ],
    );
    let changed = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(local_root_authorization_fingerprint(&changed), fingerprint);

    let renamed = temp.path().join("renamed");
    fs::rename(&repo, &renamed).unwrap();
    let moved = attribute(AttributionInput {
        declared_tool_workdir: Some(renamed.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(local_root_authorization_fingerprint(&moved), fingerprint);
}

#[cfg(unix)]
#[test]
fn local_root_authorization_fingerprint_matches_the_cross_repository_wire_encoding() {
    use std::os::unix::fs::MetadataExt;

    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    let annotation = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    let authorization = annotation.repository_bindings[0]
        .local_root_authorization
        .as_ref()
        .unwrap();
    assert_eq!(
        authorization.local_root_authorization_fingerprint_revision,
        ctx_history_core::CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION
    );

    let git_dir = repo.join(".git");
    let mut digest = Sha256::new();
    digest.update(ctx_history_core::CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN);
    digest.update(1_u16.to_be_bytes());
    for (label, path) in [
        (b"certified_root".as_slice(), repo.as_path()),
        (b"git_dir".as_slice(), git_dir.as_path()),
        (b"common_dir".as_slice(), git_dir.as_path()),
    ] {
        let metadata = fs::symlink_metadata(path).unwrap();
        digest.update([1]);
        digest.update(u64::try_from(label.len()).unwrap().to_be_bytes());
        digest.update(label);
        digest.update(metadata.dev().to_be_bytes());
        digest.update(metadata.ino().to_be_bytes());
    }
    digest.update([4]);
    digest.update(4_u64.to_be_bytes());
    digest.update(b"sha1");
    let expected: [u8; 32] = digest.finalize().into();
    assert_eq!(authorization.local_root_authorization_fingerprint, expected);
}

#[cfg(unix)]
#[test]
fn local_root_authorization_fingerprint_changes_for_recreated_root_git_dir_and_linked_worktree() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    let initial = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    let initial_fingerprint = local_root_authorization_fingerprint(&initial);

    let linked = temp.path().join("linked");
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-branch",
            linked.to_str().unwrap(),
        ],
    );
    let linked_binding = attribute(AttributionInput {
        declared_tool_workdir: Some(linked.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_ne!(
        local_root_authorization_fingerprint(&linked_binding),
        initial_fingerprint
    );

    let old_root = temp.path().join("old-root");
    fs::rename(&repo, &old_root).unwrap();
    fs::create_dir(&repo).unwrap();
    fs::rename(old_root.join(".git"), repo.join(".git")).unwrap();
    fs::copy(old_root.join("tracked.txt"), repo.join("tracked.txt")).unwrap();
    let recreated_root = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    let recreated_root_fingerprint = local_root_authorization_fingerprint(&recreated_root);
    assert_ne!(recreated_root_fingerprint, initial_fingerprint);

    fs::rename(repo.join(".git"), repo.join(".git-old")).unwrap();
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["config", "user.name", "ctx test"]);
    run_git(&repo, &["config", "user.email", "ctx@example.invalid"]);
    run_git(&repo, &["add", "tracked.txt"]);
    run_git(&repo, &["commit", "-qm", "recreated git dir"]);
    let recreated_git_dir = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_ne!(
        local_root_authorization_fingerprint(&recreated_git_dir),
        recreated_root_fingerprint
    );
}

#[cfg(unix)]
#[test]
fn linked_worktree_pointer_rebind_recertifies_local_authorization() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "repo",
        Some("https://github.com/acme/repo.git"),
    );
    let first = temp.path().join("linked-first");
    let second = temp.path().join("linked-second");
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-first",
            first.to_str().unwrap(),
        ],
    );
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-second",
            second.to_str().unwrap(),
        ],
    );

    let mut attributor = RepositoryAttributor::default();
    let before = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(first.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(attributor.full_certification_probe_count(), 1);

    let replacement_pointer = fs::read(second.join(".git")).unwrap();
    let replacement_git_dir = PathBuf::from(
        std::str::from_utf8(&replacement_pointer)
            .unwrap()
            .trim()
            .strip_prefix("gitdir: ")
            .unwrap(),
    );
    fs::write(first.join(".git"), replacement_pointer).unwrap();
    fs::write(
        replacement_git_dir.join("gitdir"),
        format!("{}\n", first.join(".git").display()),
    )
    .unwrap();

    let rebound = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(first.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(rebound.repository_bindings.len(), 1);
    assert_eq!(attributor.full_certification_probe_count(), 2);
    assert_eq!(
        before.repository_bindings[0].logical_repository_id,
        rebound.repository_bindings[0].logical_repository_id
    );
    assert_eq!(
        before.repository_bindings[0].checkout_id,
        rebound.repository_bindings[0].checkout_id
    );
    assert_eq!(
        before.repository_bindings[0].worktree_id,
        rebound.repository_bindings[0].worktree_id
    );
    assert_ne!(
        local_root_authorization_fingerprint(&before),
        local_root_authorization_fingerprint(&rebound)
    );
}

#[cfg(unix)]
#[test]
fn moved_linked_worktree_recertifies_new_root_without_stale_old_authorization() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "repo",
        Some("https://github.com/acme/repo.git"),
    );
    let old = temp.path().join("linked-old");
    let moved = temp.path().join("linked-moved");
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-move",
            old.to_str().unwrap(),
        ],
    );

    let mut attributor = RepositoryAttributor::default();
    let before = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(attributor.full_certification_probe_count(), 1);

    run_git(
        &repo,
        &[
            "worktree",
            "move",
            old.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    let after = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(moved.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(after.repository_bindings.len(), 1);
    assert_eq!(attributor.full_certification_probe_count(), 2);
    assert_eq!(
        before.repository_bindings[0].logical_repository_id,
        after.repository_bindings[0].logical_repository_id
    );
    assert_eq!(
        before.repository_bindings[0].checkout_id,
        after.repository_bindings[0].checkout_id
    );
    assert_eq!(
        before.repository_bindings[0].worktree_id,
        after.repository_bindings[0].worktree_id
    );
    assert_eq!(
        local_root_authorization_fingerprint(&before),
        local_root_authorization_fingerprint(&after)
    );
    assert_eq!(
        after.repository_bindings[0]
            .local_root_authorization
            .as_ref()
            .unwrap()
            .local_root,
        moved.to_string_lossy()
    );

    let reused = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(moved.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(reused.repository_bindings.len(), 1);
    assert_eq!(attributor.full_certification_probe_count(), 2);

    let stale = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert!(stale.repository_bindings.is_empty());
    assert!(has_reason(
        &stale,
        RepositoryAbstentionReason::CandidateMissingBeforeCertification
    ));
}

#[cfg(unix)]
#[test]
fn moved_local_root_reuses_only_exact_prior_event_time_certificates() {
    let temp = TempDir::new().unwrap();
    let neutral = temp.path().join("neutral");
    fs::create_dir(&neutral).unwrap();
    let old = repository(temp.path(), "local-old", None);
    let tracked = old.join("tracked.txt");
    let mut attributor = RepositoryAttributor::default();

    let old_call = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(100),
        session_cwd: Some(neutral.to_string_lossy().into_owned()),
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        command: Some("git status --short".to_owned()),
        ..AttributionInput::default()
    });
    let old_file = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(110),
        session_cwd: Some(neutral.to_string_lossy().into_owned()),
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        file_observations: vec![UnscopedFileObservation {
            path: tracked.to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        }],
        ..AttributionInput::default()
    });
    assert_eq!(old_call.repository_bindings.len(), 1);
    assert_eq!(old_file.repository_file_observations.len(), 1);
    assert_eq!(attributor.full_certification_probe_count(), 1);

    let moved = temp.path().join("local-moved");
    fs::rename(&old, &moved).unwrap();
    let new_call = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(200),
        session_cwd: Some(neutral.to_string_lossy().into_owned()),
        declared_tool_workdir: Some(moved.to_string_lossy().into_owned()),
        command: Some("git status --short".to_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(new_call.repository_bindings.len(), 1);
    assert_eq!(attributor.full_certification_probe_count(), 2);
    for identity in [
        &old_call.repository_bindings[0],
        &old_file.repository_bindings[0],
    ] {
        assert_eq!(
            identity.logical_repository_id,
            new_call.repository_bindings[0].logical_repository_id
        );
        assert_eq!(
            identity.checkout_id,
            new_call.repository_bindings[0].checkout_id
        );
        assert_eq!(
            identity.worktree_id,
            new_call.repository_bindings[0].worktree_id
        );
    }
    assert!(new_call.repository_bindings[0]
        .logical_repository_id
        .starts_with("local:"));
    assert_eq!(
        new_call.repository_bindings[0]
            .local_root_authorization
            .as_ref()
            .unwrap()
            .local_root,
        moved.to_string_lossy()
    );

    let replayed_call = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(100),
        session_cwd: Some(neutral.to_string_lossy().into_owned()),
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        command: Some("git status --short".to_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(replayed_call.repository_bindings.len(), 1);
    assert_eq!(
        replayed_call.repository_bindings[0].binding_id,
        old_call.repository_bindings[0].binding_id
    );
    assert!(replayed_call.repository_bindings[0]
        .local_root_authorization
        .is_none());
    assert_eq!(attributor.full_certification_probe_count(), 2);

    let replayed_file = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(110),
        session_cwd: Some(neutral.to_string_lossy().into_owned()),
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        file_observations: vec![UnscopedFileObservation {
            path: tracked.to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        }],
        ..AttributionInput::default()
    });
    assert_eq!(replayed_file.repository_bindings.len(), 1);
    assert_eq!(replayed_file.repository_file_observations.len(), 1);
    assert_eq!(
        replayed_file.repository_file_observations[0].repository_binding_id,
        old_call.repository_bindings[0].binding_id
    );

    for input in [
        AttributionInput {
            activity_at_unix_ms: Some(150),
            declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
            ..AttributionInput::default()
        },
        AttributionInput {
            activity_at_unix_ms: Some(300),
            declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
            ..AttributionInput::default()
        },
        AttributionInput {
            declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
            ..AttributionInput::default()
        },
    ] {
        let untrusted = attributor.attribute(input);
        assert!(untrusted.repository_bindings.is_empty());
        assert!(has_reason(
            &untrusted,
            RepositoryAbstentionReason::CandidateMissingBeforeCertification
        ));
    }

    let never_seen_file = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(110),
        file_observations: vec![UnscopedFileObservation {
            path: old.join("never-seen.txt").to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        }],
        ..AttributionInput::default()
    });
    assert!(never_seen_file.repository_bindings.is_empty());
    assert!(never_seen_file.repository_file_observations.is_empty());
}

#[cfg(unix)]
#[test]
fn event_time_history_fails_closed_for_identity_conflict_and_live_replacement() {
    let temp = TempDir::new().unwrap();
    let old = repository(temp.path(), "old", None);
    let moved = temp.path().join("moved");
    let mut attributor = RepositoryAttributor::default();
    let original = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(100),
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    fs::rename(&old, &moved).unwrap();
    let relocated = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(200),
        declared_tool_workdir: Some(moved.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(
        original.repository_bindings[0].binding_id,
        relocated.repository_bindings[0].binding_id
    );

    let replacement = repository(temp.path(), "old", None);
    let historical_conflict = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(100),
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert!(historical_conflict.repository_bindings.is_empty());
    assert!(has_reason(
        &historical_conflict,
        RepositoryAbstentionReason::ConflictingIdentity
    ));

    let current_replacement = attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(300),
        declared_tool_workdir: Some(replacement.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(current_replacement.repository_bindings.len(), 1);
    assert_ne!(
        current_replacement.repository_bindings[0].binding_id,
        original.repository_bindings[0].binding_id
    );

    let conflict_old = repository(temp.path(), "conflict-old", None);
    let conflict_moved = temp.path().join("conflict-moved");
    let mut conflicting_attributor = RepositoryAttributor::default();
    let local = conflicting_attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(400),
        declared_tool_workdir: Some(conflict_old.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    fs::rename(&conflict_old, &conflict_moved).unwrap();
    run_git(
        &conflict_moved,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/conflicting-identity.git",
        ],
    );
    let forge = conflicting_attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(500),
        declared_tool_workdir: Some(conflict_moved.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_ne!(
        local.repository_bindings[0].logical_repository_id,
        forge.repository_bindings[0].logical_repository_id
    );
    let unbound_old_phase = conflicting_attributor.attribute(AttributionInput {
        activity_at_unix_ms: Some(400),
        declared_tool_workdir: Some(conflict_old.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert!(unbound_old_phase.repository_bindings.is_empty());
    assert!(has_reason(
        &unbound_old_phase,
        RepositoryAbstentionReason::CandidateMissingBeforeCertification
    ));
}
