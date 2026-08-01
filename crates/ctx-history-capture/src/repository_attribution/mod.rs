mod git;
mod outcome;
mod shell;

use std::{
    collections::BTreeMap,
    fmt::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    CoreRecordAnnotation, GitObjectId, RepositoryAbstention, RepositoryAbstentionReason,
    RepositoryAlias, RepositoryAliasKind, RepositoryBinding, RepositoryEvidence,
    RepositoryEvidenceConfidence, RepositoryEvidenceKind, RepositoryFileObservation,
    RepositoryFileObservationKind, RepositoryOutcomeObservation, RepositoryVcsObservation,
    RepositoryVcsObservationKind, CORE_BOUNDED_SHELL_SUBSET_REVISION,
    CORE_MISSING_ACTIVITY_TIME_UNIX_MS, CORE_REPOSITORY_LOCATOR_FINGERPRINT_REVISION,
    CORE_REPOSITORY_OBSERVATION_REVISION, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use git::{
    negative_route_geometry_state, CandidateKind, CertifiedCandidate, EventProbeBudget,
    GitCertifier, ProbeFailure,
};
pub(crate) use outcome::{linked_outcome_evidence, LinkedOutcomeEvidence, LinkedOutcomeInput};
use shell::analyze;
pub(crate) use shell::{
    bounded_outcome_evidence_relevant, bounded_outcome_plan, lexical_absolute,
    BoundedOutcomeOperation, BoundedOutcomePlan, BoundedOutcomePlanDisposition,
};

pub(crate) const ASSOCIATION_POLICY_REVISION: u32 = 2;
const MAX_PROVIDER_NATIVE_IDENTITIES: usize = 16;
const MAX_REPOSITORY_CANDIDATES: usize = 32;
const MAX_POSITIVE_CERTIFICATION_CACHE_ENTRIES: usize = 32;
const MAX_NEGATIVE_CERTIFICATION_CACHE_ENTRIES: usize = 64;

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
    pub(crate) activity_at_unix_ms: Option<i64>,
    pub(crate) provider_native_repository_aliases: Vec<RepositoryAlias>,
    pub(crate) session_cwd: Option<String>,
    pub(crate) declared_tool_workdir: Option<String>,
    pub(crate) command: Option<String>,
    pub(crate) structured_content: Option<Value>,
    pub(crate) file_observations: Vec<UnscopedFileObservation>,
    pub(crate) vcs_observations: Vec<UnscopedVcsObservation>,
    pub(crate) outcome_operation_repository_path: Option<String>,
    pub(crate) outcome_output_repository_path: Option<String>,
    pub(crate) outcome_observations: Vec<RepositoryOutcomeObservation>,
    pub(crate) outcome_abstentions: Vec<(RepositoryAbstentionReason, &'static str)>,
}

#[derive(Debug, Default)]
pub(crate) struct RepositoryAttributor {
    certifier: GitCertifier,
    positive_cache: Vec<CachedPositiveCertificate>,
    negative_cache: Vec<CachedNegativeCertificate>,
    cache_clock: u64,
}

pub(crate) fn apply_annotation(
    record: &mut ctx_history_core::CoreRecord,
    annotation: CoreRecordAnnotation,
) {
    record.content.structured_content = annotation.structured_content;
    record.metadata = annotation.metadata;
    record.repository_candidate_evidence = annotation.repository_candidate_evidence;
    record.repository_bindings = annotation.repository_bindings;
    record.repository_abstentions = annotation.repository_abstentions;
    record.repository_file_observations = annotation.repository_file_observations;
    record.repository_vcs_observations = annotation.repository_vcs_observations;
}

#[derive(Debug, Clone)]
struct CachedPositiveCertificate {
    certificate: CertifiedCandidate,
    last_used: u64,
}

#[derive(Debug, Clone)]
struct CachedNegativeCertificate {
    path: PathBuf,
    kind: CandidateKind,
    route_geometry_state: [u8; 32],
    failure: ProbeFailure,
    last_used: u64,
}

impl RepositoryAttributor {
    pub(crate) fn attribute(&mut self, input: AttributionInput) -> CoreRecordAnnotation {
        // Certificates authorize the route observed during this event only.
        // A later event must revalidate after a move, replacement, or removal.
        attribute_with_attributor(input, self)
    }

    fn certify(
        &mut self,
        candidate: &Candidate,
        observed_at_unix_ms: i64,
        budget: &mut EventProbeBudget,
    ) -> Result<CertifiedCandidate, ProbeFailure> {
        self.cache_clock = self.cache_clock.saturating_add(1);
        let now = self.cache_clock;
        let positive = self
            .positive_cache
            .iter()
            .enumerate()
            .filter(|(_, cached)| cached.certificate.lexical_root_contains(&candidate.path))
            .max_by_key(|(_, cached)| cached.certificate.repository_root.components().count())
            .map(|(index, _)| index);
        if let Some(index) = positive {
            let cached = self.positive_cache[index].certificate.clone();
            match cached.try_reuse(
                &candidate.path,
                candidate.kind,
                candidate.evidence_kind,
                observed_at_unix_ms,
            ) {
                Ok(Some(reused)) => {
                    self.positive_cache[index].last_used = now;
                    return Ok(reused);
                }
                Ok(None) => {
                    self.positive_cache.remove(index);
                }
                Err(failure) => return Err(failure),
            }
        }

        if let Some(state) = negative_route_geometry_state(&candidate.path, candidate.kind) {
            if let Some(index) = self.negative_cache.iter().position(|cached| {
                cached.path == candidate.path
                    && cached.kind == candidate.kind
                    && cached.route_geometry_state == state
            }) {
                self.negative_cache[index].last_used = now;
                return Err(self.negative_cache[index].failure.clone());
            }
            self.negative_cache
                .retain(|cached| cached.path != candidate.path || cached.kind != candidate.kind);
        }

        let result = self.certifier.certify_at_with_budget(
            &candidate.path,
            candidate.kind,
            candidate.evidence_kind,
            observed_at_unix_ms,
            budget,
        );
        match &result {
            Ok(certificate) => {
                self.negative_cache.retain(|cached| {
                    cached.path != candidate.path || cached.kind != candidate.kind
                });
                self.positive_cache.retain(|cached| {
                    cached.certificate.repository_root != certificate.repository_root
                });
                self.positive_cache.push(CachedPositiveCertificate {
                    certificate: certificate.clone(),
                    last_used: now,
                });
                evict_oldest_positive(&mut self.positive_cache);
            }
            Err(failure) if cacheable_negative(failure) => {
                if let Some(state) = negative_route_geometry_state(&candidate.path, candidate.kind)
                {
                    self.negative_cache.push(CachedNegativeCertificate {
                        path: candidate.path.clone(),
                        kind: candidate.kind,
                        route_geometry_state: state,
                        failure: failure.clone(),
                        last_used: now,
                    });
                    evict_oldest_negative(&mut self.negative_cache);
                }
            }
            Err(_) => {}
        }
        result
    }

    pub(crate) fn full_certification_probe_count(&self) -> usize {
        self.certifier.full_certification_probe_count()
    }

    pub(crate) fn git_subprocess_count(&self) -> usize {
        self.certifier.git_subprocess_count()
    }
}

fn cacheable_negative(failure: &ProbeFailure) -> bool {
    matches!(
        failure,
        ProbeFailure::Missing
            | ProbeFailure::Failed(
                "git_command_failed" | "unexpected_git_geometry" | "unsupported_git_object_format"
            )
    )
}

fn evict_oldest_positive(cache: &mut Vec<CachedPositiveCertificate>) {
    if cache.len() <= MAX_POSITIVE_CERTIFICATION_CACHE_ENTRIES {
        return;
    }
    if let Some(index) = cache
        .iter()
        .enumerate()
        .min_by_key(|(_, cached)| {
            (
                cached.last_used,
                cached.certificate.repository_root.as_os_str(),
            )
        })
        .map(|(index, _)| index)
    {
        cache.remove(index);
    }
}

fn evict_oldest_negative(cache: &mut Vec<CachedNegativeCertificate>) {
    if cache.len() <= MAX_NEGATIVE_CERTIFICATION_CACHE_ENTRIES {
        return;
    }
    if let Some(index) = cache
        .iter()
        .enumerate()
        .min_by_key(|(_, cached)| (cached.last_used, cached.path.as_os_str()))
        .map(|(index, _)| index)
    {
        cache.remove(index);
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
    let activity_at_unix_ms = input
        .activity_at_unix_ms
        .unwrap_or(CORE_MISSING_ACTIVITY_TIME_UNIX_MS);
    let mut outcome_observations = input.outcome_observations.clone();
    let outcome_operation_repository_path = input.outcome_operation_repository_path.clone();
    let outcome_output_repository_path = input.outcome_output_repository_path.clone();
    let mut annotation = CoreRecordAnnotation {
        structured_content: input.structured_content,
        metadata: BTreeMap::from([(
            "repository_association".to_owned(),
            json!({
                "association_policy_revision": ASSOCIATION_POLICY_REVISION,
                "repository_observation_revision": CORE_REPOSITORY_OBSERVATION_REVISION,
                "bounded_shell_subset_revision": CORE_BOUNDED_SHELL_SUBSET_REVISION,
                "outcome_capture_revision": CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
                "locator_fingerprint_revision": CORE_REPOSITORY_LOCATOR_FINGERPRINT_REVISION,
                "candidate_source": "bounded_structured_activity",
            }),
        )]),
        ..CoreRecordAnnotation::default()
    };
    for (reason, detail) in input.outcome_abstentions.iter().copied() {
        push_abstention(
            &mut annotation,
            RepositoryEvidenceKind::ProviderNativeResult,
            reason,
            detail,
        );
    }
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
    let outcome_operation_path = bounded_absolute(
        outcome_operation_repository_path.as_deref(),
        RepositoryEvidenceKind::ProviderNativeResult,
        &mut annotation.repository_abstentions,
    );
    let outcome_output_path = bounded_absolute(
        outcome_output_repository_path.as_deref(),
        RepositoryEvidenceKind::ProviderNativeResult,
        &mut annotation.repository_abstentions,
    );
    annotation
        .repository_candidate_evidence
        .outcome_operation_repository_path = outcome_operation_path.as_deref().map(path_string);
    annotation
        .repository_candidate_evidence
        .outcome_output_repository_path = outcome_output_path.as_deref().map(path_string);

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

    if command_analysis.blocks_session_fallback && !outcome_observations.is_empty() {
        push_abstention(
            &mut annotation,
            RepositoryEvidenceKind::ProviderNativeResult,
            RepositoryAbstentionReason::OutcomeRepositoryUnbound,
            "opaque_command_has_no_certified_operation_route",
        );
        outcome_observations.clear();
    }

    if command_analysis
        .abstentions
        .iter()
        .any(|abstention| abstention.reason == RepositoryAbstentionReason::CandidateLimitExceeded)
    {
        if !outcome_observations.is_empty() {
            push_abstention(
                &mut annotation,
                RepositoryEvidenceKind::ProviderNativeResult,
                RepositoryAbstentionReason::OutcomeRepositoryUnbound,
                "repository_outcome_blocked_by_command_candidate_limit",
            );
        }
        return annotation;
    }

    let requested_candidate_count = command_analysis
        .repository_paths
        .len()
        .saturating_add(input.file_observations.len())
        .saturating_add(input.vcs_observations.len())
        .saturating_add(usize::from(input.declared_tool_workdir.is_some()))
        .saturating_add(usize::from(outcome_operation_path.is_some()));
    if requested_candidate_count > MAX_REPOSITORY_CANDIDATES {
        push_abstention(
            &mut annotation,
            RepositoryEvidenceKind::ProviderNativeResult,
            RepositoryAbstentionReason::CandidateLimitExceeded,
            "repository_candidate_product_limit_exceeded",
        );
        if !outcome_observations.is_empty() {
            push_abstention(
                &mut annotation,
                RepositoryEvidenceKind::ProviderNativeResult,
                RepositoryAbstentionReason::OutcomeRepositoryUnbound,
                "repository_outcome_blocked_by_candidate_limit",
            );
        }
        return annotation;
    }

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
    } else if !command_analysis.blocks_session_fallback {
        if let Some(workdir) = &declared_workdir {
            candidates.push(Candidate {
                path: workdir.clone(),
                kind: CandidateKind::Directory,
                evidence_kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
            });
        }
    }
    if !command_analysis.blocks_session_fallback {
        if let Some(path) = &outcome_operation_path {
            candidates.push(Candidate {
                path: path.clone(),
                kind: CandidateKind::Directory,
                evidence_kind: RepositoryEvidenceKind::ProviderNativeResult,
            });
        }
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
        || !vcs_inputs.is_empty()
        || outcome_operation_path.is_some();
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
    let mut probe_budget = EventProbeBudget::new();
    for candidate in candidates {
        match attributor.certify(&candidate, activity_at_unix_ms, &mut probe_budget) {
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
    scope_outcomes(
        &mut annotation,
        &certified,
        outcome_observations,
        outcome_operation_path.as_deref(),
        outcome_output_path.as_deref(),
    );
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
        ProbeFailure::BudgetExceeded => (
            RepositoryAbstentionReason::ProbeBudgetExceeded,
            "per_event_git_probe_budget_exceeded",
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
        return;
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

fn scope_outcomes(
    annotation: &mut CoreRecordAnnotation,
    certified: &[CertifiedCandidate],
    outcomes: Vec<RepositoryOutcomeObservation>,
    operation_path: Option<&Path>,
    output_path: Option<&Path>,
) {
    if outcomes.is_empty() {
        return;
    }
    let Some(operation_path) = operation_path else {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeResult,
            RepositoryAbstentionReason::OutcomeRepositoryUnbound,
            "repository_outcome_has_no_operation_route",
        );
        return;
    };
    if output_path.is_some_and(|output_path| output_path != operation_path) {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeResult,
            RepositoryAbstentionReason::ConflictingIdentity,
            "repository_outcome_operation_and_output_routes_conflict",
        );
        return;
    }

    let selected_binding_id = most_specific_certificate(certified, operation_path)
        .map(|certificate| certificate.binding.binding_id.clone());
    let Some(selected_binding_id) = selected_binding_id else {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeResult,
            RepositoryAbstentionReason::OutcomeRepositoryUnbound,
            "repository_outcome_route_has_no_single_certified_binding",
        );
        return;
    };
    let Some(binding_index) = annotation
        .repository_bindings
        .iter()
        .position(|binding| binding.binding_id == selected_binding_id)
    else {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeResult,
            RepositoryAbstentionReason::OutcomeRepositoryUnbound,
            "repository_outcome_certificate_is_not_in_event_bindings",
        );
        return;
    };

    let observed_formats = outcomes
        .iter()
        .flat_map(RepositoryOutcomeObservation::object_ids)
        .map(|object_id| object_id.format)
        .collect::<std::collections::HashSet<_>>();
    if observed_formats.len() > 1 {
        push_abstention(
            annotation,
            RepositoryEvidenceKind::ProviderNativeResult,
            RepositoryAbstentionReason::Unsafe,
            "repository_outcome_object_formats_conflict",
        );
        return;
    }
    let observed_format = observed_formats.iter().next().copied();
    let binding = &mut annotation.repository_bindings[binding_index];
    match (binding.git_object_format, observed_format) {
        (Some(binding_format), Some(outcome_format)) if binding_format != outcome_format => {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::ProviderNativeResult,
                RepositoryAbstentionReason::Unsafe,
                "repository_outcome_object_format_mismatch",
            );
            return;
        }
        (None, Some(outcome_format)) => binding.git_object_format = Some(outcome_format),
        _ => {}
    }
    let evidence = RepositoryEvidence {
        kind: RepositoryEvidenceKind::ProviderNativeResult,
        confidence: RepositoryEvidenceConfidence::Explicit,
    };
    if !binding.evidence.contains(&evidence) {
        binding.evidence.push(evidence);
    }
    let binding_id = binding.binding_id.clone();
    annotation
        .repository_vcs_observations
        .extend(
            outcomes
                .into_iter()
                .map(|outcome| RepositoryVcsObservation {
                    repository_binding_id: binding_id.clone(),
                    kind: RepositoryVcsObservationKind::Outcome(Box::new(outcome)),
                    object_id: None,
                    parent_object_ids: Vec::new(),
                    reference: None,
                    relative_path: None,
                }),
        );
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
