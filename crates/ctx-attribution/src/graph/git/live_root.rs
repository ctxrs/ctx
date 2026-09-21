//! Certified repository-root selection and revalidation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::git_executable::GitExecutable;
use crate::protocol::CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN;
use crate::query::{AmbiguityCandidates, QueryError};
use crate::repository_path::BoundRepositoryRoot;

use super::authority::{CertifiedLiveRoot, GitBlameAuthority, RepositoryWorktreeIdentity};
use super::command::{
    GIT_COMMAND_TIMEOUT, MAX_GIT_IDENTITY_BYTES, git_text, git_text_with_timeout,
};

const CORE_LIVE_ROOT_FINGERPRINT_VERSION: u16 = 1;

pub(super) fn unique_root<A: GitBlameAuthority + ?Sized>(
    authority: &A,
    repository: &RepositoryWorktreeIdentity,
) -> Result<CertifiedLiveRoot, QueryError> {
    let roots = authority.certified_live_root_candidates(repository)?;
    let git_executable = authority
        .authorized_git_executable()
        .ok_or(QueryError::RepositoryUnavailable)?;
    let mut by_head = BTreeMap::<String, Vec<CertifiedLiveRoot>>::new();
    for authorization in roots {
        if validate_persisted_fingerprint(&authorization).is_err() {
            continue;
        }
        match live_root_head(&authorization, git_executable) {
            Ok(head) => by_head.entry(head).or_default().push(authorization),
            Err(QueryError::RepositoryUnavailable) => continue,
            Err(error) => return Err(error),
        }
    }
    let mut heads = by_head.into_values();
    let Some(mut equivalent) = heads.next() else {
        return Err(QueryError::RepositoryUnavailable);
    };
    if heads.next().is_some() {
        return Err(QueryError::AmbiguousRepositoryCandidates(
            AmbiguityCandidates::undisclosed(),
        ));
    }
    equivalent.sort_by(|left, right| {
        right
            .observed_at_unix_ms
            .cmp(&left.observed_at_unix_ms)
            .then_with(|| left.worktree_resource_id.cmp(&right.worktree_resource_id))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| {
                left.security_geometry_fingerprint
                    .cmp(&right.security_geometry_fingerprint)
            })
    });
    equivalent
        .into_iter()
        .next()
        .ok_or(QueryError::RepositoryUnavailable)
}

pub(super) fn live_root_head(
    authorization: &CertifiedLiveRoot,
    git_executable: &GitExecutable,
) -> Result<String, QueryError> {
    let (root, geometry) = validate_root(authorization, git_executable)?;
    verify_git_top_level(&root, git_executable)?;
    let root_text = root
        .path()
        .to_str()
        .ok_or(QueryError::RepositoryUnavailable)?;
    let head = git_text(
        git_executable,
        ["-C", root_text, "rev-parse", "--verify", "HEAD^{commit}"],
        MAX_GIT_IDENTITY_BYTES,
    )?
    .trim()
    .to_ascii_lowercase();
    if !matches!(head.len(), 40 | 64) || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(QueryError::RepositoryUnavailable);
    }
    root.verify()
        .map_err(|_| QueryError::RepositoryUnavailable)?;
    geometry.verify()?;
    Ok(head)
}

fn validate_persisted_fingerprint(authorization: &CertifiedLiveRoot) -> Result<(), QueryError> {
    let fingerprint = &authorization.security_geometry_fingerprint;
    if fingerprint.len() == 64
        && fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(QueryError::RepositoryUnavailable)
    }
}

pub(super) fn revalidate_live_root<A: GitBlameAuthority + ?Sized>(
    authority: &A,
    authorization: &CertifiedLiveRoot,
) -> Result<(), QueryError> {
    authority.revalidate_certified_live_root(authorization)
}

pub(super) fn validate_root(
    authorization: &CertifiedLiveRoot,
    git_executable: &GitExecutable,
) -> Result<(BoundRepositoryRoot, BoundRepositoryGeometry), QueryError> {
    validate_root_with_timeout(authorization, git_executable, GIT_COMMAND_TIMEOUT)
}

fn validate_root_with_timeout(
    authorization: &CertifiedLiveRoot,
    git_executable: &GitExecutable,
    timeout: Duration,
) -> Result<(BoundRepositoryRoot, BoundRepositoryGeometry), QueryError> {
    let root = BoundRepositoryRoot::authorize(&authorization.path)
        .map_err(|_| QueryError::RepositoryUnavailable)?;
    let geometry =
        validate_geometry_fingerprint_with_timeout(&root, authorization, git_executable, timeout)?;
    Ok((root, geometry))
}

#[derive(Debug)]
pub(super) struct BoundRepositoryGeometry {
    pub(super) git_dir: BoundRepositoryRoot,
    pub(super) common_dir: BoundRepositoryRoot,
}

impl BoundRepositoryGeometry {
    pub(super) fn verify(&self) -> Result<(), QueryError> {
        self.git_dir
            .verify()
            .and_then(|()| self.common_dir.verify())
            .map_err(|_| QueryError::RepositoryUnavailable)
    }
}

pub(super) fn validate_geometry_fingerprint(
    root: &BoundRepositoryRoot,
    authorization: &CertifiedLiveRoot,
    git_executable: &GitExecutable,
) -> Result<BoundRepositoryGeometry, QueryError> {
    validate_geometry_fingerprint_with_timeout(
        root,
        authorization,
        git_executable,
        GIT_COMMAND_TIMEOUT,
    )
}

fn validate_geometry_fingerprint_with_timeout(
    root: &BoundRepositoryRoot,
    authorization: &CertifiedLiveRoot,
    git_executable: &GitExecutable,
    timeout: Duration,
) -> Result<BoundRepositoryGeometry, QueryError> {
    let expected = hex::decode(&authorization.security_geometry_fingerprint)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or(QueryError::RepositoryUnavailable)?;
    let (actual, geometry) = live_root_fingerprint_with_timeout(root, git_executable, timeout)?;
    if actual != expected {
        return Err(QueryError::RepositoryUnavailable);
    }
    geometry.verify()?;
    Ok(geometry)
}

#[cfg(test)]
pub(super) fn live_root_fingerprint(
    root: &BoundRepositoryRoot,
    git_executable: &GitExecutable,
) -> Result<([u8; 32], BoundRepositoryGeometry), QueryError> {
    live_root_fingerprint_with_timeout(root, git_executable, GIT_COMMAND_TIMEOUT)
}

fn live_root_fingerprint_with_timeout(
    root: &BoundRepositoryRoot,
    git_executable: &GitExecutable,
    timeout: Duration,
) -> Result<([u8; 32], BoundRepositoryGeometry), QueryError> {
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (root, git_executable);
        return Err(QueryError::RepositoryUnavailable);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        root.verify()
            .map_err(|_| QueryError::RepositoryUnavailable)?;
        let root_text = root
            .path()
            .to_str()
            .ok_or(QueryError::RepositoryUnavailable)?;
        let output = git_text_with_timeout(
            git_executable,
            [
                "-C",
                root_text,
                "rev-parse",
                "--path-format=absolute",
                "--git-dir",
                "--git-common-dir",
                "--show-object-format",
            ],
            MAX_GIT_IDENTITY_BYTES,
            timeout,
        )?;
        let fields = output.lines().collect::<Vec<_>>();
        let [git_dir, common_dir, object_format] = fields.as_slice() else {
            return Err(QueryError::RepositoryUnavailable);
        };
        if !matches!(*object_format, "sha1" | "sha256") {
            return Err(QueryError::RepositoryUnavailable);
        }
        let git_dir = BoundRepositoryRoot::authorize(Path::new(git_dir))
            .map_err(|_| QueryError::RepositoryUnavailable)?;
        let common_dir = BoundRepositoryRoot::authorize(Path::new(common_dir))
            .map_err(|_| QueryError::RepositoryUnavailable)?;
        let geometry = BoundRepositoryGeometry {
            git_dir,
            common_dir,
        };
        let mut digest = Sha256::new();
        digest.update(CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_DOMAIN);
        digest.update(CORE_LIVE_ROOT_FINGERPRINT_VERSION.to_be_bytes());
        update_unix_directory_identity(&mut digest, b"certified_root", root)?;
        update_unix_directory_identity(&mut digest, b"git_dir", &geometry.git_dir)?;
        update_unix_directory_identity(&mut digest, b"common_dir", &geometry.common_dir)?;
        digest.update([4]);
        digest.update((object_format.len() as u64).to_be_bytes());
        digest.update(object_format.as_bytes());
        root.verify()
            .map_err(|_| QueryError::RepositoryUnavailable)?;
        geometry.verify()?;
        Ok((digest.finalize().into(), geometry))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn update_unix_directory_identity(
    digest: &mut Sha256,
    label: &[u8],
    directory: &BoundRepositoryRoot,
) -> Result<(), QueryError> {
    use std::os::unix::fs::MetadataExt as _;

    directory
        .verify()
        .map_err(|_| QueryError::RepositoryUnavailable)?;
    let metadata =
        fs::symlink_metadata(directory.path()).map_err(|_| QueryError::RepositoryUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(QueryError::RepositoryUnavailable);
    }
    digest.update([1]);
    digest.update((label.len() as u64).to_be_bytes());
    digest.update(label);
    digest.update(metadata.dev().to_be_bytes());
    digest.update(metadata.ino().to_be_bytes());
    directory
        .verify()
        .map_err(|_| QueryError::RepositoryUnavailable)
}

pub(super) fn validate_relative_file(file: &str) -> Result<(), QueryError> {
    let path = Path::new(file);
    if file.is_empty()
        || file.contains(['\0', '\n', '\r'])
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(QueryError::InvalidRequest(
            "Git file path must stay inside its repository".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn verify_git_top_level(
    root: &BoundRepositoryRoot,
    git_executable: &GitExecutable,
) -> Result<(), QueryError> {
    verify_git_top_level_with_timeout(root, git_executable, GIT_COMMAND_TIMEOUT)
}

fn verify_git_top_level_with_timeout(
    root: &BoundRepositoryRoot,
    git_executable: &GitExecutable,
    timeout: Duration,
) -> Result<(), QueryError> {
    root.verify()
        .map_err(|_| QueryError::RepositoryUnavailable)?;
    let root_text = root
        .path()
        .to_str()
        .ok_or(QueryError::RepositoryUnavailable)?;
    let output = git_text_with_timeout(
        git_executable,
        ["-C", root_text, "rev-parse", "--show-toplevel"],
        MAX_GIT_IDENTITY_BYTES,
        timeout,
    )?;
    let reported = PathBuf::from(output.trim());
    let canonical = fs::canonicalize(reported).map_err(|_| QueryError::RepositoryUnavailable)?;
    if canonical != root.path() {
        return Err(QueryError::RepositoryUnavailable);
    }
    root.verify().map_err(|_| QueryError::RepositoryUnavailable)
}
