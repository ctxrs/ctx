#[cfg(unix)]
use std::time::Duration;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use ctx_history_core::{
    GitObjectFormat, GitObjectId, RepositoryAbstentionReason, RepositoryAlias, RepositoryAliasKind,
    RepositoryCandidateKind, RepositoryEvidenceKind, RepositoryFileInvocationKind,
    RepositoryFileInvocationTextRange, RepositoryFileObservationKind, RepositoryOutcomeKind,
    RepositoryOutcomeLinkage, RepositoryOutcomeObservation, RepositoryPullRequestIdentity,
    RepositoryVcsObservationKind, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
};
use tempfile::TempDir;

#[cfg(unix)]
use super::git::ProbeFailure;
use super::{
    attribute,
    git::{CandidateKind, GitCertifier},
    linked_outcome_evidence, AttributionInput, LinkedOutcomeInput, RepositoryAttributor,
    UnscopedFileObservation, UnscopedRepositoryFileInvocationEvidence, UnscopedVcsObservation,
};
use crate::OutputOutcome;

mod certification;
#[cfg(unix)]
mod local_root_authorization;
mod outcome_parser;
mod pull_request_association;

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

fn git_output(path: &Path, arguments: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {:?} failed", arguments);
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[cfg(unix)]
fn loose_object_path(repository: &Path, oid: &str) -> PathBuf {
    repository
        .join(".git/objects")
        .join(&oid[..2])
        .join(&oid[2..])
}

#[cfg(unix)]
fn shell_quote_path(path: &Path) -> String {
    format!(
        "'{}'",
        path.to_str()
            .expect("test fixture paths must be UTF-8")
            .replace('\'', "'\"'\"'")
    )
}

#[cfg(unix)]
fn delegating_git_with_object_mutation(
    fixture_root: &Path,
    target: &Path,
    replacement: Option<&Path>,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let wrapper = fixture_root.join("delegating-git");
    let first_read_marker = fixture_root.join("mapped-object-first-read");
    let target = shell_quote_path(target);
    let mutation = replacement.map_or_else(
        || format!("        /usr/bin/rm -- {target}\n"),
        |replacement| {
            let replacement = shell_quote_path(replacement);
            format!(
                "        /usr/bin/rm -- {target}\n        /usr/bin/cp -- {replacement} {target}\n"
            )
        },
    );
    let body = format!(
        "#!/bin/sh\n\
         saw_show=0\n\
         saw_exact_format=0\n\
         for argument in \"$@\"; do\n\
             case \"$argument\" in\n\
                 show) saw_show=1 ;;\n\
                 --format=%H) saw_exact_format=1 ;;\n\
             esac\n\
         done\n\
         /usr/bin/git \"$@\"\n\
         status=$?\n\
         if [ \"$status\" -eq 0 ] && [ \"$saw_show\" -eq 1 ] && [ \"$saw_exact_format\" -eq 1 ]; then\n\
             if /usr/bin/mkdir {} 2>/dev/null; then\n\
         {}\
             fi\n\
         fi\n\
         exit \"$status\"\n",
        shell_quote_path(&first_read_marker),
        mutation,
    );
    fs::write(&wrapper, body).unwrap();
    let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&wrapper, permissions).unwrap();
    wrapper
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

fn sha256_repository(parent: &Path, name: &str) -> Option<PathBuf> {
    let path = parent.join(name);
    fs::create_dir(&path).unwrap();
    let initialized = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(&path)
        .args(["init", "-q", "--object-format=sha256"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    if !initialized.success() {
        return None;
    }
    run_git(&path, &["config", "user.name", "ctx test"]);
    run_git(&path, &["config", "user.email", "ctx@example.invalid"]);
    fs::write(path.join("tracked.txt"), "tracked\n").unwrap();
    run_git(&path, &["add", "tracked.txt"]);
    run_git(&path, &["commit", "-qm", "fixture"]);
    Some(path)
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

fn candidate_paths(
    annotation: &ctx_history_core::CoreRecordAnnotation,
    kind: RepositoryCandidateKind,
) -> Vec<&str> {
    annotation
        .repository_candidate_evidence
        .paths(kind)
        .collect()
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
        candidate_paths(&annotation, RepositoryCandidateKind::SessionCwd),
        vec![control.to_string_lossy().as_ref()]
    );
}

#[test]
fn multi_repository_candidate_evidence_is_complete_and_input_order_independent() {
    let temp = TempDir::new().unwrap();
    let first = repository(temp.path(), "candidate-first", None);
    let second = repository(temp.path(), "candidate-second", None);
    let observations = [
        UnscopedFileObservation {
            path: first.join("tracked.txt").to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Read,
        },
        UnscopedFileObservation {
            path: second.join("tracked.txt").to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        },
    ];
    let forward = attribute(AttributionInput {
        file_observations: observations.to_vec(),
        ..AttributionInput::default()
    });
    let reverse = attribute(AttributionInput {
        file_observations: observations.into_iter().rev().collect(),
        ..AttributionInput::default()
    });

    assert_eq!(
        forward.repository_candidate_evidence,
        reverse.repository_candidate_evidence
    );
    let mut expected = [
        first.join("tracked.txt").to_string_lossy().into_owned(),
        second.join("tracked.txt").to_string_lossy().into_owned(),
    ];
    expected.sort();
    assert_eq!(
        candidate_paths(&forward, RepositoryCandidateKind::FileActivityPath),
        expected.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!(forward.repository_bindings.len(), 2);
    assert_eq!(forward.repository_file_observations.len(), 2);
}

#[test]
fn exact_file_invocations_scope_and_canonicalize_without_observation_promotion() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "provider-intent",
        Some("https://github.com/acme/provider-intent.git"),
    );
    let read = UnscopedRepositoryFileInvocationEvidence {
        operation_ordinal: 2,
        path: repo.join("tracked.txt").to_string_lossy().into_owned(),
        prior_path: None,
        kind: RepositoryFileInvocationKind::Read,
        tool_name: Some("read_file".to_owned()),
        normalized_text_range: Some(RepositoryFileInvocationTextRange { start: 5, end: 12 }),
    };
    let write = UnscopedRepositoryFileInvocationEvidence {
        operation_ordinal: 1,
        path: "src/lib.rs".to_owned(),
        prior_path: None,
        kind: RepositoryFileInvocationKind::Write,
        tool_name: Some("write_file".to_owned()),
        normalized_text_range: None,
    };
    let rename = UnscopedRepositoryFileInvocationEvidence {
        operation_ordinal: 0,
        path: "src/new.rs".to_owned(),
        prior_path: Some("src/old.rs".to_owned()),
        kind: RepositoryFileInvocationKind::Rename,
        tool_name: Some("rename_file".to_owned()),
        normalized_text_range: None,
    };
    let annotation = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        structured_content: Some(serde_json::json!({
            "recursive": {"path": "invented.rs", "action": "delete"}
        })),
        repository_file_invocation_evidence: vec![read.clone(), write, rename, read],
        file_observations: vec![UnscopedFileObservation {
            path: "observed-only.rs".to_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Unknown,
        }],
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_file_invocation_evidence.len(), 3);
    assert_eq!(
        annotation
            .repository_file_invocation_evidence
            .iter()
            .map(|evidence| evidence.operation_ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        annotation.repository_file_invocation_evidence[0]
            .prior_relative_path
            .as_deref(),
        Some("src/old.rs")
    );
    let read = &annotation.repository_file_invocation_evidence[2];
    assert_eq!(read.relative_path, "tracked.txt");
    assert_eq!(read.kind, RepositoryFileInvocationKind::Read);
    assert_eq!(read.tool_name.as_deref(), Some("read_file"));
    assert_eq!(
        read.normalized_text_range,
        Some(RepositoryFileInvocationTextRange { start: 5, end: 12 })
    );
    assert_eq!(annotation.repository_file_observations.len(), 1);
    assert!(annotation
        .repository_file_invocation_evidence
        .iter()
        .all(|evidence| evidence.relative_path != "observed-only.rs"
            && evidence.relative_path != "invented.rs"));
}

#[test]
fn generic_observations_and_structured_json_never_create_invocation_evidence() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "generic-only", None);
    let annotation = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        structured_content: Some(serde_json::json!({
            "tool": {"name": "read_file", "path": "tracked.txt", "operation_ordinal": 0}
        })),
        file_observations: vec![UnscopedFileObservation {
            path: "tracked.txt".to_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Read,
        }],
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_file_observations.len(), 1);
    assert!(annotation.repository_file_invocation_evidence.is_empty());
}

#[test]
fn invocation_intent_never_asserts_a_file_effect_observation() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "intent-not-effect", None);
    let annotation = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        repository_file_invocation_evidence: vec![UnscopedRepositoryFileInvocationEvidence {
            operation_ordinal: 0,
            path: "src/new.rs".to_owned(),
            prior_path: None,
            kind: RepositoryFileInvocationKind::Create,
            tool_name: Some("create_file".to_owned()),
            normalized_text_range: None,
        }],
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_file_invocation_evidence.len(), 1);
    assert!(annotation.repository_file_observations.is_empty());
}

#[test]
fn invocation_rename_requires_both_paths_in_one_certified_repository() {
    let temp = TempDir::new().unwrap();
    let first = repository(temp.path(), "rename-first", None);
    let second = repository(temp.path(), "rename-second", None);
    let annotation = attribute(AttributionInput {
        repository_file_invocation_evidence: vec![UnscopedRepositoryFileInvocationEvidence {
            operation_ordinal: 7,
            path: first.join("tracked.txt").to_string_lossy().into_owned(),
            prior_path: Some(second.join("tracked.txt").to_string_lossy().into_owned()),
            kind: RepositoryFileInvocationKind::Rename,
            tool_name: Some("rename_file".to_owned()),
            normalized_text_range: None,
        }],
        ..AttributionInput::default()
    });

    assert!(annotation.repository_file_invocation_evidence.is_empty());
    assert!(has_reason(
        &annotation,
        RepositoryAbstentionReason::UnscopedFileActivity
    ));
}

#[test]
fn invalid_invocation_path_fails_closed_without_session_repository_fallback() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "invocation-fallback", None);
    let annotation = attribute(AttributionInput {
        session_cwd: Some(repo.to_string_lossy().into_owned()),
        repository_file_invocation_evidence: vec![UnscopedRepositoryFileInvocationEvidence {
            operation_ordinal: 0,
            path: "$DYNAMIC/file.rs".to_owned(),
            prior_path: None,
            kind: RepositoryFileInvocationKind::Read,
            tool_name: Some("read_file".to_owned()),
            normalized_text_range: None,
        }],
        ..AttributionInput::default()
    });

    assert!(annotation.repository_bindings.is_empty());
    assert!(annotation.repository_file_invocation_evidence.is_empty());
    assert!(has_reason(
        &annotation,
        RepositoryAbstentionReason::UnscopedFileActivity
    ));
}

#[test]
fn duplicate_invocation_and_observation_paths_share_one_candidate_slot() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "mixed-duplicate-ceiling", None);
    let mut file_observations = Vec::new();
    for index in 0..32 {
        let path = repo.join(format!("candidate-{index}.txt"));
        fs::write(&path, "candidate\n").unwrap();
        file_observations.push(UnscopedFileObservation {
            path: path.to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        });
    }
    let annotation = attribute(AttributionInput {
        repository_file_invocation_evidence: vec![UnscopedRepositoryFileInvocationEvidence {
            operation_ordinal: 0,
            path: file_observations[0].path.clone(),
            prior_path: None,
            kind: RepositoryFileInvocationKind::Modify,
            tool_name: Some("apply_patch".to_owned()),
            normalized_text_range: None,
        }],
        file_observations,
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_bindings.len(), 1);
    assert_eq!(annotation.repository_file_observations.len(), 32);
    assert_eq!(annotation.repository_file_invocation_evidence.len(), 1);
    assert!(!has_reason(
        &annotation,
        RepositoryAbstentionReason::CandidateLimitExceeded
    ));
}

#[test]
fn strict_candidate_overflow_preserves_admissible_ordinary_attribution() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "strict-overflow-ordinary", None);
    let repository_file_invocation_evidence = (0..33)
        .map(|operation_ordinal| {
            let path = repo.join(format!("strict-{operation_ordinal}.txt"));
            fs::write(&path, "strict\n").unwrap();
            UnscopedRepositoryFileInvocationEvidence {
                operation_ordinal,
                path: path.to_string_lossy().into_owned(),
                prior_path: None,
                kind: RepositoryFileInvocationKind::Modify,
                tool_name: Some("apply_patch".to_owned()),
                normalized_text_range: None,
            }
        })
        .collect();
    let annotation = attribute(AttributionInput {
        repository_file_invocation_evidence,
        file_observations: vec![UnscopedFileObservation {
            path: repo.join("tracked.txt").to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        }],
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_bindings.len(), 1);
    assert_eq!(annotation.repository_file_observations.len(), 1);
    assert!(annotation.repository_file_invocation_evidence.is_empty());
    assert!(has_reason(
        &annotation,
        RepositoryAbstentionReason::CandidateLimitExceeded
    ));
}

#[test]
fn mixed_candidate_channels_are_admitted_at_the_shared_ceiling() {
    let temp = TempDir::new().unwrap();
    let repo = repository(temp.path(), "mixed-channel-ceiling", None);
    let command_dir = repo.join("command-route");
    fs::create_dir(&command_dir).unwrap();
    let mut vcs_observations = Vec::new();
    for index in 0..10 {
        let path = repo.join(format!("vcs-route-{index}"));
        fs::create_dir(&path).unwrap();
        vcs_observations.push(UnscopedVcsObservation {
            path: Some(path.to_string_lossy().into_owned()),
            kind: RepositoryVcsObservationKind::Commit,
            object_id: None,
            parent_object_ids: Vec::new(),
            reference: None,
        });
    }
    let mut file_observations = Vec::new();
    for index in 0..10 {
        let path = repo.join(format!("ordinary-{index}.txt"));
        fs::write(&path, "ordinary\n").unwrap();
        file_observations.push(UnscopedFileObservation {
            path: path.to_string_lossy().into_owned(),
            prior_path: None,
            kind: RepositoryFileObservationKind::Modified,
        });
    }
    let repository_file_invocation_evidence = (0..11)
        .map(|operation_ordinal| {
            let path = repo.join(format!("strict-{operation_ordinal}.txt"));
            fs::write(&path, "strict\n").unwrap();
            UnscopedRepositoryFileInvocationEvidence {
                operation_ordinal,
                path: path.to_string_lossy().into_owned(),
                prior_path: None,
                kind: RepositoryFileInvocationKind::Modify,
                tool_name: Some("apply_patch".to_owned()),
                normalized_text_range: None,
            }
        })
        .collect();
    let annotation = attribute(AttributionInput {
        command: Some(format!("git -C {} status", command_dir.display())),
        repository_file_invocation_evidence,
        file_observations,
        vcs_observations,
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_bindings.len(), 1);
    assert_eq!(annotation.repository_file_observations.len(), 10);
    assert_eq!(annotation.repository_vcs_observations.len(), 10);
    assert_eq!(annotation.repository_file_invocation_evidence.len(), 11);
    assert!(!has_reason(
        &annotation,
        RepositoryAbstentionReason::CandidateLimitExceeded
    ));
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
        candidate_paths(&relative, RepositoryCandidateKind::DerivedEffectiveCwd),
        vec![first.to_string_lossy().as_ref()]
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
    assert_eq!(
        candidate_paths(
            &repeated,
            RepositoryCandidateKind::CommandSpecificRepositoryPath
        ),
        vec![
            first.to_string_lossy().as_ref(),
            second.to_string_lossy().as_ref()
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
        candidate_paths(&annotation, RepositoryCandidateKind::DeclaredToolWorkdir),
        vec![workdir.to_string_lossy().as_ref()]
    );
    assert_eq!(
        candidate_paths(
            &annotation,
            RepositoryCandidateKind::CommandSpecificRepositoryPath
        ),
        vec![command_repo.to_string_lossy().as_ref()]
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
            candidate_paths(&annotation, RepositoryCandidateKind::DerivedEffectiveCwd),
            vec![first.to_string_lossy().as_ref()]
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

#[test]
fn conflicting_provider_identity_preserves_independent_multi_repository_evidence() {
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
        provider_native_repository_aliases: vec![forge("acme", "provider")],
        session_cwd: Some(session.to_string_lossy().into_owned()),
        declared_tool_workdir: Some(workdir.to_string_lossy().into_owned()),
        command: Some(format!("git -C {} status", workdir.to_string_lossy())),
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
            reference: None,
        }],
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_bindings.len(), 3);
    assert!(annotation.repository_bindings.iter().any(|binding| {
        binding.logical_repository_id == "forge:github.com/acme/provider"
            && binding.local_root_authorization.is_none()
    }));
    assert!(annotation.repository_bindings.iter().any(|binding| {
        binding.logical_repository_id == "forge:github.com/acme/workdir"
            && binding
                .evidence
                .iter()
                .any(|evidence| evidence.kind == RepositoryEvidenceKind::DeclaredToolWorkdir)
            && binding.evidence.iter().any(|evidence| {
                evidence.kind == RepositoryEvidenceKind::CommandSpecificRepositoryPath
            })
    }));
    let activity_binding = annotation
        .repository_bindings
        .iter()
        .find(|binding| binding.logical_repository_id == "forge:github.com/acme/activity")
        .unwrap();
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
        RepositoryAbstentionReason::ConflictingIdentity
    ));
}

include!("tests/outcomes.rs");

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
fn fork_and_upstream_remain_distinct_logical_repositories() {
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
        provider_native_repository_aliases: vec![forge("other", "repo")],
        outcome_observations: vec![exact_pull_request_outcome(forge("other", "repo")).into()],
        ..AttributionInput::default()
    });
    assert_eq!(annotation.repository_bindings.len(), 2);
    let binding = annotation
        .repository_bindings
        .iter()
        .find(|binding| binding.logical_repository_id == "forge:github.com/acme/repo")
        .expect("origin fork binding");
    assert_eq!(binding.aliases.len(), 2);
    assert!(
        binding
            .aliases
            .iter()
            .any(|alias| alias.namespace == ["acme"]
                && alias.remote_name.as_deref() == Some("origin"))
    );
    assert!(binding.aliases.iter().any(|alias| {
        alias.namespace == ["other"] && alias.remote_name.as_deref() == Some("upstream")
    }));
    let upstream = annotation
        .repository_bindings
        .iter()
        .find(|binding| binding.logical_repository_id == "forge:github.com/other/repo")
        .expect("provider-native upstream binding");
    assert!(upstream
        .aliases
        .iter()
        .any(|alias| { alias.namespace == ["other"] && alias.remote_name.is_none() }));
    assert!(upstream
        .evidence
        .iter()
        .any(|evidence| { evidence.kind == RepositoryEvidenceKind::ProviderNativeProject }));
    assert_eq!(annotation.repository_vcs_observations.len(), 1);
    assert_eq!(
        annotation.repository_vcs_observations[0].repository_binding_id,
        upstream.binding_id
    );
    assert!(!has_reason(
        &annotation,
        RepositoryAbstentionReason::AmbiguousRemote
    ));
    assert!(!has_reason(
        &annotation,
        RepositoryAbstentionReason::OutcomeRepositoryUnbound
    ));
    assert!(!has_reason(
        &annotation,
        RepositoryAbstentionReason::ConflictingIdentity
    ));
}

#[test]
fn origin_authority_survives_secondary_remote_addition_and_config_reordering() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "repo",
        Some("https://github.com/acme/repo.git"),
    );
    let before = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    run_git(
        &repo,
        &[
            "remote",
            "add",
            "internal",
            "https://github.com/acme/repo-internal.git",
        ],
    );
    let added = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    run_git(&repo, &["remote", "remove", "origin"]);
    run_git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/repo.git",
        ],
    );
    let reordered = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });

    for annotation in [&before, &added, &reordered] {
        assert_eq!(annotation.repository_bindings.len(), 1);
        assert_eq!(
            annotation.repository_bindings[0].logical_repository_id,
            "forge:github.com/acme/repo"
        );
    }
    assert_eq!(
        before.repository_bindings[0].binding_id,
        added.repository_bindings[0].binding_id
    );
    assert_eq!(
        added.repository_bindings[0].binding_id,
        reordered.repository_bindings[0].binding_id
    );
}

#[test]
fn github_gitlab_and_custom_ports_have_canonical_distinct_authority() {
    let temp = TempDir::new().unwrap();
    let cases = [
        (
            "github",
            "https://GitHub.com:443/acme/repo.git",
            "forge:github.com/acme/repo",
        ),
        (
            "gitlab",
            "git@gitlab.com:group/subgroup/repo.git",
            "forge:gitlab.com/group/subgroup/repo",
        ),
        (
            "custom-default-ssh",
            "ssh://git@forge.example.test:22/acme/repo.git",
            "forge:forge.example.test/acme/repo",
        ),
        (
            "custom-port",
            "ssh://git@forge.example.test:2222/acme/repo.git",
            "forge:forge.example.test:2222/acme/repo",
        ),
    ];
    for (name, remote, expected) in cases {
        let repo = repository(temp.path(), name, Some(remote));
        let annotation = attribute(AttributionInput {
            declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
            ..AttributionInput::default()
        });
        assert_eq!(annotation.repository_bindings.len(), 1, "{name}");
        assert_eq!(
            annotation.repository_bindings[0].logical_repository_id, expected,
            "{name}"
        );
    }
}

#[test]
fn multiple_remote_names_for_one_repository_keep_one_forge_identity() {
    let temp = TempDir::new().unwrap();
    let repo = repository(
        temp.path(),
        "repo",
        Some("https://github.com/acme/repo.git"),
    );
    run_git(
        &repo,
        &["remote", "add", "backup", "git@github.com:acme/repo.git"],
    );

    let annotation = attribute(AttributionInput {
        declared_tool_workdir: Some(repo.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });

    assert_eq!(annotation.repository_bindings.len(), 1);
    let binding = &annotation.repository_bindings[0];
    assert_eq!(binding.logical_repository_id, "forge:github.com/acme/repo");
    assert_eq!(binding.aliases.len(), 2);
    assert!(binding
        .aliases
        .iter()
        .any(|alias| alias.remote_name.as_deref() == Some("origin")));
    assert!(binding
        .aliases
        .iter()
        .any(|alias| alias.remote_name.as_deref() == Some("backup")));
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

    let no_session_fallback = attribute(AttributionInput {
        provider_native_repository_aliases: vec![forge("acme", "one"), forge("acme", "two")],
        session_cwd: Some(repository.to_string_lossy().into_owned()),
        ..AttributionInput::default()
    });
    assert!(no_session_fallback.repository_bindings.is_empty());
    assert!(has_reason(
        &no_session_fallback,
        RepositoryAbstentionReason::ConflictingIdentity
    ));
}

#[test]
fn provider_prebounded_oversized_command_blocks_session_cwd_fallback() {
    let temp = TempDir::new().unwrap();
    let repository = repository(temp.path(), "session", None);
    let annotation = attribute(AttributionInput {
        session_cwd: Some(repository.to_string_lossy().into_owned()),
        command_disposition: super::CommandEvidenceDisposition::CommandTooLarge,
        ..AttributionInput::default()
    });

    assert!(annotation.repository_bindings.is_empty());
    assert!(has_reason(
        &annotation,
        RepositoryAbstentionReason::CommandTooLarge
    ));
}
