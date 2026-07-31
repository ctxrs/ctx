mod git;
mod shell;

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    CoreRecordAnnotation, GitObjectId, RepositoryAbstention, RepositoryAbstentionReason,
    RepositoryAlias, RepositoryAliasKind, RepositoryBinding, RepositoryEvidence,
    RepositoryEvidenceConfidence, RepositoryEvidenceKind, RepositoryFileObservation,
    RepositoryFileObservationKind, RepositoryVcsObservation, RepositoryVcsObservationKind,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use git::{CandidateKind, CertifiedCandidate, GitCertifier, ProbeFailure};
use shell::{analyze, lexical_absolute};

pub(crate) const ASSOCIATION_POLICY_REVISION: u32 = 1;
const MAX_PROVIDER_NATIVE_IDENTITIES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnscopedFileObservation {
    pub(crate) path: String,
    pub(crate) prior_path: Option<String>,
    pub(crate) kind: RepositoryFileObservationKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnscopedVcsObservation {
    pub(crate) path: Option<String>,
    pub(crate) kind: RepositoryVcsObservationKind,
    pub(crate) object_id: Option<GitObjectId>,
    pub(crate) parent_object_ids: Vec<GitObjectId>,
    pub(crate) reference: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct AttributionInput {
    pub(crate) provider_native_repository_aliases: Vec<RepositoryAlias>,
    pub(crate) session_cwd: Option<String>,
    pub(crate) declared_tool_workdir: Option<String>,
    pub(crate) command: Option<String>,
    pub(crate) structured_content: Option<Value>,
    pub(crate) file_observations: Vec<UnscopedFileObservation>,
    pub(crate) vcs_observations: Vec<UnscopedVcsObservation>,
}

#[derive(Debug, Default)]
pub(crate) struct RepositoryAttributor {
    certifier: GitCertifier,
    cache: HashMap<(PathBuf, CandidateKind), Result<CertifiedCandidate, ProbeFailure>>,
}

impl RepositoryAttributor {
    pub(crate) fn attribute(&mut self, input: AttributionInput) -> CoreRecordAnnotation {
        // Certificates authorize the route observed during this event only.
        // A later event must revalidate after a move, replacement, or removal.
        self.cache.clear();
        attribute_with_attributor(input, self)
    }

    fn certify(&mut self, candidate: &Candidate) -> Result<CertifiedCandidate, ProbeFailure> {
        if let Some(cached) = self.cache.get(&(candidate.path.clone(), candidate.kind)) {
            return cached.clone().map(|certificate| {
                certificate_with_evidence(certificate, candidate.evidence_kind)
            });
        }
        if let Some(certificate) = self
            .cache
            .values()
            .filter_map(|result| result.as_ref().ok())
            .find(|certificate| candidate.path.starts_with(&certificate.repository_root))
        {
            return Ok(certificate_with_evidence(
                certificate.clone(),
                candidate.evidence_kind,
            ));
        }
        let result =
            self.certifier
                .certify(&candidate.path, candidate.kind, candidate.evidence_kind);
        self.cache
            .insert((candidate.path.clone(), candidate.kind), result.clone());
        result
    }
}

#[derive(Debug, Clone)]
struct Candidate {
    path: PathBuf,
    kind: CandidateKind,
    evidence_kind: RepositoryEvidenceKind,
}

#[derive(Debug, Clone)]
struct ScopedFileInput {
    path: PathBuf,
    prior_path: Option<PathBuf>,
    kind: RepositoryFileObservationKind,
}

#[derive(Debug, Clone)]
struct ScopedVcsInput {
    path: Option<PathBuf>,
    observation: UnscopedVcsObservation,
}

enum ProviderIdentityResolution {
    Absent,
    Binding(Box<RepositoryBinding>),
    Abstained,
}

#[cfg(test)]
pub(crate) fn attribute(input: AttributionInput) -> CoreRecordAnnotation {
    RepositoryAttributor::default().attribute(input)
}

fn attribute_with_attributor(
    input: AttributionInput,
    attributor: &mut RepositoryAttributor,
) -> CoreRecordAnnotation {
    let mut annotation = CoreRecordAnnotation {
        structured_content: input.structured_content,
        metadata: BTreeMap::from([(
            "repository_association".to_owned(),
            json!({
                "association_policy_revision": ASSOCIATION_POLICY_REVISION,
                "candidate_source": "bounded_structured_activity",
            }),
        )]),
        ..CoreRecordAnnotation::default()
    };
    let provider_identity =
        resolve_provider_native_identity(input.provider_native_repository_aliases, &mut annotation);

    let session_cwd = bounded_absolute(
        input.session_cwd.as_deref(),
        RepositoryEvidenceKind::SessionCwd,
        &mut annotation.repository_abstentions,
    );
    let declared_workdir = bounded_absolute(
        input.declared_tool_workdir.as_deref(),
        RepositoryEvidenceKind::DeclaredToolWorkdir,
        &mut annotation.repository_abstentions,
    );
    annotation.repository_candidate_evidence.session_cwd = session_cwd.as_deref().map(path_string);
    annotation
        .repository_candidate_evidence
        .declared_tool_workdir = declared_workdir.as_deref().map(path_string);

    let base = declared_workdir.as_deref().or(session_cwd.as_deref());
    let command_analysis = analyze(input.command.as_deref(), base);
    annotation
        .repository_abstentions
        .extend(
            command_analysis
                .abstentions
                .iter()
                .map(|abstention| RepositoryAbstention {
                    evidence_kind: abstention.evidence_kind,
                    reason: abstention.reason,
                    detail: Some(abstention.detail.to_owned()),
                    association_policy_revision: ASSOCIATION_POLICY_REVISION,
                }),
        );
    annotation
        .repository_candidate_evidence
        .derived_effective_cwd = command_analysis
        .derived_effective_cwd
        .as_deref()
        .map(path_string);
    annotation
        .repository_candidate_evidence
        .command_specific_repository_path = command_analysis
        .repository_paths
        .iter()
        .find(|candidate| {
            candidate.evidence_kind == RepositoryEvidenceKind::CommandSpecificRepositoryPath
        })
        .map(|candidate| path_string(&candidate.path));

    let mut candidates = Vec::new();
    let more_specific_command = !command_analysis.repository_paths.is_empty();
    if more_specific_command {
        candidates.extend(
            command_analysis
                .repository_paths
                .iter()
                .map(|candidate| Candidate {
                    path: candidate.path.clone(),
                    kind: CandidateKind::Directory,
                    evidence_kind: candidate.evidence_kind,
                }),
        );
    } else if let Some(workdir) = &declared_workdir {
        candidates.push(Candidate {
            path: workdir.clone(),
            kind: CandidateKind::Directory,
            evidence_kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
        });
    }

    let file_base = declared_workdir.as_deref().or(session_cwd.as_deref());
    let mut file_inputs = Vec::new();
    for observation in input.file_observations {
        let Some(path) = lexical_absolute(&observation.path, file_base) else {
            push_abstention(
                &mut annotation,
                RepositoryEvidenceKind::FileActivity,
                RepositoryAbstentionReason::UnscopedFileActivity,
                "unscoped_or_dynamic_file_path",
            );
            continue;
        };
        let prior_path = match (observation.kind, observation.prior_path.as_deref()) {
            (RepositoryFileObservationKind::Renamed, Some(path)) => {
                let Some(path) = lexical_absolute(path, file_base) else {
                    push_abstention(
                        &mut annotation,
                        RepositoryEvidenceKind::FileActivity,
                        RepositoryAbstentionReason::UnscopedFileActivity,
                        "rename_prior_path_is_not_bounded_literal",
                    );
                    continue;
                };
                Some(path)
            }
            (RepositoryFileObservationKind::Renamed, None) | (_, Some(_)) => {
                push_abstention(
                    &mut annotation,
                    RepositoryEvidenceKind::FileActivity,
                    RepositoryAbstentionReason::UnscopedFileActivity,
                    "file_change_and_prior_path_shape_conflict",
                );
                continue;
            }
            (_, None) => None,
        };
        candidates.push(Candidate {
            path: path.clone(),
            kind: CandidateKind::File,
            evidence_kind: RepositoryEvidenceKind::FileActivity,
        });
        file_inputs.push(ScopedFileInput {
            path,
            prior_path,
            kind: observation.kind,
        });
    }

    let mut vcs_inputs = Vec::new();
    for observation in input.vcs_observations {
        let path = match observation.path.as_deref() {
            Some(path) => match lexical_absolute(path, file_base) {
                Some(path) => Some(path),
                None => {
                    push_abstention(
                        &mut annotation,
                        RepositoryEvidenceKind::VcsActivity,
                        RepositoryAbstentionReason::UnsafePath,
                        "unscoped_or_dynamic_vcs_path",
                    );
                    continue;
                }
            },
            None => {
                let Some(base) = file_base else {
                    push_abstention(
                        &mut annotation,
                        RepositoryEvidenceKind::VcsActivity,
                        RepositoryAbstentionReason::UnsafePath,
                        "vcs_observation_has_no_structured_base",
                    );
                    continue;
                };
                Some(base.to_path_buf())
            }
        };
        if let Some(path) = &path {
            candidates.push(Candidate {
                path: path.clone(),
                kind: CandidateKind::Directory,
                evidence_kind: RepositoryEvidenceKind::VcsActivity,
            });
        }
        vcs_inputs.push(ScopedVcsInput { path, observation });
    }

    let attempted_specific = input.declared_tool_workdir.is_some()
        || more_specific_command
        || command_analysis.blocks_session_fallback
        || !file_inputs.is_empty()
        || !vcs_inputs.is_empty();
    if candidates.is_empty() && !attempted_specific {
        if let Some(cwd) = &session_cwd {
            candidates.push(Candidate {
                path: cwd.clone(),
                kind: CandidateKind::Directory,
                evidence_kind: RepositoryEvidenceKind::SessionCwd,
            });
        }
    }
    dedupe_candidates(&mut candidates);

    let mut certified = Vec::new();
    for candidate in candidates {
        match attributor.certify(&candidate) {
            Ok(certificate) => merge_certificate(&mut certified, certificate),
            Err(failure) => push_probe_failure(
                &mut annotation,
                candidate.evidence_kind,
                failure,
                matches!(provider_identity, ProviderIdentityResolution::Binding(_)),
            ),
        }
    }
    reconcile_provider_identity(&mut annotation, &mut certified, provider_identity);
    annotation.repository_bindings.extend(
        certified
            .iter()
            .map(|certificate| certificate.binding.clone()),
    );
    scope_files(&mut annotation, &certified, file_inputs);
    scope_vcs(&mut annotation, &certified, vcs_inputs);
    if annotation.repository_bindings.is_empty() && annotation.repository_abstentions.is_empty() {
        push_abstention(
            &mut annotation,
            RepositoryEvidenceKind::SessionCwd,
            RepositoryAbstentionReason::NoCandidate,
            "no_structured_repository_candidate",
        );
    }
    annotation
}

fn certificate_with_evidence(
    mut certificate: CertifiedCandidate,
    evidence_kind: RepositoryEvidenceKind,
) -> CertifiedCandidate {
    let evidence = RepositoryEvidence {
        kind: evidence_kind,
        confidence: RepositoryEvidenceConfidence::High,
    };
    if !certificate.binding.evidence.contains(&evidence) {
        certificate.binding.evidence.push(evidence);
    }
    certificate
}

fn bounded_absolute(
    value: Option<&str>,
    kind: RepositoryEvidenceKind,
    abstentions: &mut Vec<RepositoryAbstention>,
) -> Option<PathBuf> {
    let value = value?;
    match lexical_absolute(value, None) {
        Some(path) => Some(path),
        None => {
            abstentions.push(RepositoryAbstention {
                evidence_kind: kind,
                reason: RepositoryAbstentionReason::UnsafePath,
                detail: Some("structured_path_is_not_bounded_absolute_literal".to_owned()),
                association_policy_revision: ASSOCIATION_POLICY_REVISION,
            });
            None
        }
    }
}

fn dedupe_candidates(candidates: &mut Vec<Candidate>) {
    let mut deduped = Vec::with_capacity(candidates.len());
    for candidate in candidates.drain(..) {
        if !deduped.iter().any(|existing: &Candidate| {
            existing.path == candidate.path
                && existing.kind == candidate.kind
                && existing.evidence_kind == candidate.evidence_kind
        }) {
            deduped.push(candidate);
        }
    }
    *candidates = deduped;
}

fn merge_certificate(certificates: &mut Vec<CertifiedCandidate>, incoming: CertifiedCandidate) {
    if let Some(existing) = certificates
        .iter_mut()
        .find(|certificate| certificate.binding.binding_id == incoming.binding.binding_id)
    {
        for evidence in incoming.binding.evidence {
            if !existing.binding.evidence.contains(&evidence) {
                existing.binding.evidence.push(evidence);
            }
        }
    } else {
        certificates.push(incoming);
    }
}

fn push_probe_failure(
    annotation: &mut CoreRecordAnnotation,
    evidence_kind: RepositoryEvidenceKind,
    failure: ProbeFailure,
    has_provider_identity: bool,
) {
    let (reason, detail) = match failure {
        ProbeFailure::Missing if has_provider_identity => (
            RepositoryAbstentionReason::Unavailable,
            "candidate_missing_but_provider_identity_retained",
        ),
        ProbeFailure::Missing => (
            RepositoryAbstentionReason::CandidateMissingBeforeCertification,
            "candidate_missing_before_certification",
        ),
        ProbeFailure::Unsafe(detail) => (RepositoryAbstentionReason::UnsafePath, detail),
        ProbeFailure::AmbiguousRemote => (
            RepositoryAbstentionReason::AmbiguousRemote,
            "credential_free_remotes_conflict",
        ),
        ProbeFailure::Failed(detail) => (RepositoryAbstentionReason::GitProbeFailed, detail),
        ProbeFailure::ConcurrentDrift => (
            RepositoryAbstentionReason::ConcurrentDrift,
            "repository_changed_during_probe",
        ),
        ProbeFailure::PlatformUnsupported => (
            RepositoryAbstentionReason::PlatformUnsupported,
            "safe_git_probe_unavailable",
        ),
    };
    push_abstention(annotation, evidence_kind, reason, detail);
}

fn resolve_provider_native_identity(
    aliases: Vec<RepositoryAlias>,
    annotation: &mut CoreRecordAnnotation,
) -> ProviderIdentityResolution {
    if aliases.is_empty() {
        return ProviderIdentityResolution::Absent;
    }
    if aliases.len() > MAX_PROVIDER_NATIVE_IDENTITIES {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeProject,
            RepositoryAbstentionReason::Ambiguous,
            "provider_native_identity_limit_exceeded",
        );
        return ProviderIdentityResolution::Abstained;
    }

    let mut by_logical_id = BTreeMap::<String, Vec<RepositoryAlias>>::new();
    for mut alias in aliases {
        if alias.kind != RepositoryAliasKind::Forge {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::ProviderNativeProject,
                RepositoryAbstentionReason::Unsafe,
                "provider_native_identity_is_not_forge_structured",
            );
            return ProviderIdentityResolution::Abstained;
        }
        alias.host.make_ascii_lowercase();
        let logical_id = logical_repository_id(&alias);
        let validation = logical_only_binding(logical_id.clone(), vec![alias.clone()]);
        if validation.validate_contract().is_err() {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::ProviderNativeProject,
                RepositoryAbstentionReason::Unsafe,
                "invalid_provider_native_repository_alias",
            );
            return ProviderIdentityResolution::Abstained;
        }
        let values = by_logical_id.entry(logical_id).or_default();
        if !values.contains(&alias) {
            values.push(alias);
        }
    }
    if by_logical_id.len() != 1 {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeProject,
            RepositoryAbstentionReason::ConflictingIdentity,
            "provider_native_repository_identities_conflict",
        );
        return ProviderIdentityResolution::Abstained;
    }
    let (logical_id, aliases) = by_logical_id.into_iter().next().expect("one identity");
    let binding = logical_only_binding(logical_id, aliases);
    if binding.validate_contract().is_err() {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeProject,
            RepositoryAbstentionReason::Unsafe,
            "invalid_provider_native_repository_binding",
        );
        ProviderIdentityResolution::Abstained
    } else {
        ProviderIdentityResolution::Binding(Box::new(binding))
    }
}

fn reconcile_provider_identity(
    annotation: &mut CoreRecordAnnotation,
    certified: &mut Vec<CertifiedCandidate>,
    resolution: ProviderIdentityResolution,
) {
    let provider = match resolution {
        ProviderIdentityResolution::Absent => return,
        ProviderIdentityResolution::Abstained => {
            certified.clear();
            return;
        }
        ProviderIdentityResolution::Binding(provider) => provider,
    };
    let mut matched = false;
    for certificate in certified.iter_mut().filter(|certificate| {
        certificate.binding.logical_repository_id == provider.logical_repository_id
    }) {
        matched = true;
        for alias in &provider.aliases {
            if !certificate.binding.aliases.contains(alias) {
                certificate.binding.aliases.push(alias.clone());
            }
        }
        for evidence in &provider.evidence {
            if !certificate.binding.evidence.contains(evidence) {
                certificate.binding.evidence.push(evidence.clone());
            }
        }
    }
    if matched {
        return;
    }
    if !certified.is_empty() {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeProject,
            RepositoryAbstentionReason::ConflictingIdentity,
            "provider_native_identity_does_not_match_local_certificate",
        );
        certified.clear();
    }
    annotation.repository_bindings.push(*provider);
}

fn logical_only_binding(
    logical_repository_id: String,
    aliases: Vec<RepositoryAlias>,
) -> RepositoryBinding {
    let mut digest = Sha256::new();
    digest.update(b"ctx.repository.provider-binding.v1");
    digest.update(logical_repository_id.as_bytes());
    let digest = digest.finalize();
    RepositoryBinding {
        binding_id: format!("binding:{}", hex_digest(&digest)),
        logical_repository_id,
        checkout_id: None,
        worktree_id: None,
        aliases,
        git_object_format: None,
        local_root_authorization: None,
        evidence: vec![RepositoryEvidence {
            kind: RepositoryEvidenceKind::ProviderNativeProject,
            confidence: RepositoryEvidenceConfidence::Explicit,
        }],
        association_policy_revision: ASSOCIATION_POLICY_REVISION,
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn logical_repository_id(alias: &RepositoryAlias) -> String {
    let mut path = alias.namespace.join("/");
    path.push('/');
    path.push_str(&alias.name);
    format!("forge:{}/{path}", alias.host)
}

fn push_abstention(
    annotation: &mut CoreRecordAnnotation,
    evidence_kind: RepositoryEvidenceKind,
    reason: RepositoryAbstentionReason,
    detail: &str,
) {
    let abstention = RepositoryAbstention {
        evidence_kind,
        reason,
        detail: Some(detail.to_owned()),
        association_policy_revision: ASSOCIATION_POLICY_REVISION,
    };
    if !annotation.repository_abstentions.contains(&abstention) {
        annotation.repository_abstentions.push(abstention);
    }
}

fn scope_files(
    annotation: &mut CoreRecordAnnotation,
    certified: &[CertifiedCandidate],
    inputs: Vec<ScopedFileInput>,
) {
    for input in inputs {
        let Some(certificate) = most_specific_certificate(certified, &input.path) else {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::FileActivity,
                RepositoryAbstentionReason::UnscopedFileActivity,
                "file_path_has_no_certified_repository",
            );
            continue;
        };
        let Some(relative_path) = repository_relative(&certificate.repository_root, &input.path)
        else {
            continue;
        };
        let prior_relative_path = input
            .prior_path
            .as_ref()
            .and_then(|path| repository_relative(&certificate.repository_root, path));
        if input.kind == RepositoryFileObservationKind::Renamed && prior_relative_path.is_none() {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::FileActivity,
                RepositoryAbstentionReason::UnscopedFileActivity,
                "rename_paths_do_not_share_one_certified_repository",
            );
            continue;
        }
        annotation
            .repository_file_observations
            .push(RepositoryFileObservation {
                repository_binding_id: certificate.binding.binding_id.clone(),
                relative_path,
                kind: input.kind,
                prior_relative_path,
            });
    }
}

fn scope_vcs(
    annotation: &mut CoreRecordAnnotation,
    certified: &[CertifiedCandidate],
    inputs: Vec<ScopedVcsInput>,
) {
    for input in inputs {
        let Some(path) = input.path.as_ref() else {
            continue;
        };
        let Some(certificate) = most_specific_certificate(certified, path) else {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::VcsActivity,
                RepositoryAbstentionReason::UnsafePath,
                "vcs_observation_has_no_certified_repository",
            );
            continue;
        };
        let relative_path = if path == &certificate.repository_root {
            None
        } else {
            repository_relative(&certificate.repository_root, path)
        };
        annotation
            .repository_vcs_observations
            .push(RepositoryVcsObservation {
                repository_binding_id: certificate.binding.binding_id.clone(),
                kind: input.observation.kind,
                object_id: input.observation.object_id,
                parent_object_ids: input.observation.parent_object_ids,
                reference: input.observation.reference,
                relative_path,
            });
    }
}

fn most_specific_certificate<'a>(
    certified: &'a [CertifiedCandidate],
    path: &Path,
) -> Option<&'a CertifiedCandidate> {
    certified
        .iter()
        .filter(|certificate| path.starts_with(&certificate.repository_root))
        .max_by_key(|certificate| certificate.repository_root.components().count())
}

fn repository_relative(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    Some(relative.to_string_lossy().replace('\\', "/"))
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests;
