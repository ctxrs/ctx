use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use ctx_history_core::{
    GitObjectFormat, GitObjectId, RepositoryAbstentionReason, RepositoryAlias, RepositoryAliasKind,
    RepositoryEvidenceKind, RepositoryFileObservationKind, RepositoryVcsObservationKind,
};
use tempfile::TempDir;

use super::{
    attribute,
    git::{CandidateKind, GitCertifier, ProbeFailure},
    AttributionInput, RepositoryAttributor, UnscopedFileObservation, UnscopedVcsObservation,
};

fn run_git(path: &Path, arguments: &[&str]) {
    let status = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "git {:?} failed", arguments);
}

fn repository(parent: &Path, name: &str, remote: Option<&str>) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir(&path).unwrap();
    run_git(&path, &["init", "-q"]);
    run_git(&path, &["config", "user.name", "ctx test"]);
    run_git(&path, &["config", "user.email", "ctx@example.invalid"]);
    if let Some(remote) = remote {
        run_git(&path, &["remote", "add", "origin", remote]);
    }
    fs::write(path.join("tracked.txt"), "tracked\n").unwrap();
    run_git(&path, &["add", "tracked.txt"]);
    run_git(&path, &["commit", "-qm", "fixture"]);
    path
}

fn forge(namespace: &str, name: &str) -> RepositoryAlias {
    RepositoryAlias {
        kind: RepositoryAliasKind::Forge,
        host: "github.com".to_owned(),
        namespace: vec![namespace.to_owned()],
        name: name.to_owned(),
        remote_name: None,
    }
}

fn has_reason(
    annotation: &ctx_history_core::CoreRecordAnnotation,
    reason: RepositoryAbstentionReason,
) -> bool {
    annotation
        .repository_abstentions
        .iter()
        .any(|abstention| abstention.reason == reason)
}

#[test]
fn control_workspace_and_declared_repo_bind_only_the_repo() {
    let temp = TempDir::new().unwrap();
    let control = temp.path().join("control");
    fs::create_dir(&control).unwrap();
    let repo = repository(
        temp.path(),
        "repo",
        Some("https://github.com/acme/repo.git"),
    );
    let direct = GitCertifier::default().certify(
        &repo,
        CandidateKind::Directory,
        RepositoryEvidenceKind::DeclaredToolWorkdir,
    );
    assert!(direct.is_ok(), "direct certification failed: {direct:?}");
    let annotation = attribute(AttributionInput {
        session_cwd: Some(control.to_string_lossy().into_owned()),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        command: Some("git status".to_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(annotation.repository_bindings.len(), 1);
    assert_eq!(
        annotation.repository_bindings[0].logical_repository_id,
        "forge:github.com/acme/repo"
    );
    assert_eq!(
        annotation.repository_bindings[0].evidence[0].kind,
        RepositoryEvidenceKind::DeclaredToolWorkdir
    );
    assert_eq!(
        annotation
            .repository_candidate_evidence
            .session_cwd
            .as_deref(),
        Some(control.to_string_lossy().as_ref())
    );
}

#[test]
fn one_session_can_bind_two_repositories_without_crossing_files() {
    let temp = TempDir::new().unwrap();
    let control = temp.path().join("control");
    fs::create_dir(&control).unwrap();
    let first = repository(
        temp.path(),
        "first",
        Some("https://github.com/acme/first.git"),
    );
    let second = repository(
        temp.path(),
        "second",
        Some("https://github.com/acme/second.git"),
    );
    let mut attributor = RepositoryAttributor::default();
    let first_event = attributor.attribute(AttributionInput {
        session_cwd: Some(control.to_string_lossy().into_owned()),
        declared_tool_workdir: Some(first.to_string_lossy().into_owned()),
        file_observations: vec![UnscopedFileObservation {
            path: "src/a.rs".to_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        }],
        ..AttributionInput::default()
    });
    let second_event = attributor.attribute(AttributionInput {
        session_cwd: Some(control.to_string_lossy().into_owned()),
        declared_tool_workdir: Some(second.to_string_lossy().into_owned()),
        file_observations: vec![UnscopedFileObservation {
            path: "src/b.rs".to_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Created,
        }],
        ..AttributionInput::default()
    });
    assert_ne!(
        first_event.repository_bindings[0].logical_repository_id,
        second_event.repository_bindings[0].logical_repository_id
    );
    assert_eq!(
        first_event.repository_file_observations[0].relative_path,
        "src/a.rs"
    );
    assert_eq!(
        second_event.repository_file_observations[0].relative_path,
        "src/b.rs"
    );
    assert_ne!(
        first_event.repository_file_observations[0].repository_binding_id,
        second_event.repository_file_observations[0].repository_binding_id
    );
}

#[test]
fn relative_absolute_cd_and_repeated_git_c_are_literal_and_multi_repo() {
    let temp = TempDir::new().unwrap();
    let control = temp.path().join("control");
    fs::create_dir(&control).unwrap();
    let first = repository(
        temp.path(),
        "first",
        Some("https://github.com/acme/first.git"),
    );
    let second = repository(
        temp.path(),
        "second",
        Some("https://github.com/acme/second.git"),
    );

    let relative = attribute(AttributionInput {
        session_cwd: Some(control.to_string_lossy().into_owned()),
        command: Some("cd ../first && git status".to_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(relative.repository_bindings.len(), 1);
    assert_eq!(
        relative
            .repository_candidate_evidence
            .derived_effective_cwd
            .as_deref(),
        Some(first.to_string_lossy().as_ref())
    );
    let absolute = attribute(AttributionInput {
        command: Some(format!("cd -- {} && git status", second.display())),
        ..AttributionInput::default()
    });
    assert_eq!(absolute.repository_bindings.len(), 1);

    let repeated = attribute(AttributionInput {
        session_cwd: Some(control.to_string_lossy().into_owned()),
        command: Some("git -C .. -C first status && git -C ../second log".to_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(repeated.repository_bindings.len(), 2);
    let mut logical = repeated
        .repository_bindings
        .iter()
        .map(|binding| binding.logical_repository_id.as_str())
        .collect::<Vec<_>>();
    logical.sort();
    assert_eq!(
        logical,
        [
            "forge:github.com/acme/first",
            "forge:github.com/acme/second"
        ]
    );
}

#[test]
fn exact_wrappers_are_accepted_and_unknown_shapes_abstain() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    for command in [
        "A=1 git status",
        "env -- A=1 git status",
        "command -- git status",
        "time -p git status",
        "timeout 5s git status",
    ] {
        let annotation = attribute(AttributionInput {
            declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
            command: Some(command.to_owned()),
            ..AttributionInput::default()
        });
        assert_eq!(annotation.repository_bindings.len(), 1, "{command}");
    }
    for command in [
        "env -C /tmp git status",
        "command -p git status",
        "time -v git status",
        "timeout --signal KILL 5s git status",
        "timeout 1.2.3s git status",
        "safe-wrap git status",
    ] {
        let annotation = attribute(AttributionInput {
            session_cwd: Some(temp.path().to_string_lossy().into_owned()),
            command: Some(command.to_owned()),
            ..AttributionInput::default()
        });
        assert!(annotation.repository_bindings.is_empty(), "{command}");
        assert!(has_reason(
            &annotation,
            RepositoryAbstentionReason::UnknownWrapper
        ));
    }
}

#[test]
fn literal_cd_survives_opaque_suffix_and_stops_before_later_inference() {
    let temp = TempDir::new().unwrap();
    let control = temp.path().join("control");
    fs::create_dir(&control).unwrap();
    let first = repository(
        temp.path(),
        "first",
        Some("https://github.com/acme/first.git"),
    );
    let _second = repository(
        temp.path(),
        "second",
        Some("https://github.com/acme/second.git"),
    );
    let cargo = attribute(AttributionInput {
        session_cwd: Some(control.to_string_lossy().into_owned()),
        command: Some("cd ../first && cargo test".to_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(cargo.repository_bindings.len(), 1);
    assert_eq!(
        cargo.repository_bindings[0].logical_repository_id,
        "forge:github.com/acme/first"
    );
    assert!(has_reason(
        &cargo,
        RepositoryAbstentionReason::UnknownWrapper
    ));

    for (suffix, reason) in [
        ("my_alias", RepositoryAbstentionReason::UnknownWrapper),
        (
            "bash -lc 'pwd'",
            RepositoryAbstentionReason::ProfileDependent,
        ),
        ("custom $TARGET", RepositoryAbstentionReason::DynamicPath),
    ] {
        let annotation = attribute(AttributionInput {
            session_cwd: Some(control.to_string_lossy().into_owned()),
            command: Some(format!(
                "cd ../first && {suffix} && git -C ../second status"
            )),
            ..AttributionInput::default()
        });
        assert_eq!(annotation.repository_bindings.len(), 1, "{suffix}");
        assert_eq!(
            annotation.repository_bindings[0].logical_repository_id,
            "forge:github.com/acme/first"
        );
        assert!(annotation.repository_bindings[0]
            .evidence
            .iter()
            .any(|evidence| evidence.kind == RepositoryEvidenceKind::DerivedEffectiveCwd));
        assert!(has_reason(&annotation, reason), "{suffix}");
        assert_eq!(
            annotation
                .repository_candidate_evidence
                .derived_effective_cwd
                .as_deref(),
            Some(first.to_string_lossy().as_ref())
        );
    }
}

#[test]
fn class_f_does_not_destroy_independent_workdir_or_file_evidence() {
    let temp = TempDir::new().unwrap();
    let control = temp.path().join("control");
    fs::create_dir(&control).unwrap();
    let repo = repository(temp.path(), "repo", None);

    let workdir = attribute(AttributionInput {
        session_cwd: Some(control.to_string_lossy().into_owned()),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        command: Some("project_alias".to_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(workdir.repository_bindings.len(), 1);
    assert!(workdir.repository_bindings[0]
        .evidence
        .iter()
        .any(|evidence| evidence.kind == RepositoryEvidenceKind::DeclaredToolWorkdir));
    assert!(has_reason(
        &workdir,
        RepositoryAbstentionReason::UnknownWrapper
    ));

    let file = attribute(AttributionInput {
        session_cwd: Some(control.to_string_lossy().into_owned()),
        command: Some("project_function".to_owned()),
        file_observations: vec![UnscopedFileObservation {
            path: repo.join("tracked.txt").to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        }],
        ..AttributionInput::default()
    });
    assert_eq!(file.repository_bindings.len(), 1);
    assert_eq!(file.repository_file_observations.len(), 1);
    assert!(file.repository_bindings[0]
        .evidence
        .iter()
        .any(|evidence| evidence.kind == RepositoryEvidenceKind::FileActivity));
    assert!(has_reason(
        &file,
        RepositoryAbstentionReason::UnknownWrapper
    ));
}

#[test]
fn dynamic_profile_comment_quote_and_heredoc_forms_never_bind() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    let commands = [
        "cd $REPO && git status".to_owned(),
        "cd \"$REPO\" && git status".to_owned(),
        "cd $(pwd) && git status".to_owned(),
        "cd ~/repo && git status".to_owned(),
        "cd ../* && git status".to_owned(),
        "bash -lc 'git -C ../repo status'".to_owned(),
        "source ~/.profile && git -C ../repo status".to_owned(),
        "echo 'git -C ../repo status'".to_owned(),
        "true # git -C ../repo status".to_owned(),
        "python3 - <<'PY'\ngit -C ../repo status\nPY\n".to_owned(),
        "cd 'unterminated && git status".to_owned(),
    ];
    for command in commands {
        let annotation = attribute(AttributionInput {
            session_cwd: Some(temp.path().to_string_lossy().into_owned()),
            command: Some(command.clone()),
            ..AttributionInput::default()
        });
        assert!(annotation.repository_bindings.is_empty(), "{command}");
        assert!(!annotation.repository_abstentions.is_empty(), "{command}");
    }
    assert!(repo.exists());
}

#[test]
fn malformed_or_conflicting_git_c_forms_never_bind() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    for command in [
        "git -C",
        "git -C $REPO status",
        "git --git-dir=/tmp/other status",
    ] {
        let annotation = attribute(AttributionInput {
            session_cwd: Some(repo.to_string_lossy().into_owned()),
            command: Some(command.to_owned()),
            ..AttributionInput::default()
        });
        assert!(annotation.repository_bindings.is_empty(), "{command}");
        assert!(!annotation.repository_abstentions.is_empty(), "{command}");
    }
}

#[test]
fn certification_does_not_synthesize_vcs_activity_but_explicit_activity_survives() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    let certified = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(certified.repository_bindings.len(), 1);
    assert!(certified.repository_vcs_observations.is_empty());

    let object_id = GitObjectId {
        format: GitObjectFormat::Sha1,
        hex: "0123456789abcdef0123456789abcdef01234567".to_owned(),
    };
    let explicit = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        vcs_observations: vec![UnscopedVcsObservation {
            path: Some(repo.to_string_lossy().into_owned()),
            kind: RepositoryVcsObservationKind::Commit,
            object_id: Some(object_id.clone()),
            parent_object_ids: Vec::new(),
            reference: Some("refs/heads/recorded".to_owned()),
        }],
        ..AttributionInput::default()
    });
    assert_eq!(explicit.repository_vcs_observations.len(), 1);
    assert_eq!(
        explicit.repository_vcs_observations[0].object_id,
        Some(object_id)
    );
}

#[test]
fn conflicting_remotes_abstain_without_session_fallback() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "repo",
        Some("https://github.com/acme/repo.git"),
    );
    run_git(
        &repo,
        &[
            "remote",
            "add",
            "upstream",
            "https://github.com/other/repo.git",
        ],
    );
    let annotation = attribute(AttributionInput {
        session_cwd: Some(temp.path().to_string_lossy().into_owned()),
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert!(annotation.repository_bindings.is_empty());
    assert!(has_reason(
        &annotation,
        RepositoryAbstentionReason::AmbiguousRemote
    ));
}

#[test]
fn move_preserves_certified_identity_but_old_path_first_seen_missing_abstains() {
    let temp = TempDir::new().unwrap();
    let old = repository(temp.path(), "old", None);
    let mut attributor = RepositoryAttributor::default();
    let first = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    let new = temp.path().join("new");
    fs::rename(&old, &new).unwrap();
    let moved = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(new.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(
        first.repository_bindings[0].logical_repository_id,
        moved.repository_bindings[0].logical_repository_id
    );
    assert_eq!(
        first.repository_bindings[0].checkout_id,
        moved.repository_bindings[0].checkout_id
    );
    let missing = attributor.attribute(AttributionInput {
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert!(missing.repository_bindings.is_empty());
    assert!(has_reason(
        &missing,
        RepositoryAbstentionReason::CandidateMissingBeforeCertification
    ));
}

#[test]
fn provider_native_identity_survives_missing_or_moved_local_path_without_authorization() {
    let temp = TempDir::new().unwrap();
    let old = repository(temp.path(), "old", Some("https://github.com/acme/repo.git"));
    let alias = forge("acme", "repo");
    let certified = attribute(AttributionInput {
        provider_native_repository_aliases: vec![alias.clone()],
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert!(certified.repository_bindings[0]
        .local_root_authorization
        .is_some());

    let moved = temp.path().join("moved");
    fs::rename(&old, &moved).unwrap();
    let retained = attribute(AttributionInput {
        provider_native_repository_aliases: vec![alias.clone()],
        declared_tool_workdir: Some(old.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(retained.repository_bindings.len(), 1);
    assert_eq!(
        retained.repository_bindings[0].logical_repository_id,
        certified.repository_bindings[0].logical_repository_id
    );
    assert!(retained.repository_bindings[0].checkout_id.is_none());
    assert!(retained.repository_bindings[0]
        .local_root_authorization
        .is_none());
    assert!(!has_reason(
        &retained,
        RepositoryAbstentionReason::CandidateMissingBeforeCertification
    ));

    let never_present = attribute(AttributionInput {
        provider_native_repository_aliases: vec![alias],
        declared_tool_workdir: Some(temp.path().join("never").to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(never_present.repository_bindings.len(), 1);
    assert!(never_present.repository_bindings[0]
        .local_root_authorization
        .is_none());
}

#[test]
fn conflicting_or_unbounded_provider_native_identities_abstain() {
    let conflict = attribute(AttributionInput {
        provider_native_repository_aliases: vec![forge("acme", "one"), forge("acme", "two")],
        ..AttributionInput::default()
    });
    assert!(conflict.repository_bindings.is_empty());
    assert!(has_reason(
        &conflict,
        RepositoryAbstentionReason::ConflictingIdentity
    ));

    let ambiguous = attribute(AttributionInput {
        provider_native_repository_aliases: vec![forge("acme", "repo"); 17],
        ..AttributionInput::default()
    });
    assert!(ambiguous.repository_bindings.is_empty());
    assert!(has_reason(
        &ambiguous,
        RepositoryAbstentionReason::Ambiguous
    ));
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
        command: Some("x".repeat(super::shell::MAX_COMMAND_BYTES + 1)),
        ..AttributionInput::default()
    });
    assert!(has_reason(
        &too_large,
        RepositoryAbstentionReason::CommandTooLarge
    ));
}
