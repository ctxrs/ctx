//! Direct Git authority and process-bound tests.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::cell::{Cell, RefCell};

use super::*;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::protocol::{EvidenceCitation, StableEntityId, StableEntityKind};
use crate::protocol::{IDENTITY_VERSION, SourceAnchor, SourceKey};

const LOGICAL_REPOSITORY_ID: &str = "forge:example.test/acme/repository";
const LOGICAL_WORKTREE_ID: &str = "worktree-1";

fn authorization(path: PathBuf, fingerprint: [u8; 32]) -> CertifiedLiveRoot {
    CertifiedLiveRoot {
        worktree_resource_id: "worktree:test".to_owned(),
        repository_resource_id: "repository:test".to_owned(),
        path,
        security_geometry_fingerprint: hex::encode(fingerprint),
        observed_at_unix_ms: 1,
    }
}
fn repository_identity(
    repository_id: &str,
    worktree_id: Option<&str>,
) -> RepositoryWorktreeIdentity {
    RepositoryWorktreeIdentity {
        repository_id: repository_id.to_owned(),
        worktree_id: worktree_id.map(str::to_owned),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
enum RevalidationAction {
    SwapRoot {
        root: PathBuf,
        replacement: PathBuf,
        displaced: PathBuf,
    },
    AdvanceHead {
        root: PathBuf,
    },
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct FakeAuthority {
    git: GitExecutable,
    file_id: ResourceId,
    file: ExactGitBlameFile,
    roots: Vec<CertifiedLiveRoot>,
    revalidations: Cell<usize>,
    action: RefCell<Option<(usize, RevalidationAction)>>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl FakeAuthority {
    fn new(
        git: GitExecutable,
        file_id: ResourceId,
        file: ExactGitBlameFile,
        roots: Vec<CertifiedLiveRoot>,
        action: Option<(usize, RevalidationAction)>,
    ) -> Self {
        Self {
            git,
            file_id,
            file,
            roots,
            revalidations: Cell::new(0),
            action: RefCell::new(action),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl GitBlameAuthority for FakeAuthority {
    fn authorized_git_executable(&self) -> Option<&GitExecutable> {
        Some(&self.git)
    }

    fn exact_file_authority(&self, file_id: &ResourceId) -> Result<ExactGitBlameFile, QueryError> {
        if file_id != &self.file_id {
            return Err(QueryError::RepositoryUnavailable);
        }
        Ok(self.file.clone())
    }

    fn certified_live_root_candidates(
        &self,
        repository: &RepositoryWorktreeIdentity,
    ) -> Result<Vec<CertifiedLiveRoot>, QueryError> {
        if repository != &self.file.repository {
            return Err(QueryError::RepositoryUnavailable);
        }
        Ok(self.roots.clone())
    }

    fn revalidate_certified_live_root(
        &self,
        authorization: &CertifiedLiveRoot,
    ) -> Result<(), QueryError> {
        if !self.roots.iter().any(|root| root == authorization) {
            return Err(QueryError::RepositoryUnavailable);
        }
        let current = self.revalidations.get().saturating_add(1);
        self.revalidations.set(current);
        let should_run = self
            .action
            .borrow()
            .as_ref()
            .is_some_and(|(trigger, _)| *trigger == current);
        if !should_run {
            return Ok(());
        }
        let Some((_, action)) = self.action.borrow_mut().take() else {
            return Ok(());
        };
        match action {
            RevalidationAction::SwapRoot {
                root,
                replacement,
                displaced,
            } => {
                std::fs::rename(root, displaced)
                    .and_then(|()| std::fs::rename(replacement, authorization.path.as_path()))
                    .map_err(|_| QueryError::RepositoryUnavailable)?;
            }
            RevalidationAction::AdvanceHead { root } => {
                let root = root.to_str().ok_or(QueryError::RepositoryUnavailable)?;
                require_git_success(
                    &self.git,
                    ["-C", root, "commit", "--allow-empty", "-m", "advance HEAD"],
                    MAX_GIT_IDENTITY_BYTES,
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exact_citation() -> Result<Citation, Box<dyn std::error::Error>> {
    let source = SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage([0x4a; 32]),
    )?;
    let stable_entity = |kind: StableEntityKind, byte: u8| {
        let mut uuid_bytes = [byte; 16];
        uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
        uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
        serde_json::from_value::<StableEntityId>(serde_json::json!({
            "contract_version": IDENTITY_VERSION,
            "entity_kind": kind,
            "digest": vec![byte; 32],
            "source_digest": source.identity().digest(),
            "source_descriptor_digest": source.exact_descriptor_digest(),
            "uuid": uuid::Uuid::from_bytes(uuid_bytes),
        }))
    };
    let session_id = stable_entity(StableEntityKind::Session, 0x31)?;
    let event_id = stable_entity(StableEntityKind::Event, 0x21)?;
    Ok(Citation::new(EvidenceCitation {
        core_generation_id: "a".repeat(64),
        source,
        session_id,
        event_id,
        event_sequence: 1,
        byte_range: None,
        evidence_sha256: Some("b".repeat(64)),
    })?)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn fake_file_authority(citation: Citation) -> ExactGitBlameFile {
    ExactGitBlameFile {
        relative_path: "tracked.txt".to_owned(),
        repository: repository_identity(LOGICAL_REPOSITORY_ID, Some(LOGICAL_WORKTREE_ID)),
        core_citation: citation,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn portable_file_authority(citation: Citation) -> ExactGitBlameFile {
    ExactGitBlameFile {
        relative_path: "tracked.txt".to_owned(),
        repository: repository_identity(LOGICAL_REPOSITORY_ID, None),
        core_citation: citation,
    }
}

fn git() -> Result<GitExecutable, Box<dyn std::error::Error>> {
    Ok(GitExecutable::discover()?)
}

fn init_repository(git: &GitExecutable, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir(path)?;
    let path = path.to_str().ok_or("non-UTF-8 test repository")?;
    require_git_success(git, ["init", path], MAX_GIT_IDENTITY_BYTES)?;
    require_git_success(
        git,
        ["-C", path, "config", "user.name", "ctx test"],
        MAX_GIT_IDENTITY_BYTES,
    )?;
    require_git_success(
        git,
        ["-C", path, "config", "user.email", "ctx@example.test"],
        MAX_GIT_IDENTITY_BYTES,
    )?;
    commit_file(git, path, "first\n", "initial")
}

fn commit_file(
    git: &GitExecutable,
    root: &str,
    contents: &str,
    message: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write(Path::new(root).join("tracked.txt"), contents)?;
    require_git_success(
        git,
        ["-C", root, "add", "--", "tracked.txt"],
        MAX_GIT_IDENTITY_BYTES,
    )?;
    require_git_success(
        git,
        ["-C", root, "commit", "-m", message],
        MAX_GIT_IDENTITY_BYTES,
    )?;
    Ok(())
}

fn fingerprint(git: &GitExecutable, path: &Path) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let root = BoundRepositoryRoot::authorize(path)?;
    let (fingerprint, geometry) = live_root_fingerprint(&root, git)?;
    geometry.verify()?;
    Ok(fingerprint)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn fake_authority_runs_generic_blame_without_sql_and_preserves_exact_citation()
-> Result<(), Box<dyn std::error::Error>> {
    let git = git()?;
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("repository");
    init_repository(&git, &root)?;
    let citation = exact_citation()?;
    let file_id = ResourceId("fake-file".to_owned());
    let authority = FakeAuthority::new(
        git.clone(),
        file_id.clone(),
        fake_file_authority(citation.clone()),
        vec![authorization(root.clone(), fingerprint(&git, &root)?)],
        None,
    );

    let window = blame_with_authority(
        &authority,
        &file_id,
        Some(LineRange { start: 1, end: 1 }),
        None,
    )?;

    assert_eq!(authority.revalidations.get(), 3);
    assert_eq!(window.window_start, 1);
    assert_eq!(window.window_end, 1);
    assert_eq!(window.observations.len(), 1);
    assert_eq!(window.observations[0].file, file_id);
    assert_eq!(window.observations[0].citation, citation);
    assert_eq!(window.observations[0].lines, LineRange { start: 1, end: 1 });
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn fake_authority_rejects_root_swap_before_live_git_reads() -> Result<(), Box<dyn std::error::Error>>
{
    let git = git()?;
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("repository");
    let replacement = directory.path().join("replacement");
    let displaced = directory.path().join("displaced");
    init_repository(&git, &root)?;
    init_repository(&git, &replacement)?;
    let file_id = ResourceId("fake-file".to_owned());
    let authority = FakeAuthority::new(
        git.clone(),
        file_id.clone(),
        fake_file_authority(exact_citation()?),
        vec![authorization(root.clone(), fingerprint(&git, &root)?)],
        Some((
            1,
            RevalidationAction::SwapRoot {
                root,
                replacement,
                displaced,
            },
        )),
    );

    assert!(matches!(
        blame_with_authority(
            &authority,
            &file_id,
            Some(LineRange { start: 1, end: 1 }),
            None,
        ),
        Err(QueryError::RepositoryUnavailable)
    ));
    assert_eq!(authority.revalidations.get(), 1);
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn fake_authority_rejects_head_change_during_final_revalidation()
-> Result<(), Box<dyn std::error::Error>> {
    let git = git()?;
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("repository");
    init_repository(&git, &root)?;
    let file_id = ResourceId("fake-file".to_owned());
    let authority = FakeAuthority::new(
        git.clone(),
        file_id.clone(),
        fake_file_authority(exact_citation()?),
        vec![authorization(root.clone(), fingerprint(&git, &root)?)],
        Some((2, RevalidationAction::AdvanceHead { root })),
    );

    assert!(matches!(
        blame_with_authority(
            &authority,
            &file_id,
            Some(LineRange { start: 1, end: 1 }),
            None,
        ),
        Err(QueryError::StaleSnapshot)
    ));
    assert_eq!(authority.revalidations.get(), 2);
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn fake_authority_rejects_truly_divergent_live_worktrees() -> Result<(), Box<dyn std::error::Error>>
{
    let git = git()?;
    let directory = tempfile::tempdir()?;
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    init_repository(&git, &first)?;
    let first_text = first.to_str().ok_or("non-UTF-8 first worktree")?;
    let second_text = second.to_str().ok_or("non-UTF-8 second worktree")?;
    require_git_success(
        &git,
        ["-C", first_text, "worktree", "add", "--detach", second_text],
        MAX_GIT_IDENTITY_BYTES,
    )?;
    commit_file(&git, second_text, "divergent\n", "diverge worktree")?;
    let file_id = ResourceId("fake-file".to_owned());
    let mut second_root = authorization(second.clone(), fingerprint(&git, &second)?);
    second_root.worktree_resource_id = "worktree:second".to_owned();
    let authority = FakeAuthority::new(
        git.clone(),
        file_id.clone(),
        portable_file_authority(exact_citation()?),
        vec![
            authorization(first.clone(), fingerprint(&git, &first)?),
            second_root,
        ],
        None,
    );

    assert!(matches!(
        blame_with_authority(
            &authority,
            &file_id,
            Some(LineRange { start: 1, end: 1 }),
            None,
        ),
        Err(QueryError::AmbiguousRepositoryCandidates(details)) if details.candidates.is_empty()
    ));
    assert_eq!(authority.revalidations.get(), 0);
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn portable_file_uses_equivalent_live_worktrees_and_ignores_retired_roots()
-> Result<(), Box<dyn std::error::Error>> {
    let git = git()?;
    let directory = tempfile::tempdir()?;
    let primary = directory.path().join("primary");
    let linked = directory.path().join("linked");
    init_repository(&git, &primary)?;
    let primary_text = primary.to_str().ok_or("non-UTF-8 primary worktree")?;
    let linked_text = linked.to_str().ok_or("non-UTF-8 linked worktree")?;
    require_git_success(
        &git,
        [
            "-C",
            primary_text,
            "worktree",
            "add",
            "--detach",
            linked_text,
        ],
        MAX_GIT_IDENTITY_BYTES,
    )?;
    let file_id = ResourceId("portable-file".to_owned());
    let primary_root = authorization(primary.clone(), fingerprint(&git, &primary)?);
    let mut linked_root = authorization(linked.clone(), fingerprint(&git, &linked)?);
    linked_root.worktree_resource_id = "worktree:linked".to_owned();
    let equivalent = FakeAuthority::new(
        git.clone(),
        file_id.clone(),
        portable_file_authority(exact_citation()?),
        vec![primary_root, linked_root.clone()],
        None,
    );
    let window = blame_with_authority(
        &equivalent,
        &file_id,
        Some(LineRange { start: 1, end: 1 }),
        None,
    )?;
    assert_eq!(window.observations.len(), 1);

    let retired = directory.path().join("retired");
    init_repository(&git, &retired)?;
    let retired_root = authorization(retired.clone(), fingerprint(&git, &retired)?);
    std::fs::remove_dir_all(&retired)?;
    let surviving = FakeAuthority::new(
        git,
        file_id.clone(),
        portable_file_authority(exact_citation()?),
        vec![retired_root, linked_root],
        None,
    );
    let window = blame_with_authority(
        &surviving,
        &file_id,
        Some(LineRange { start: 1, end: 1 }),
        None,
    )?;
    assert_eq!(window.observations.len(), 1);
    Ok(())
}

#[cfg(unix)]
fn fake_git(
    script: &str,
) -> Result<(tempfile::TempDir, GitExecutable), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("git");
    std::fs::write(&path, script)?;
    let mut permissions = std::fs::metadata(&path)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&path, permissions)?;
    let executable = GitExecutable::authorize(&std::fs::canonicalize(path)?)?;
    Ok((directory, executable))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn repository_rename_preserves_the_certified_geometry_fingerprint()
-> Result<(), Box<dyn std::error::Error>> {
    let git = git()?;
    let parent = tempfile::tempdir()?;
    let original = parent.path().join("original");
    let moved = parent.path().join("moved");
    init_repository(&git, &original)?;
    let fingerprint = fingerprint(&git, &original)?;
    let stale = authorization(original.clone(), fingerprint);
    std::fs::rename(&original, &moved)?;
    assert!(matches!(
        validate_root(&stale, &git),
        Err(QueryError::RepositoryUnavailable)
    ));
    let rebound = authorization(moved, fingerprint);
    let (root, geometry) = validate_root(&rebound, &git)?;
    root.verify()?;
    geometry.verify()?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn recreated_repository_root_fails_until_recertified() -> Result<(), Box<dyn std::error::Error>> {
    let git = git()?;
    let parent = tempfile::tempdir()?;
    let path = parent.path().join("repository");
    let retired = parent.path().join("retired");
    init_repository(&git, &path)?;
    let stale = authorization(path.clone(), fingerprint(&git, &path)?);
    std::fs::rename(&path, &retired)?;
    init_repository(&git, &path)?;
    assert!(matches!(
        validate_root(&stale, &git),
        Err(QueryError::RepositoryUnavailable)
    ));
    let recertified = authorization(path.clone(), fingerprint(&git, &path)?);
    validate_root(&recertified, &git)?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn recreated_git_directory_fails_until_recertified() -> Result<(), Box<dyn std::error::Error>> {
    let git = git()?;
    let parent = tempfile::tempdir()?;
    let path = parent.path().join("repository");
    let retired_git = parent.path().join("retired-git");
    init_repository(&git, &path)?;
    let certified = authorization(path.clone(), fingerprint(&git, &path)?);
    std::fs::rename(path.join(".git"), &retired_git)?;
    let path_text = path.to_str().ok_or("non-UTF-8 test repository")?;
    require_git_success(&git, ["init", path_text], MAX_GIT_IDENTITY_BYTES)?;
    assert!(matches!(
        validate_root(&certified, &git),
        Err(QueryError::RepositoryUnavailable)
    ));
    let recertified = authorization(path.clone(), fingerprint(&git, &path)?);
    validate_root(&recertified, &git)?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn normal_head_change_preserves_the_certified_geometry_fingerprint()
-> Result<(), Box<dyn std::error::Error>> {
    let git = git()?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("repository");
    init_repository(&git, &path)?;
    let before = fingerprint(&git, &path)?;
    commit_file(
        &git,
        path.to_str().ok_or("non-UTF-8 test repository")?,
        "second\n",
        "second",
    )?;
    assert_eq!(fingerprint(&git, &path)?, before);
    validate_root(&authorization(path, before), &git)?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn linked_worktree_fingerprint_binds_git_and_common_directories()
-> Result<(), Box<dyn std::error::Error>> {
    let git = git()?;
    let directory = tempfile::tempdir()?;
    let main = directory.path().join("main");
    let linked = directory.path().join("linked");
    init_repository(&git, &main)?;
    require_git_success(
        &git,
        [
            "-C",
            main.to_str().ok_or("non-UTF-8 test repository")?,
            "worktree",
            "add",
            "--detach",
            linked.to_str().ok_or("non-UTF-8 linked worktree")?,
        ],
        MAX_GIT_IDENTITY_BYTES,
    )?;
    let root = BoundRepositoryRoot::authorize(&linked)?;
    let (certified, geometry) = live_root_fingerprint(&root, &git)?;
    assert_ne!(geometry.git_dir.path(), geometry.common_dir.path());
    validate_root(&authorization(linked, certified), &git)?;
    Ok(())
}

#[test]
fn hostile_environment_does_not_redirect_exact_git_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "graph::git::tests::hostile_environment_child",
            "--nocapture",
        ])
        .env("CTX_ATTRIBUTION_HOSTILE_GIT_ENV_CHILD", "1")
        .env("GIT_DIR", "/definitely/not/the/certified/git-dir")
        .env("GIT_WORK_TREE", "/definitely/not/the/certified/worktree")
        .env(
            "GIT_OBJECT_DIRECTORY",
            "/definitely/not/the/certified/objects",
        )
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.bare")
        .env("GIT_CONFIG_VALUE_0", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    assert!(status.success());
    Ok(())
}

#[test]
fn hostile_environment_child() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("CTX_ATTRIBUTION_HOSTILE_GIT_ENV_CHILD").is_none() {
        return Ok(());
    }
    let git = git()?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("repository");
    init_repository(&git, &path)?;
    let root = BoundRepositoryRoot::authorize(&path)?;
    let path_text = root.path().to_str().ok_or("non-UTF-8 test repository")?;
    let reported = git_text(
        &git,
        ["-C", path_text, "rev-parse", "--show-toplevel"],
        MAX_GIT_IDENTITY_BYTES,
    )?;
    assert_eq!(std::fs::canonicalize(reported.trim())?, root.path());
    Ok(())
}

#[cfg(unix)]
#[test]
fn hanging_exact_git_process_times_out_and_fails_closed() -> Result<(), Box<dyn std::error::Error>>
{
    let (_directory, executable) = fake_git("#!/bin/sh\nwhile :; do :; done\n")?;
    let started = Instant::now();
    assert!(matches!(
        run_git_with_timeout(
            &executable,
            std::iter::empty::<&str>(),
            1024,
            Duration::from_millis(100),
        ),
        Err(QueryError::GitUnavailable)
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn exited_git_parent_cannot_leave_stdout_descendant_unbounded()
-> Result<(), Box<dyn std::error::Error>> {
    let (_directory, executable) = fake_git("#!/bin/sh\n(while :; do :; done) &\nexit 0\n")?;
    let started = Instant::now();
    assert!(matches!(
        run_git_with_timeout(
            &executable,
            std::iter::empty::<&str>(),
            1024,
            Duration::from_millis(100),
        ),
        Err(QueryError::GitUnavailable)
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
    Ok(())
}

#[cfg(unix)]
#[test]
fn bounded_stdout_is_drained_concurrently_without_pipe_deadlock()
-> Result<(), Box<dyn std::error::Error>> {
    let (_directory, executable) = fake_git(
        "#!/bin/sh\ni=0\nwhile [ \"$i\" -lt 100000 ]; do\n  printf 0123456789abcdef\n  i=$((i + 1))\ndone\n",
    )?;
    let started = Instant::now();
    assert!(matches!(
        run_git_with_timeout(
            &executable,
            std::iter::empty::<&str>(),
            128,
            Duration::from_secs(2),
        ),
        Err(QueryError::Backend(_))
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
    Ok(())
}
