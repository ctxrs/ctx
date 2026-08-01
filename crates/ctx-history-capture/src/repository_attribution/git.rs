use std::{
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use ctx_history_core::{
    GitObjectFormat, GitObjectId, RepositoryAlias, RepositoryAliasKind, RepositoryBinding,
    RepositoryEvidence, RepositoryEvidenceConfidence, RepositoryEvidenceKind,
    RepositoryLocalRootAuthorization, CORE_REPOSITORY_LOCATOR_FINGERPRINT_REVISION,
};
use sha2::{Digest, Sha256};
use url::Url;

use super::shell::lexical_absolute;
mod geometry;

pub(super) use geometry::negative_route_geometry_state;
use geometry::{
    path_identity_fingerprint, repository_geometry_state, repository_locator_fingerprint,
    repository_mutable_evidence_state, route_fingerprint, validate_candidate_route,
};

const MAX_PARENT_COMPONENTS: usize = 64;
const MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_REMOTES: usize = 64;
const GIT_TIMEOUT: Duration = Duration::from_secs(2);
// Two repositories, each checked by two snapshots of two Git subprocesses.
pub(super) const MAX_FULL_CERTIFICATIONS_PER_EVENT: usize = 2;
pub(super) const MAX_GIT_SUBPROCESSES_PER_EVENT: usize = 8;
const MAX_GIT_PROBE_TIME_PER_EVENT: Duration = Duration::from_secs(4);

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
    BudgetExceeded,
}

pub(super) struct EventProbeBudget {
    full_certifications: usize,
    git_subprocesses: usize,
    deadline: Instant,
}

impl EventProbeBudget {
    pub(super) fn new() -> Self {
        Self {
            full_certifications: 0,
            git_subprocesses: 0,
            deadline: Instant::now() + MAX_GIT_PROBE_TIME_PER_EVENT,
        }
    }

    fn start_full_certification(&mut self) -> Result<(), ProbeFailure> {
        if self.full_certifications >= MAX_FULL_CERTIFICATIONS_PER_EVENT
            || Instant::now() >= self.deadline
        {
            return Err(ProbeFailure::BudgetExceeded);
        }
        self.full_certifications += 1;
        Ok(())
    }

    fn start_git_subprocess(
        &mut self,
        timeout: Duration,
    ) -> Result<(Duration, bool), ProbeFailure> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or(ProbeFailure::BudgetExceeded)?;
        if self.git_subprocesses >= MAX_GIT_SUBPROCESSES_PER_EVENT || remaining.is_zero() {
            return Err(ProbeFailure::BudgetExceeded);
        }
        self.git_subprocesses += 1;
        Ok((timeout.min(remaining), remaining <= timeout))
    }
}

#[derive(Debug, Clone)]
pub(super) struct CertifiedCandidate {
    pub(super) binding: RepositoryBinding,
    pub(super) repository_root: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    // Internal cache fence only: logical identity and wire authorization keep
    // their existing contracts while pointer/geometry changes force a probe.
    repository_geometry_state: [u8; 32],
    branch: Option<String>,
    mutable_evidence_state: [u8; 32],
}

#[derive(Debug, Clone)]
pub(super) struct GitCertifier {
    executable: OsString,
    timeout: Duration,
    output_limit: usize,
    full_certification_probes: Arc<AtomicUsize>,
    git_subprocesses: Arc<AtomicUsize>,
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
            full_certification_probes: Arc::new(AtomicUsize::new(0)),
            git_subprocesses: Arc::new(AtomicUsize::new(0)),
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
            full_certification_probes: Arc::new(AtomicUsize::new(0)),
            git_subprocesses: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[cfg(test)]
    pub(super) fn certify(
        &self,
        path: &Path,
        kind: CandidateKind,
        evidence_kind: RepositoryEvidenceKind,
    ) -> Result<CertifiedCandidate, ProbeFailure> {
        self.certify_at(
            path,
            kind,
            evidence_kind,
            ctx_history_core::CORE_MISSING_ACTIVITY_TIME_UNIX_MS,
        )
    }

    #[cfg(test)]
    pub(super) fn certify_at(
        &self,
        path: &Path,
        kind: CandidateKind,
        evidence_kind: RepositoryEvidenceKind,
        observed_at_unix_ms: i64,
    ) -> Result<CertifiedCandidate, ProbeFailure> {
        let mut budget = EventProbeBudget::new();
        self.certify_with_between_probe_at(
            path,
            kind,
            evidence_kind,
            observed_at_unix_ms,
            &mut budget,
            || {},
        )
    }

    pub(super) fn certify_at_with_budget(
        &self,
        path: &Path,
        kind: CandidateKind,
        evidence_kind: RepositoryEvidenceKind,
        observed_at_unix_ms: i64,
        budget: &mut EventProbeBudget,
    ) -> Result<CertifiedCandidate, ProbeFailure> {
        self.certify_with_between_probe_at(
            path,
            kind,
            evidence_kind,
            observed_at_unix_ms,
            budget,
            || {},
        )
    }

    #[cfg(test)]
    pub(super) fn certify_with_between_probe(
        &self,
        path: &Path,
        kind: CandidateKind,
        evidence_kind: RepositoryEvidenceKind,
        between_probe: impl FnOnce(),
    ) -> Result<CertifiedCandidate, ProbeFailure> {
        self.certify_with_between_probe_at(
            path,
            kind,
            evidence_kind,
            ctx_history_core::CORE_MISSING_ACTIVITY_TIME_UNIX_MS,
            &mut EventProbeBudget::new(),
            between_probe,
        )
    }

    fn certify_with_between_probe_at(
        &self,
        path: &Path,
        kind: CandidateKind,
        evidence_kind: RepositoryEvidenceKind,
        observed_at_unix_ms: i64,
        budget: &mut EventProbeBudget,
        between_probe: impl FnOnce(),
    ) -> Result<CertifiedCandidate, ProbeFailure> {
        budget.start_full_certification()?;
        self.full_certification_probes
            .fetch_add(1, Ordering::Relaxed);
        let probe_directory = validate_candidate_route(path, kind)?;
        let opening = self.inspect_once(&probe_directory, budget)?;
        between_probe();
        let closing = self.inspect_once(&probe_directory, budget)?;
        if opening != closing {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        opening.into_certificate(evidence_kind, observed_at_unix_ms)
    }

    pub(super) fn full_certification_probe_count(&self) -> usize {
        self.full_certification_probes.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(super) fn git_subprocess_count(&self) -> usize {
        self.git_subprocesses.load(Ordering::Relaxed)
    }

    fn inspect_once(
        &self,
        directory: &Path,
        budget: &mut EventProbeBudget,
    ) -> Result<GitSnapshot, ProbeFailure> {
        route_fingerprint(directory)?;
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
            budget,
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
        let repository_geometry = repository_geometry_state(&root)?;
        if repository_geometry.git_dir != git_dir || repository_geometry.common_dir != common_dir {
            return Err(ProbeFailure::ConcurrentDrift);
        }

        let branch = repository_head_branch(&git_dir, object_format)?;
        let remotes_output = self.run_git(
            directory,
            &[
                "config",
                "--no-includes",
                "--local",
                "--get-regexp",
                "^remote\\..*\\.url$",
            ],
            true,
            budget,
        )?;
        let aliases = parse_aliases(&remotes_output)?;
        let mutable_evidence_state =
            repository_mutable_evidence_state(&git_dir, &common_dir, branch.as_deref())?;

        let locator_fingerprint =
            repository_locator_fingerprint(&root, &git_dir, &common_dir, object_format)?;
        Ok(GitSnapshot {
            root,
            git_dir,
            common_dir,
            object_format,
            branch,
            aliases,
            repository_geometry_state: repository_geometry.fingerprint,
            locator_fingerprint,
            mutable_evidence_state,
        })
    }

    fn run_git(
        &self,
        directory: &Path,
        arguments: &[&str],
        allow_empty_status: bool,
        budget: &mut EventProbeBudget,
    ) -> Result<Vec<u8>, ProbeFailure> {
        let (timeout, event_deadline_limited) = budget.start_git_subprocess(self.timeout)?;
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
        self.git_subprocesses.fetch_add(1, Ordering::Relaxed);
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
            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(if event_deadline_limited {
                    ProbeFailure::BudgetExceeded
                } else {
                    ProbeFailure::Failed("git_timeout")
                });
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
    git_dir: PathBuf,
    common_dir: PathBuf,
    object_format: GitObjectFormat,
    branch: Option<String>,
    aliases: Vec<RepositoryAlias>,
    repository_geometry_state: [u8; 32],
    locator_fingerprint: [u8; 32],
    mutable_evidence_state: [u8; 32],
}

impl GitSnapshot {
    fn into_certificate(
        self,
        evidence_kind: RepositoryEvidenceKind,
        observed_at_unix_ms: i64,
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
        let binding = RepositoryBinding {
            binding_id,
            logical_repository_id,
            checkout_id: Some(checkout_id),
            worktree_id: Some(worktree_id),
            aliases: self.aliases,
            git_object_format: Some(self.object_format),
            local_root_authorization: Some(RepositoryLocalRootAuthorization {
                local_root: self.root.to_string_lossy().into_owned(),
                locator_fingerprint_revision: CORE_REPOSITORY_LOCATOR_FINGERPRINT_REVISION,
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
            git_dir: self.git_dir,
            common_dir: self.common_dir,
            repository_geometry_state: self.repository_geometry_state,
            branch: self.branch,
            mutable_evidence_state: self.mutable_evidence_state,
        })
    }
}

impl CertifiedCandidate {
    pub(super) fn lexical_root_contains(&self, path: &Path) -> bool {
        path.starts_with(&self.repository_root)
    }

    pub(super) fn try_reuse(
        &self,
        path: &Path,
        kind: CandidateKind,
        evidence_kind: RepositoryEvidenceKind,
        observed_at_unix_ms: i64,
    ) -> Result<Option<Self>, ProbeFailure> {
        let probe_directory = validate_candidate_route(path, kind)?;
        if !probe_directory.starts_with(&self.repository_root)
            || has_nested_repository_boundary(&probe_directory, &self.repository_root)?
        {
            return Ok(None);
        }
        let Some(authorization) = self.binding.local_root_authorization.as_ref() else {
            return Ok(None);
        };
        let object_format = self.binding.git_object_format.ok_or(ProbeFailure::Unsafe(
            "cached_repository_has_no_object_format",
        ))?;
        let geometry = repository_geometry_state(&self.repository_root)?;
        if geometry.git_dir != self.git_dir
            || geometry.common_dir != self.common_dir
            || geometry.fingerprint != self.repository_geometry_state
        {
            return Ok(None);
        }
        let current = repository_locator_fingerprint(
            &self.repository_root,
            &self.git_dir,
            &self.common_dir,
            object_format,
        )?;
        if current != authorization.locator_fingerprint {
            return Ok(None);
        }
        let mutable_evidence_state = repository_mutable_evidence_state(
            &self.git_dir,
            &self.common_dir,
            self.branch.as_deref(),
        )?;
        if mutable_evidence_state != self.mutable_evidence_state {
            return Ok(None);
        }
        let closing_probe = validate_candidate_route(path, kind)?;
        let closing_geometry = repository_geometry_state(&self.repository_root)?;
        if closing_probe != probe_directory || closing_geometry != geometry {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        let closing = repository_locator_fingerprint(
            &self.repository_root,
            &self.git_dir,
            &self.common_dir,
            object_format,
        )?;
        let closing_mutable_evidence_state = repository_mutable_evidence_state(
            &self.git_dir,
            &self.common_dir,
            self.branch.as_deref(),
        )?;
        if closing != current || closing_mutable_evidence_state != mutable_evidence_state {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        let mut reused = self.clone();
        reused.binding.evidence = vec![RepositoryEvidence {
            kind: evidence_kind,
            confidence: RepositoryEvidenceConfidence::High,
        }];
        if let Some(authorization) = reused.binding.local_root_authorization.as_mut() {
            authorization.observed_at_unix_ms = observed_at_unix_ms;
        }
        Ok(Some(reused))
    }
}

fn has_nested_repository_boundary(candidate: &Path, root: &Path) -> Result<bool, ProbeFailure> {
    let mut current = candidate;
    while current != root {
        match fs::symlink_metadata(current.join(".git")) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ProbeFailure::Unsafe("nested_git_boundary_metadata_failed")),
        }
        let mut bare_markers = 0;
        for entry in ["HEAD", "objects", "refs"] {
            match fs::symlink_metadata(current.join(entry)) {
                Ok(_) => bare_markers += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(ProbeFailure::Unsafe("nested_git_boundary_metadata_failed"));
                }
            }
        }
        if bare_markers == 3 {
            return Ok(true);
        }
        current = current.parent().ok_or(ProbeFailure::Unsafe(
            "cached_candidate_escaped_repository_root",
        ))?;
    }
    Ok(false)
}

fn metadata_is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
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

fn repository_head_branch(
    git_dir: &Path,
    format: GitObjectFormat,
) -> Result<Option<String>, ProbeFailure> {
    let head_path = git_dir.join("HEAD");
    let metadata = fs::symlink_metadata(&head_path)
        .map_err(|_| ProbeFailure::Failed("git_head_metadata_failed"))?;
    if metadata_is_link_like(&metadata) || !metadata.is_file() {
        return Err(ProbeFailure::Unsafe("git_head_is_not_regular_file"));
    }
    if metadata.len() > MAX_GIT_OUTPUT_BYTES as u64 {
        return Err(ProbeFailure::Failed("git_head_limit_exceeded"));
    }
    let value = fs::read(&head_path).map_err(|_| ProbeFailure::Failed("git_head_read_failed"))?;
    let line = parse_optional_line(&value)?.ok_or(ProbeFailure::Failed("git_head_is_empty"))?;
    if let Some(branch) = line.strip_prefix("ref: ") {
        if !canonical_symbolic_branch(branch) {
            return Err(ProbeFailure::Unsafe("git_branch_is_not_canonical"));
        }
        return Ok(Some(branch.to_owned()));
    }
    if parse_optional_oid(&value, format)?.is_some() {
        Ok(None)
    } else {
        Err(ProbeFailure::Failed("invalid_git_head"))
    }
}

fn canonical_symbolic_branch(branch: &str) -> bool {
    branch.starts_with("refs/")
        && !branch
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        && !branch
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
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
