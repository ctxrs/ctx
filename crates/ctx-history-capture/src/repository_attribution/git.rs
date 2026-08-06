use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use super::shell::lexical_absolute;
use ctx_history_core::{
    GitObjectFormat, GitObjectId, RepositoryAlias, RepositoryBinding, RepositoryEvidence,
    RepositoryEvidenceConfidence, RepositoryEvidenceKind, RepositoryFileObservationKind,
    RepositoryLocalRootAuthorization,
    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
};
use sha2::{Digest, Sha256};
mod geometry;
mod parsing;
mod pull_request;

pub(super) use geometry::{negative_route_geometry_state, validate_candidate_route};
use geometry::{
    path_identity_fingerprint, repository_geometry_state,
    repository_local_root_authorization_fingerprint, repository_mutable_evidence_state,
    route_fingerprint,
};
use parsing::{
    canonical_symbolic_branch, digest_hex, metadata_is_link_like, object_format_name,
    parse_aliases, parse_resolved_commit_files, parse_resolved_commit_metadata, read_bounded,
    repository_head_branch, utf8_lines,
};

const MAX_PARENT_COMPONENTS: usize = 64;
const MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_RESOLVED_COMMIT_FILES: usize = 256;
const MAX_PULL_REQUEST_CONTAINS_COMMITS: usize = 256;
const MAX_REMOTES: usize = 64;
const GIT_TIMEOUT: Duration = Duration::from_secs(2);
// Two repositories, each checked by two snapshots of two Git subprocesses.
pub(super) const MAX_FULL_CERTIFICATIONS_PER_EVENT: usize = 2;
pub(super) const MAX_GIT_SUBPROCESSES_PER_EVENT: usize = 10;
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
    Failed(&'static str),
    ConcurrentDrift,
    ConflictingEventTimeIdentity,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedCommit {
    pub(super) object_id: GitObjectId,
    pub(super) parent_object_ids: Vec<GitObjectId>,
    pub(super) files: Vec<ResolvedCommitFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedCommitFile {
    pub(super) path: String,
    pub(super) prior_path: Option<String>,
    pub(super) kind: RepositoryFileObservationKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedPullRequestMergeMembership {
    pub(super) contains_commits: Vec<GitObjectId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResolvedCommitProducer {
    Commit,
    Merge,
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

    pub(super) fn resolve_commit(
        &self,
        certificate: &CertifiedCandidate,
        oid_prefix: &str,
        expected_subject: &str,
        producer: ResolvedCommitProducer,
        budget: &mut EventProbeBudget,
    ) -> Result<ResolvedCommit, ProbeFailure> {
        if !(7..=64).contains(&oid_prefix.len())
            || !oid_prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
            || expected_subject.is_empty()
            || expected_subject.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(ProbeFailure::Failed("invalid_deferred_commit_hint"));
        }
        certificate.ensure_current_geometry()?;
        let revision = format!("{}^{{commit}}", oid_prefix.to_ascii_lowercase());
        let metadata = self.run_git(
            &certificate.repository_root,
            &["show", "-s", "--format=%H%x00%P%x00%s", &revision],
            false,
            budget,
        )?;
        let (object_id, parent_object_ids, subject) =
            parse_resolved_commit_metadata(&metadata, certificate.object_format())?;
        if subject != expected_subject {
            return Err(ProbeFailure::Failed("commit_subject_mismatch"));
        }
        match producer {
            ResolvedCommitProducer::Commit if parent_object_ids.len() > 1 => {
                return Err(ProbeFailure::Failed("commit_has_merge_parent_shape"));
            }
            ResolvedCommitProducer::Merge if parent_object_ids.len() < 2 => {
                return Err(ProbeFailure::Failed("merge_has_nonmerge_parent_shape"));
            }
            _ => {}
        }

        let containing_refs = self.run_git(
            &certificate.repository_root,
            &[
                "for-each-ref",
                "--contains",
                object_id.hex.as_str(),
                "--count=1",
                "--format=%(refname)",
                "refs/heads",
                "refs/tags",
            ],
            false,
            budget,
        )?;
        let containing_refs = utf8_lines(&containing_refs)?;
        if containing_refs.len() != 1
            || containing_refs
                .iter()
                .any(|reference| !canonical_symbolic_branch(reference))
        {
            return Err(ProbeFailure::Failed(
                "commit_is_not_reachable_from_local_ref",
            ));
        }

        let object_hex = object_id.hex.as_str();
        let diff = if let Some(first_parent) = parent_object_ids.first() {
            self.run_git(
                &certificate.repository_root,
                &[
                    "diff-tree",
                    "-r",
                    "--no-commit-id",
                    "--name-status",
                    "-z",
                    "--find-renames=100%",
                    first_parent.hex.as_str(),
                    object_hex,
                ],
                false,
                budget,
            )?
        } else {
            self.run_git(
                &certificate.repository_root,
                &[
                    "diff-tree",
                    "--root",
                    "-r",
                    "--no-commit-id",
                    "--name-status",
                    "-z",
                    "--find-renames=100%",
                    object_hex,
                ],
                false,
                budget,
            )?
        };
        let files = parse_resolved_commit_files(&diff)?;
        certificate.ensure_current_geometry()?;
        Ok(ResolvedCommit {
            object_id,
            parent_object_ids,
            files,
        })
    }

    /// Resolves the exact full source from the bounded command and the one
    /// native Git result prefix/subject in a single certified repository
    /// window. The linked command/result receipt supplies causality; these
    /// object probes only close identity, object format, and drift predicates.
    pub(super) fn resolve_cherry_pick_operation(
        &self,
        certificate: &CertifiedCandidate,
        source: &GitObjectId,
        result_oid_prefix: &str,
        result_subject: &str,
        budget: &mut EventProbeBudget,
    ) -> Result<(ResolvedCommit, [u8; 32]), ProbeFailure> {
        source
            .validate_contract()
            .map_err(|_| ProbeFailure::Failed("invalid_cherry_pick_source"))?;
        if source.format != certificate.object_format() {
            return Err(ProbeFailure::Failed("cherry_pick_object_format_mismatch"));
        }
        certificate.ensure_current_geometry()?;
        let opening_mutable_state = repository_mutable_evidence_state(
            &certificate.git_dir,
            &certificate.common_dir,
            certificate.branch.as_deref(),
        )?;
        if opening_mutable_state != certificate.mutable_evidence_state {
            return Err(ProbeFailure::ConcurrentDrift);
        }

        let source_revision = format!("{}^{{commit}}", source.hex);
        let source_output = self.run_git(
            &certificate.repository_root,
            &["show", "-s", "--format=%H", &source_revision],
            false,
            budget,
        )?;
        if utf8_lines(&source_output)?.as_slice() != [source.hex.as_str()] {
            return Err(ProbeFailure::Failed(
                "cherry_pick_source_resolution_mismatch",
            ));
        }

        let result = self.resolve_commit(
            certificate,
            result_oid_prefix,
            result_subject,
            ResolvedCommitProducer::Commit,
            budget,
        )?;
        if result.object_id.format != source.format || result.object_id == *source {
            return Err(ProbeFailure::Failed("invalid_cherry_pick_mapping"));
        }

        certificate.ensure_current_geometry()?;
        let closing_mutable_state = repository_mutable_evidence_state(
            &certificate.git_dir,
            &certificate.common_dir,
            certificate.branch.as_deref(),
        )?;
        if closing_mutable_state != opening_mutable_state {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        Ok((result, repository_object_domain_sha256(certificate)))
    }

    /// Verifies full source/result commit objects in one immutable
    /// repository/object domain. Object existence is corroboration only; the
    /// caller must already hold prospective linked operation evidence.
    pub(super) fn verify_commit_operation_objects(
        &self,
        certificate: &CertifiedCandidate,
        object_ids: &[GitObjectId],
        budget: &mut EventProbeBudget,
    ) -> Result<[u8; 32], ProbeFailure> {
        if object_ids.is_empty() || object_ids.len() > 2 {
            return Err(ProbeFailure::Failed(
                "commit_operation_object_bound_exceeded",
            ));
        }
        certificate.ensure_current_geometry()?;
        let opening_mutable_state = repository_mutable_evidence_state(
            &certificate.git_dir,
            &certificate.common_dir,
            certificate.branch.as_deref(),
        )?;
        if opening_mutable_state != certificate.mutable_evidence_state {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        for object_id in object_ids {
            object_id
                .validate_contract()
                .map_err(|_| ProbeFailure::Failed("invalid_commit_operation_object"))?;
            if object_id.format != certificate.object_format() {
                return Err(ProbeFailure::Failed(
                    "commit_operation_object_format_mismatch",
                ));
            }
            let revision = format!("{}^{{commit}}", object_id.hex);
            let output = self.run_git(
                &certificate.repository_root,
                &["show", "-s", "--format=%H", &revision],
                false,
                budget,
            )?;
            let lines = utf8_lines(&output)?;
            if lines.as_slice() != [object_id.hex.as_str()] {
                return Err(ProbeFailure::Failed(
                    "commit_operation_object_resolution_mismatch",
                ));
            }
        }
        certificate.ensure_current_geometry()?;
        let closing_mutable_state = repository_mutable_evidence_state(
            &certificate.git_dir,
            &certificate.common_dir,
            certificate.branch.as_deref(),
        )?;
        if closing_mutable_state != opening_mutable_state {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        Ok(repository_object_domain_sha256(certificate))
    }

    #[cfg(test)]
    pub(super) fn git_subprocess_count(&self) -> usize {
        self.git_subprocesses.load(Ordering::Relaxed)
    }

    fn executable_state(
        &self,
    ) -> Result<([u8; 32], u64, Option<std::time::SystemTime>), ProbeFailure> {
        let path = Path::new(&self.executable);
        if !path.is_absolute() {
            return Err(ProbeFailure::PlatformUnsupported);
        }
        let identity = path_identity_fingerprint(path)?;
        let metadata = fs::metadata(path)
            .map_err(|_| ProbeFailure::Unsafe("git_executable_metadata_failed"))?;
        Ok((identity, metadata.len(), metadata.modified().ok()))
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

        let local_root_authorization_fingerprint = repository_local_root_authorization_fingerprint(
            &root,
            &git_dir,
            &common_dir,
            object_format,
        )?;
        Ok(GitSnapshot {
            root,
            git_dir,
            common_dir,
            object_format,
            branch,
            aliases,
            repository_geometry_state: repository_geometry.fingerprint,
            local_root_authorization_fingerprint,
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
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("LC_ALL", "C")
            .arg("--no-optional-locks")
            .arg("--no-replace-objects")
            .arg("-c")
            .arg(format!("core.hooksPath={null_device}"))
            .arg("-c")
            .arg("credential.helper=")
            .arg("-c")
            .arg("core.fsmonitor=false")
            .arg("-c")
            .arg("maintenance.auto=false")
            .arg("-c")
            .arg("protocol.allow=never")
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

fn repository_object_domain_sha256(certificate: &CertifiedCandidate) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.repository.object-domain.v1\0");
    digest.update(certificate.binding.logical_repository_id.as_bytes());
    digest.update([match certificate.object_format() {
        GitObjectFormat::Sha1 => 1,
        GitObjectFormat::Sha256 => 2,
    }]);
    digest.update(certificate.repository_geometry_state);
    digest.finalize().into()
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
    local_root_authorization_fingerprint: [u8; 32],
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
        let logical_repository_id = authoritative_logical_alias(&self.aliases).map_or_else(
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
                local_root_authorization_fingerprint_revision:
                    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
                local_root_authorization_fingerprint: self.local_root_authorization_fingerprint,
                observed_at_unix_ms,
            }),
            evidence: vec![RepositoryEvidence {
                kind: evidence_kind,
                confidence: RepositoryEvidenceConfidence::High,
            }],
            association_policy_revision:
                ctx_history_core::CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
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

fn authoritative_logical_alias(aliases: &[RepositoryAlias]) -> Option<&RepositoryAlias> {
    let first = aliases.first()?;
    let origin = aliases
        .iter()
        .filter(|alias| alias.remote_name.as_deref() == Some("origin"))
        .collect::<Vec<_>>();
    if let Some(authority) = origin.first().copied() {
        if origin
            .iter()
            .all(|alias| same_alias_identity(alias, authority))
        {
            return Some(authority);
        }
    }
    aliases
        .iter()
        .all(|alias| same_alias_identity(alias, first))
        .then_some(first)
}

fn same_alias_identity(left: &RepositoryAlias, right: &RepositoryAlias) -> bool {
    left.host.eq_ignore_ascii_case(&right.host)
        && left.namespace == right.namespace
        && left.name == right.name
}

impl CertifiedCandidate {
    pub(super) fn lexical_root_contains(&self, path: &Path) -> bool {
        path.starts_with(&self.repository_root)
    }

    pub(super) fn observed_at_unix_ms(&self) -> i64 {
        self.binding.local_root_authorization.as_ref().map_or(
            ctx_history_core::CORE_MISSING_ACTIVITY_TIME_UNIX_MS,
            |authorization| authorization.observed_at_unix_ms,
        )
    }

    fn object_format(&self) -> GitObjectFormat {
        self.binding
            .git_object_format
            .expect("certified Git candidates always carry an object format")
    }

    fn ensure_current_geometry(&self) -> Result<(), ProbeFailure> {
        validate_candidate_route(&self.repository_root, CandidateKind::Directory)?;
        let geometry = repository_geometry_state(&self.repository_root)?;
        if geometry.git_dir != self.git_dir
            || geometry.common_dir != self.common_dir
            || geometry.fingerprint != self.repository_geometry_state
        {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        let Some(authorization) = self.binding.local_root_authorization.as_ref() else {
            return Err(ProbeFailure::Missing);
        };
        let fingerprint = repository_local_root_authorization_fingerprint(
            &self.repository_root,
            &self.git_dir,
            &self.common_dir,
            self.object_format(),
        )?;
        if fingerprint != authorization.local_root_authorization_fingerprint {
            return Err(ProbeFailure::ConcurrentDrift);
        }
        Ok(())
    }

    pub(super) fn same_binding_identity(&self, other: &Self) -> bool {
        self.binding.binding_id == other.binding.binding_id
            && self.binding.logical_repository_id == other.binding.logical_repository_id
            && self.binding.checkout_id == other.binding.checkout_id
            && self.binding.worktree_id == other.binding.worktree_id
            && self.binding.git_object_format == other.binding.git_object_format
    }

    pub(super) fn same_local_root_authorization_identity(&self, other: &Self) -> bool {
        self.binding
            .local_root_authorization
            .as_ref()
            .zip(other.binding.local_root_authorization.as_ref())
            .is_some_and(|(left, right)| {
                left.local_root_authorization_fingerprint_revision
                    == right.local_root_authorization_fingerprint_revision
                    && left.local_root_authorization_fingerprint
                        == right.local_root_authorization_fingerprint
            })
    }

    pub(super) fn for_event(
        &self,
        evidence_kind: RepositoryEvidenceKind,
        observed_at_unix_ms: i64,
    ) -> Self {
        let mut certificate = self.clone();
        certificate.binding.evidence = vec![RepositoryEvidence {
            kind: evidence_kind,
            confidence: RepositoryEvidenceConfidence::High,
        }];
        if let Some(authorization) = certificate.binding.local_root_authorization.as_mut() {
            authorization.observed_at_unix_ms = observed_at_unix_ms;
        }
        certificate
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
        let current = repository_local_root_authorization_fingerprint(
            &self.repository_root,
            &self.git_dir,
            &self.common_dir,
            object_format,
        )?;
        if current != authorization.local_root_authorization_fingerprint {
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
        let closing = repository_local_root_authorization_fingerprint(
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
        Ok(Some(self.for_event(evidence_kind, observed_at_unix_ms)))
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
