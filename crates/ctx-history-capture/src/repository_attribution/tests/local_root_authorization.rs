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
