#[cfg(unix)]
use std::time::Duration;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use ctx_history_core::{
    GitObjectFormat, GitObjectId, RepositoryAbstentionReason, RepositoryAlias, RepositoryAliasKind,
    RepositoryEvidenceKind, RepositoryFileObservationKind, RepositoryOutcomeKind,
    RepositoryOutcomeLinkage, RepositoryOutcomeObservation, RepositoryVcsObservationKind,
    CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
};
#[cfg(unix)]
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[cfg(unix)]
use super::git::ProbeFailure;
use super::{
    attribute,
    git::{CandidateKind, GitCertifier},
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

fn locator_fingerprint(annotation: &ctx_history_core::CoreRecordAnnotation) -> [u8; 32] {
    annotation.repository_bindings[0]
        .local_root_authorization
        .as_ref()
        .unwrap()
        .locator_fingerprint
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
fn structured_workdir_and_git_c_bind_independently_supported_repositories() {
    let temp = TempDir::new().unwrap();
    let workdir = repository(
        temp.path(),
        "workdir",
        Some("https://github.com/acme/workdir.git"),
    );
    let command_repo = repository(
        temp.path(),
        "command",
        Some("https://github.com/acme/command.git"),
    );

    let annotation = attribute(AttributionInput {
        declared_tool_workdir: Some(workdir.to_string_lossy().into_owned()),
        command: Some(format!("git -C {} status", command_repo.display())),
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_bindings.len(), 2);
    let workdir_binding = annotation
        .repository_bindings
        .iter()
        .find(|binding| binding.logical_repository_id == "forge:github.com/acme/workdir")
        .unwrap();
    assert!(workdir_binding
        .evidence
        .iter()
        .any(|evidence| evidence.kind == RepositoryEvidenceKind::DeclaredToolWorkdir));
    let command_binding = annotation
        .repository_bindings
        .iter()
        .find(|binding| binding.logical_repository_id == "forge:github.com/acme/command")
        .unwrap();
    assert!(command_binding.evidence.iter().any(|evidence| {
        evidence.kind == RepositoryEvidenceKind::CommandSpecificRepositoryPath
    }));
    assert_eq!(
        annotation
            .repository_candidate_evidence
            .declared_tool_workdir
            .as_deref(),
        Some(workdir.to_string_lossy().as_ref())
    );
    assert_eq!(
        annotation
            .repository_candidate_evidence
            .command_specific_repository_path
            .as_deref(),
        Some(command_repo.to_string_lossy().as_ref())
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
fn literal_cd_is_preserved_as_candidate_evidence_but_not_opaque_operation_authority() {
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
    assert!(cargo.repository_bindings.is_empty());
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
        assert!(annotation.repository_bindings.is_empty(), "{suffix}");
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
fn opaque_command_suppresses_only_command_and_session_guesses() {
    let temp = TempDir::new().unwrap();
    let session = repository(
        temp.path(),
        "session",
        Some("https://github.com/acme/session.git"),
    );
    let workdir = repository(
        temp.path(),
        "workdir",
        Some("https://github.com/acme/workdir.git"),
    );
    let activity = repository(
        temp.path(),
        "activity",
        Some("https://github.com/acme/activity.git"),
    );
    let annotation = attribute(AttributionInput {
        session_cwd: Some(session.to_string_lossy().into_owned()),
        declared_tool_workdir: Some(workdir.to_string_lossy().into_owned()),
        command: Some("project_alias".to_owned()),
        file_observations: vec![UnscopedFileObservation {
            path: activity.join("tracked.txt").to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        }],
        vcs_observations: vec![UnscopedVcsObservation {
            path: Some(activity.to_string_lossy().into_owned()),
            kind: RepositoryVcsObservationKind::Commit,
            object_id: None,
            parent_object_ids: Vec::new(),
            reference: Some("refs/heads/main".to_owned()),
        }],
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_bindings.len(), 2);
    assert!(annotation
        .repository_bindings
        .iter()
        .all(|binding| binding.logical_repository_id != "forge:github.com/acme/session"));
    let workdir_binding = annotation
        .repository_bindings
        .iter()
        .find(|binding| binding.logical_repository_id == "forge:github.com/acme/workdir")
        .unwrap();
    assert!(workdir_binding
        .evidence
        .iter()
        .any(|evidence| evidence.kind == RepositoryEvidenceKind::DeclaredToolWorkdir));
    let activity_binding = annotation
        .repository_bindings
        .iter()
        .find(|binding| binding.logical_repository_id == "forge:github.com/acme/activity")
        .unwrap();
    assert!(activity_binding
        .evidence
        .iter()
        .any(|evidence| evidence.kind == RepositoryEvidenceKind::FileActivity));
    assert!(activity_binding
        .evidence
        .iter()
        .any(|evidence| evidence.kind == RepositoryEvidenceKind::VcsActivity));
    assert_eq!(annotation.repository_file_observations.len(), 1);
    assert_eq!(annotation.repository_vcs_observations.len(), 1);
    assert_eq!(
        annotation.repository_file_observations[0].repository_binding_id,
        activity_binding.binding_id
    );
    assert_eq!(
        annotation.repository_vcs_observations[0].repository_binding_id,
        activity_binding.binding_id
    );
    assert!(has_reason(
        &annotation,
        RepositoryAbstentionReason::UnknownWrapper
    ));
}

fn exact_commit_outcome() -> RepositoryOutcomeObservation {
    RepositoryOutcomeObservation {
        kind: RepositoryOutcomeKind::Commit,
        produced_object_ids: vec![GitObjectId {
            format: GitObjectFormat::Sha1,
            hex: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        }],
        replacement_lineage: Vec::new(),
        pull_request: None,
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
            outcome_observations: vec![exact_commit_outcome()],
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
            annotation
                .repository_candidate_evidence
                .declared_tool_workdir
                .as_deref(),
            Some(path.as_str())
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
        outcome_observations: vec![exact_commit_outcome()],
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

    let temp = TempDir::new().unwrap();
    let repository = repository(
        temp.path(),
        "structured-repository",
        Some("https://github.com/local/structured-repository.git"),
    );
    let independent = attribute(AttributionInput {
        provider_native_repository_aliases: vec![forge("acme", "one"), forge("acme", "two")],
        declared_tool_workdir: Some(repository.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(independent.repository_bindings.len(), 1);
    assert!(independent.repository_bindings[0]
        .local_root_authorization
        .is_some());
    assert!(has_reason(
        &independent,
        RepositoryAbstentionReason::ConflictingIdentity
    ));
}

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
fn one_event_is_bounded_to_two_full_certificates_and_eight_git_subprocesses() {
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
        super::git::MAX_FULL_CERTIFICATIONS_PER_EVENT
    );
    assert_eq!(
        attributor.git_subprocess_count(),
        super::git::MAX_GIT_SUBPROCESSES_PER_EVENT
    );
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
fn locator_fingerprint_ignores_mutable_git_state_and_path_renames() {
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
    let fingerprint = locator_fingerprint(&initial);

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
    assert_eq!(locator_fingerprint(&changed), fingerprint);

    let renamed = temp.path().join("renamed");
    fs::rename(&repo, &renamed).unwrap();
    let moved = attribute(AttributionInput {
        declared_tool_workdir: Some(renamed.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert_eq!(locator_fingerprint(&moved), fingerprint);
}

#[cfg(unix)]
#[test]
fn locator_fingerprint_matches_the_cross_repository_wire_encoding() {
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
        authorization.locator_fingerprint_revision,
        ctx_history_core::CORE_REPOSITORY_LOCATOR_FINGERPRINT_REVISION
    );

    let git_dir = repo.join(".git");
    let mut digest = Sha256::new();
    digest.update(ctx_history_core::CORE_REPOSITORY_LOCATOR_FINGERPRINT_DOMAIN);
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
    assert_eq!(authorization.locator_fingerprint, expected);
}

#[cfg(unix)]
#[test]
fn locator_fingerprint_changes_for_recreated_root_git_dir_and_linked_worktree() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "repo", None);
    let initial = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    let initial_fingerprint = locator_fingerprint(&initial);

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
    assert_ne!(locator_fingerprint(&linked_binding), initial_fingerprint);

    let old_root = temp.path().join("old-root");
    fs::rename(&repo, &old_root).unwrap();
    fs::create_dir(&repo).unwrap();
    fs::rename(old_root.join(".git"), repo.join(".git")).unwrap();
    fs::copy(old_root.join("tracked.txt"), repo.join("tracked.txt")).unwrap();
    let recreated_root = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    let recreated_root_fingerprint = locator_fingerprint(&recreated_root);
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
        locator_fingerprint(&recreated_git_dir),
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
    assert_ne!(locator_fingerprint(&before), locator_fingerprint(&rebound));
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
    assert_eq!(locator_fingerprint(&before), locator_fingerprint(&after));
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
    fs::create_dir(&later_repo).unwrap();
    for _ in 0..2 {
        let negative = attributor.attribute(AttributionInput {
            declared_tool_workdir: Some(later_repo.to_string_lossy().into_owned()),
            ..AttributionInput::default()
        });
        assert!(negative.repository_bindings.is_empty());
    }
    assert_eq!(attributor.full_certification_probe_count(), 4);
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
        command: Some("x".repeat(super::shell::MAX_COMMAND_BYTES + 1)),
        ..AttributionInput::default()
    });
    assert!(has_reason(
        &too_large,
        RepositoryAbstentionReason::CommandTooLarge
    ));
}
