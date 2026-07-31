use std::{
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ctx_history_core::{
    GitObjectFormat, GitObjectId, RepositoryAlias, RepositoryAliasKind, RepositoryBinding,
    RepositoryEvidence, RepositoryEvidenceConfidence, RepositoryEvidenceKind,
    RepositoryLocalRootAuthorization,
};
use sha2::{Digest, Sha256};
use url::Url;

use super::shell::lexical_absolute;

const MAX_PARENT_COMPONENTS: usize = 64;
const MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_REMOTES: usize = 64;
const GIT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum CandidateKind {
    Directory,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProbeFailure {
    Missing,
    Unsafe(&'static str),
    AmbiguousRemote,
    Failed(&'static str),
    ConcurrentDrift,
    PlatformUnsupported,
}

#[derive(Debug, Clone)]
pub(super) struct CertifiedCandidate {
    pub(super) binding: RepositoryBinding,
    pub(super) repository_root: PathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct GitCertifier {
    executable: OsString,
    timeout: Duration,
    output_limit: usize,
}

impl Default for GitCertifier {
    fn default() -> Self {
        #[cfg(unix)]
        let executable = OsString::from("/usr/bin/git");
        #[cfg(not(unix))]
        let executable = OsString::from("git");
        Self {
            executable,
            timeout: GIT_TIMEOUT,
            output_limit: MAX_GIT_OUTPUT_BYTES,
        }
    }
}

impl GitCertifier {
    #[cfg(test)]
    pub(super) fn for_test(executable: impl Into<OsString>, timeout: Duration) -> Self {
        Self {
            executable: executable.into(),
            timeout,
            output_limit: MAX_GIT_OUTPUT_BYTES,
        }
    }

    pub(super) fn certify(
        &self,
        path: &Path,
        kind: CandidateKind,
        evidence_kind: RepositoryEvidenceKind,
    ) -> Result<CertifiedCandidate, ProbeFailure> {
        self.certify_with_between_probe(path, kind, evidence_kind, || {})
    }

    pub(super) fn certify_with_between_probe(
        &self,
        path: &Path,
        kind: CandidateKind,
        evidence_kind: RepositoryEvidenceKind,
        between_probe: impl FnOnce(),
    ) -> Result<CertifiedCandidate, ProbeFailure> {
        let probe_directory = validate_candidate_route(path, kind)?;
        let opening = self.inspect_once(&probe_directory)?;
        between_probe();
        let closing = self.inspect_once(&probe_directory)?;
        if opening != closing {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        opening.into_certificate(evidence_kind)
    }

    fn inspect_once(&self, directory: &Path) -> Result<GitSnapshot, ProbeFailure> {
        let route_fingerprint = route_fingerprint(directory)?;
        let geometry = self.run_git(
            directory,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--absolute-git-dir",
                "--git-common-dir",
                "--show-object-format",
            ],
            false,
        )?;
        let lines = utf8_lines(&geometry)?;
        if lines.len() != 4 {
            return Err(ProbeFailure::Failed("unexpected_git_geometry"));
        }
        let root = lexical_absolute(lines[0], None)
            .ok_or(ProbeFailure::Unsafe("non_absolute_git_root"))?;
        let git_dir =
            lexical_absolute(lines[1], None).ok_or(ProbeFailure::Unsafe("non_absolute_git_dir"))?;
        let common_dir = lexical_absolute(lines[2], None)
            .ok_or(ProbeFailure::Unsafe("non_absolute_git_common_dir"))?;
        validate_candidate_route(&root, CandidateKind::Directory)?;
        validate_candidate_route(&git_dir, CandidateKind::Directory)?;
        validate_candidate_route(&common_dir, CandidateKind::Directory)?;
        if !directory.starts_with(&root) {
            return Err(ProbeFailure::Unsafe("git_root_does_not_contain_candidate"));
        }
        let object_format = match lines[3] {
            "sha1" => GitObjectFormat::Sha1,
            "sha256" => GitObjectFormat::Sha256,
            _ => return Err(ProbeFailure::Failed("unsupported_git_object_format")),
        };

        let head_output = self.run_git(directory, &["rev-parse", "--verify", "HEAD"], true)?;
        let head = parse_optional_oid(&head_output, object_format)?;
        let branch_output = self.run_git(directory, &["symbolic-ref", "-q", "HEAD"], true)?;
        let branch = parse_optional_line(&branch_output)?;
        let remotes_output = self.run_git(
            directory,
            &["config", "--local", "--get-regexp", "^remote\\..*\\.url$"],
            true,
        )?;
        let aliases = parse_aliases(&remotes_output)?;

        let mut fingerprint = Sha256::new();
        fingerprint.update(route_fingerprint);
        fingerprint.update(path_fingerprint(&root)?);
        fingerprint.update(path_fingerprint(&git_dir)?);
        fingerprint.update(path_fingerprint(&common_dir)?);
        fingerprint.update(&geometry);
        fingerprint.update(&head_output);
        fingerprint.update(&branch_output);
        fingerprint.update(&remotes_output);
        Ok(GitSnapshot {
            root,
            common_dir,
            object_format,
            head,
            branch,
            aliases,
            locator_fingerprint: fingerprint.finalize().into(),
        })
    }

    fn run_git(
        &self,
        directory: &Path,
        arguments: &[&str],
        allow_empty_status: bool,
    ) -> Result<Vec<u8>, ProbeFailure> {
        let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let mut command = Command::new(&self.executable);
        command
            .env_clear()
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null_device)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .env("LC_ALL", "C")
            .arg("--no-optional-locks")
            .arg("-c")
            .arg(format!("core.hooksPath={null_device}"))
            .arg("-c")
            .arg("credential.helper=")
            .arg("-c")
            .arg("core.fsmonitor=false")
            .arg("-c")
            .arg("maintenance.auto=false")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| ProbeFailure::PlatformUnsupported)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(ProbeFailure::Failed("git_stdout_unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(ProbeFailure::Failed("git_stderr_unavailable"))?;
        let limit = self.output_limit;
        let stdout_reader = thread::spawn(move || read_bounded(stdout, limit));
        let stderr_reader = thread::spawn(move || read_bounded(stderr, limit));
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|_| ProbeFailure::Failed("git_wait_failed"))?
            {
                break status;
            }
            if started.elapsed() >= self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(ProbeFailure::Failed("git_timeout"));
            }
            thread::sleep(Duration::from_millis(5));
        };
        let stdout = stdout_reader
            .join()
            .map_err(|_| ProbeFailure::Failed("git_stdout_reader_failed"))??;
        let _stderr = stderr_reader
            .join()
            .map_err(|_| ProbeFailure::Failed("git_stderr_reader_failed"))??;
        if !status.success() && !(allow_empty_status && status.code() == Some(1)) {
            return Err(ProbeFailure::Failed("git_command_failed"));
        }
        Ok(stdout)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitSnapshot {
    root: PathBuf,
    common_dir: PathBuf,
    object_format: GitObjectFormat,
    head: Option<GitObjectId>,
    branch: Option<String>,
    aliases: Vec<RepositoryAlias>,
    locator_fingerprint: [u8; 32],
}

impl GitSnapshot {
    fn into_certificate(
        self,
        evidence_kind: RepositoryEvidenceKind,
    ) -> Result<CertifiedCandidate, ProbeFailure> {
        let checkout_fingerprint = path_identity_fingerprint(&self.common_dir)?;
        let worktree_fingerprint = path_identity_fingerprint(&self.root)?;
        let checkout_id = format!(
            "checkout:{}",
            digest_hex(&[
                b"ctx.repository.checkout.v1",
                object_format_name(self.object_format),
                &checkout_fingerprint,
            ])
        );
        let worktree_id = format!(
            "worktree:{}",
            digest_hex(&[
                b"ctx.repository.worktree.v1",
                checkout_id.as_bytes(),
                &worktree_fingerprint,
            ])
        );
        let logical_repository_id = self.aliases.first().map_or_else(
            || {
                format!(
                    "local:{}",
                    digest_hex(&[b"ctx.repository.local.v1", checkout_id.as_bytes()])
                )
            },
            |alias| {
                let mut parts = alias.namespace.clone();
                parts.push(alias.name.clone());
                format!("forge:{}/{}", alias.host, parts.join("/"))
            },
        );
        let binding_id = format!(
            "binding:{}",
            digest_hex(&[
                b"ctx.repository.binding.v1",
                logical_repository_id.as_bytes(),
                checkout_id.as_bytes(),
                worktree_id.as_bytes(),
            ])
        );
        let observed_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            .unwrap_or(0);
        let binding = RepositoryBinding {
            binding_id,
            logical_repository_id,
            checkout_id: Some(checkout_id),
            worktree_id: Some(worktree_id),
            aliases: self.aliases,
            git_object_format: Some(self.object_format),
            local_root_authorization: Some(RepositoryLocalRootAuthorization {
                local_root: self.root.to_string_lossy().into_owned(),
                locator_fingerprint: self.locator_fingerprint,
                observed_at_unix_ms,
            }),
            evidence: vec![RepositoryEvidence {
                kind: evidence_kind,
                confidence: RepositoryEvidenceConfidence::High,
            }],
            association_policy_revision: super::ASSOCIATION_POLICY_REVISION,
        };
        Ok(CertifiedCandidate {
            binding,
            repository_root: self.root,
        })
    }
}

fn validate_candidate_route(path: &Path, kind: CandidateKind) -> Result<PathBuf, ProbeFailure> {
    if !path.is_absolute() || path.components().count() > MAX_PARENT_COMPONENTS {
        return Err(ProbeFailure::Unsafe("unbounded_or_relative_candidate"));
    }
    let probe = match kind {
        CandidateKind::Directory => path.to_path_buf(),
        CandidateKind::File => {
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(ProbeFailure::Unsafe("file_candidate_is_symlink"));
                }
                Ok(metadata) if !metadata.is_file() => {
                    return Err(ProbeFailure::Unsafe("file_candidate_is_not_file"));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(ProbeFailure::Unsafe("file_candidate_metadata_failed"));
                }
            }
            let mut parent = path
                .parent()
                .ok_or(ProbeFailure::Unsafe("file_candidate_has_no_parent"))?;
            loop {
                match fs::symlink_metadata(parent) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(ProbeFailure::Unsafe("candidate_contains_symlink"));
                    }
                    Ok(metadata) if metadata.is_dir() => break parent.to_path_buf(),
                    Ok(_) => {
                        return Err(ProbeFailure::Unsafe(
                            "file_candidate_parent_is_not_directory",
                        ));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        parent = parent.parent().ok_or(ProbeFailure::Missing)?;
                    }
                    Err(_) => {
                        return Err(ProbeFailure::Unsafe("candidate_metadata_failed"));
                    }
                }
            }
        }
    };
    let mut components = probe.ancestors().collect::<Vec<_>>();
    components.reverse();
    for component in components {
        let metadata = fs::symlink_metadata(component).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ProbeFailure::Missing
            } else {
                ProbeFailure::Unsafe("candidate_metadata_failed")
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ProbeFailure::Unsafe("candidate_contains_symlink"));
        }
    }
    let metadata = fs::metadata(&probe).map_err(|_| ProbeFailure::Missing)?;
    if !metadata.is_dir() {
        return Err(ProbeFailure::Unsafe(
            "candidate_probe_base_is_not_directory",
        ));
    }
    Ok(probe)
}

fn route_fingerprint(path: &Path) -> Result<[u8; 32], ProbeFailure> {
    let mut digest = Sha256::new();
    let mut components = path.ancestors().collect::<Vec<_>>();
    components.reverse();
    if components.len() > MAX_PARENT_COMPONENTS {
        return Err(ProbeFailure::Unsafe("parent_route_limit_exceeded"));
    }
    for component in components {
        digest.update(component.as_os_str().as_encoded_bytes());
        // A sibling created under a shared ancestor (for example `/tmp`) must
        // not look like drift in this candidate's route. Stable filesystem
        // identity still detects replacement of any component we traversed.
        digest.update(path_identity_fingerprint(component)?);
    }
    Ok(digest.finalize().into())
}

fn path_fingerprint(path: &Path) -> Result<[u8; 32], ProbeFailure> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| ProbeFailure::Unsafe("repository_metadata_failed"))?;
    if metadata.file_type().is_symlink() {
        return Err(ProbeFailure::Unsafe("repository_path_is_symlink"));
    }
    let mut digest = Sha256::new();
    digest.update(path.as_os_str().as_encoded_bytes());
    digest.update(metadata.len().to_be_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        digest.update(metadata.dev().to_be_bytes());
        digest.update(metadata.ino().to_be_bytes());
        digest.update(metadata.mode().to_be_bytes());
        digest.update(metadata.mtime().to_be_bytes());
        digest.update(metadata.mtime_nsec().to_be_bytes());
    }
    #[cfg(not(unix))]
    {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map_or(0_u128, |duration| duration.as_nanos());
        digest.update(modified.to_be_bytes());
    }
    Ok(digest.finalize().into())
}

fn path_identity_fingerprint(path: &Path) -> Result<[u8; 32], ProbeFailure> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| ProbeFailure::Unsafe("repository_identity_metadata_failed"))?;
    if metadata.file_type().is_symlink() {
        return Err(ProbeFailure::Unsafe("repository_identity_path_is_symlink"));
    }
    let mut digest = Sha256::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        digest.update(metadata.dev().to_be_bytes());
        digest.update(metadata.ino().to_be_bytes());
    }
    #[cfg(not(unix))]
    {
        digest.update(path.as_os_str().as_encoded_bytes());
        digest.update(metadata.len().to_be_bytes());
    }
    Ok(digest.finalize().into())
}

fn read_bounded(mut reader: impl Read, limit: usize) -> Result<Vec<u8>, ProbeFailure> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let mut exceeded = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| ProbeFailure::Failed("git_output_read_failed"))?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(remaining)]);
        exceeded |= read > remaining;
    }
    if exceeded {
        Err(ProbeFailure::Failed("git_output_limit_exceeded"))
    } else {
        Ok(output)
    }
}

fn utf8_lines(value: &[u8]) -> Result<Vec<&str>, ProbeFailure> {
    std::str::from_utf8(value)
        .map_err(|_| ProbeFailure::Unsafe("git_output_is_not_unicode"))
        .map(|value| value.lines().collect())
}

fn parse_optional_line(value: &[u8]) -> Result<Option<String>, ProbeFailure> {
    let lines = utf8_lines(value)?;
    match lines.as_slice() {
        [] => Ok(None),
        [line] if !line.is_empty() => Ok(Some((*line).to_owned())),
        _ => Err(ProbeFailure::Failed("unexpected_git_scalar")),
    }
}

fn parse_optional_oid(
    value: &[u8],
    format: GitObjectFormat,
) -> Result<Option<GitObjectId>, ProbeFailure> {
    let Some(hex) = parse_optional_line(value)? else {
        return Ok(None);
    };
    let object = GitObjectId { format, hex };
    object
        .validate_contract()
        .map_err(|_| ProbeFailure::Failed("invalid_git_head"))?;
    Ok(Some(object))
}

fn parse_aliases(value: &[u8]) -> Result<Vec<RepositoryAlias>, ProbeFailure> {
    let text = std::str::from_utf8(value)
        .map_err(|_| ProbeFailure::Unsafe("remote_output_is_not_unicode"))?;
    let mut aliases = Vec::new();
    for line in text.lines() {
        let Some((key, remote)) = line.split_once(char::is_whitespace) else {
            return Err(ProbeFailure::Failed("malformed_remote_config"));
        };
        let remote_name = key
            .strip_prefix("remote.")
            .and_then(|value| value.strip_suffix(".url"))
            .ok_or(ProbeFailure::Failed("malformed_remote_key"))?;
        if let Some(mut alias) = normalize_remote(remote.trim())? {
            alias.remote_name = Some(remote_name.to_owned());
            aliases.push(alias);
        }
        if aliases.len() > MAX_REMOTES {
            return Err(ProbeFailure::Failed("remote_limit_exceeded"));
        }
    }
    aliases.sort_by(|left, right| {
        (&left.host, &left.namespace, &left.name, &left.remote_name).cmp(&(
            &right.host,
            &right.namespace,
            &right.name,
            &right.remote_name,
        ))
    });
    aliases.dedup();
    let mut logical = aliases
        .iter()
        .map(|alias| (&alias.host, &alias.namespace, &alias.name))
        .collect::<Vec<_>>();
    logical.sort();
    logical.dedup();
    if logical.len() > 1 {
        return Err(ProbeFailure::AmbiguousRemote);
    }
    Ok(aliases)
}

fn normalize_remote(remote: &str) -> Result<Option<RepositoryAlias>, ProbeFailure> {
    let (host, path) = if let Ok(url) = Url::parse(remote) {
        if !matches!(url.scheme(), "http" | "https" | "ssh" | "git") {
            return Ok(None);
        }
        if matches!(url.scheme(), "http" | "https")
            && (!url.username().is_empty() || url.password().is_some())
        {
            return Err(ProbeFailure::Unsafe("credential_bearing_remote"));
        }
        let host = url
            .host_str()
            .ok_or(ProbeFailure::Failed("remote_host_missing"))?;
        (
            host.to_ascii_lowercase(),
            url.path().trim_matches('/').to_owned(),
        )
    } else if let Some((authority, path)) = remote.split_once(':') {
        if authority.contains('/') || path.is_empty() {
            return Ok(None);
        }
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        (host.to_ascii_lowercase(), path.trim_matches('/').to_owned())
    } else {
        return Ok(None);
    };
    let path = path.strip_suffix(".git").unwrap_or(&path);
    let mut parts = path
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if parts.len() < 2
        || parts.iter().any(|part| {
            part == "."
                || part == ".."
                || part
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || matches!(byte, b'@' | b'\\'))
        })
    {
        return Err(ProbeFailure::Failed("remote_path_invalid"));
    }
    let name = parts
        .pop()
        .ok_or(ProbeFailure::Failed("remote_name_missing"))?;
    Ok(Some(RepositoryAlias {
        kind: RepositoryAliasKind::Forge,
        host,
        namespace: parts,
        name,
        remote_name: None,
    }))
}

fn object_format_name(format: GitObjectFormat) -> &'static [u8] {
    match format {
        GitObjectFormat::Sha1 => b"sha1",
        GitObjectFormat::Sha256 => b"sha256",
    }
}

fn digest_hex(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part);
        digest.update([0]);
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[allow(dead_code)]
fn _os_str_is_bounded(value: &OsStr) -> bool {
    value.as_encoded_bytes().len() <= MAX_GIT_OUTPUT_BYTES
}
