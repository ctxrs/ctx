mod git;
mod outcome;
mod scoping;
mod shell;

use std::{collections::BTreeMap, fmt::Write, path::PathBuf};

use ctx_history_core::{
    CoreRecordAnnotation, GitObjectId, RepositoryAbstention, RepositoryAbstentionReason,
    RepositoryAlias, RepositoryAliasKind, RepositoryBinding, RepositoryCandidateKind,
    RepositoryEvidence, RepositoryEvidenceConfidence, RepositoryEvidenceKind,
    RepositoryFileObservation, RepositoryFileObservationKind, RepositoryOutcomeKind,
    RepositoryOutcomeObservation, RepositoryVcsObservation, RepositoryVcsObservationKind,
    CORE_BOUNDED_SHELL_SUBSET_REVISION, CORE_MISSING_ACTIVITY_TIME_UNIX_MS,
    CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
    CORE_REPOSITORY_OBSERVATION_REVISION, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use git::{
    negative_route_geometry_state, CandidateKind, CertifiedCandidate, EventProbeBudget,
    GitCertifier, ProbeFailure, ResolvedCommitProducer,
};
pub(crate) use outcome::{
    linked_outcome_evidence, LinkedOutcomeEvidence, LinkedOutcomeInput, UnscopedOutcomeObservation,
};
use scoping::{path_string, scope_files, scope_outcomes, scope_vcs};
use shell::{analyze, command_too_large};
pub(crate) use shell::{
    bounded_outcome_evidence_relevant, bounded_outcome_plan, lexical_absolute,
    BoundedCommitProducer, BoundedOutcomeOperation, BoundedOutcomePlan,
    BoundedOutcomePlanDisposition, MAX_COMMAND_BYTES,
};

const MAX_PROVIDER_NATIVE_IDENTITIES: usize = 16;
const MAX_REPOSITORY_CANDIDATES: usize = 32;
const MAX_POSITIVE_CERTIFICATION_CACHE_ENTRIES: usize = 32;
const MAX_NEGATIVE_CERTIFICATION_CACHE_ENTRIES: usize = 64;
const MAX_EVENT_TIME_CERTIFICATION_ENTRIES: usize = 256;

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
    pub(crate) command_disposition: CommandEvidenceDisposition,
    pub(crate) provider_native_context_ambiguous: bool,
    pub(crate) structured_content: Option<Value>,
    pub(crate) file_observations: Vec<UnscopedFileObservation>,
    pub(crate) vcs_observations: Vec<UnscopedVcsObservation>,
    pub(crate) outcome_operation_repository_path: Option<String>,
    pub(crate) outcome_output_repository_path: Option<String>,
    pub(crate) outcome_observations: Vec<UnscopedOutcomeObservation>,
    pub(crate) outcome_abstentions: Vec<(RepositoryAbstentionReason, &'static str)>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandEvidenceDisposition {
    #[default]
    Analyze,
    CommandTooLarge,
}

#[derive(Debug, Default)]
pub(crate) struct RepositoryAttributor {
    certifier: GitCertifier,
    positive_cache: Vec<CachedPositiveCertificate>,
    negative_cache: Vec<CachedNegativeCertificate>,
    event_time_cache: Vec<CachedEventTimeCertificate>,
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

#[derive(Debug, Clone)]
struct CachedEventTimeCertificate {
    path: PathBuf,
    kind: CandidateKind,
    observed_at_unix_ms: i64,
    certificate: CertifiedCandidate,
    certified_move_at_unix_ms: Option<i64>,
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
                    self.record_live_event_time_certificate(candidate, &reused, now)?;
                    self.positive_cache[index].last_used = now;
                    return Ok(reused);
                }
                Ok(None) => {
                    self.positive_cache.remove(index);
                }
                Err(ProbeFailure::Missing) => {
                    if let Some(reused) =
                        self.try_reuse_moved_event_time_certificate(candidate, now)?
                    {
                        return Ok(reused);
                    }
                    return Err(ProbeFailure::Missing);
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
                let failure = self.negative_cache[index].failure.clone();
                if failure == ProbeFailure::Missing {
                    if let Some(reused) =
                        self.try_reuse_moved_event_time_certificate(candidate, now)?
                    {
                        return Ok(reused);
                    }
                }
                return Err(failure);
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
        let result = match result {
            Ok(certificate) => {
                self.record_live_event_time_certificate(candidate, &certificate, now)?;
                Ok(certificate)
            }
            Err(ProbeFailure::Missing) => {
                match self.try_reuse_moved_event_time_certificate(candidate, now)? {
                    Some(reused) => Ok(reused),
                    None => Err(ProbeFailure::Missing),
                }
            }
            Err(failure) => Err(failure),
        };
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

    fn record_live_event_time_certificate(
        &mut self,
        candidate: &Candidate,
        certificate: &CertifiedCandidate,
        now: u64,
    ) -> Result<(), ProbeFailure> {
        let observed_at_unix_ms = certificate.observed_at_unix_ms();
        if observed_at_unix_ms == CORE_MISSING_ACTIVITY_TIME_UNIX_MS {
            return Ok(());
        }

        if self.event_time_cache.iter().any(|cached| {
            cached.path == candidate.path
                && cached.kind == candidate.kind
                && cached.observed_at_unix_ms == observed_at_unix_ms
                && !cached.certificate.same_binding_identity(certificate)
        }) {
            return Err(ProbeFailure::ConflictingEventTimeIdentity);
        }

        for cached in &mut self.event_time_cache {
            if cached.certificate.repository_root == certificate.repository_root
                || cached.observed_at_unix_ms >= observed_at_unix_ms
                || !cached.certificate.same_binding_identity(certificate)
                || !cached
                    .certificate
                    .same_local_root_authorization_identity(certificate)
            {
                continue;
            }
            if matches!(
                git::validate_candidate_route(
                    &cached.certificate.repository_root,
                    CandidateKind::Directory,
                ),
                Err(ProbeFailure::Missing)
            ) {
                cached.certified_move_at_unix_ms = Some(
                    cached
                        .certified_move_at_unix_ms
                        .map_or(observed_at_unix_ms, |existing| {
                            existing.min(observed_at_unix_ms)
                        }),
                );
            }
        }

        if let Some(cached) = self.event_time_cache.iter_mut().find(|cached| {
            cached.path == candidate.path
                && cached.kind == candidate.kind
                && cached.observed_at_unix_ms == observed_at_unix_ms
                && cached.certificate.same_binding_identity(certificate)
        }) {
            cached.certificate = certificate.clone();
            cached.last_used = now;
            return Ok(());
        }
        self.event_time_cache.push(CachedEventTimeCertificate {
            path: candidate.path.clone(),
            kind: candidate.kind,
            observed_at_unix_ms,
            certificate: certificate.clone(),
            certified_move_at_unix_ms: None,
            last_used: now,
        });
        evict_oldest_event_time(&mut self.event_time_cache);
        Ok(())
    }

    fn try_reuse_moved_event_time_certificate(
        &mut self,
        candidate: &Candidate,
        now: u64,
    ) -> Result<Option<CertifiedCandidate>, ProbeFailure> {
        let observed_at_unix_ms = candidate.observed_at_unix_ms;
        if observed_at_unix_ms == CORE_MISSING_ACTIVITY_TIME_UNIX_MS {
            return Ok(None);
        }
        let matching = self
            .event_time_cache
            .iter()
            .enumerate()
            .filter(|(_, cached)| {
                cached.path == candidate.path
                    && cached.kind == candidate.kind
                    && cached.observed_at_unix_ms == observed_at_unix_ms
                    && cached
                        .certified_move_at_unix_ms
                        .is_some_and(|moved_at| moved_at > observed_at_unix_ms)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let Some((&first, rest)) = matching.split_first() else {
            return Ok(None);
        };
        if rest.iter().any(|index| {
            !self.event_time_cache[*index]
                .certificate
                .same_binding_identity(&self.event_time_cache[first].certificate)
        }) {
            return Err(ProbeFailure::ConflictingEventTimeIdentity);
        }
        let mut reused = self.event_time_cache[first]
            .certificate
            .for_event(candidate.evidence_kind, observed_at_unix_ms);
        // The historical certificate proves stable repository identity at the
        // event's timestamp. It does not re-authorize a route that is missing
        // now, so never project its former local-root authorization.
        reused.binding.local_root_authorization = None;
        for index in matching {
            self.event_time_cache[index].last_used = now;
        }
        Ok(Some(reused))
    }

    pub(crate) fn full_certification_probe_count(&self) -> usize {
        self.certifier.full_certification_probe_count()
    }

    #[cfg(test)]
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

fn evict_oldest_event_time(cache: &mut Vec<CachedEventTimeCertificate>) {
    if cache.len() <= MAX_EVENT_TIME_CERTIFICATION_ENTRIES {
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
    observed_at_unix_ms: i64,
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

impl ProviderIdentityResolution {
    fn was_attempted(&self) -> bool {
        !matches!(self, Self::Absent)
    }

    fn binds_all_outcomes(&self, outcomes: &[UnscopedOutcomeObservation]) -> bool {
        let Self::Binding(binding) = self else {
            return false;
        };
        !outcomes.is_empty()
            && outcomes.iter().all(|outcome| {
                let UnscopedOutcomeObservation::Exact(outcome) = outcome else {
                    return false;
                };
                outcome.pull_request.as_ref().is_some_and(|pull_request| {
                    binding.aliases.contains(&pull_request.forge_repository)
                })
            })
    }
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
    let provider_native_context_ambiguous = input.provider_native_context_ambiguous;
    let outcome_operation_repository_path = input.outcome_operation_repository_path.clone();
    let outcome_output_repository_path = input.outcome_output_repository_path.clone();
    let mut annotation = CoreRecordAnnotation {
        structured_content: input.structured_content,
        metadata: BTreeMap::from([(
            "repository_association".to_owned(),
            json!({
                "association_policy_revision": CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
                "repository_observation_revision": CORE_REPOSITORY_OBSERVATION_REVISION,
                "bounded_shell_subset_revision": CORE_BOUNDED_SHELL_SUBSET_REVISION,
                "outcome_capture_revision": CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
                "local_root_authorization_fingerprint_revision": CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
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
    if let Some(path) = &session_cwd {
        annotation
            .repository_candidate_evidence
            .insert(RepositoryCandidateKind::SessionCwd, path_string(path));
    }
    if let Some(path) = &declared_workdir {
        annotation.repository_candidate_evidence.insert(
            RepositoryCandidateKind::DeclaredToolWorkdir,
            path_string(path),
        );
    }
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
    if let Some(path) = &outcome_operation_path {
        annotation.repository_candidate_evidence.insert(
            RepositoryCandidateKind::OutcomeOperationRepositoryPath,
            path_string(path),
        );
    }
    if let Some(path) = &outcome_output_path {
        annotation.repository_candidate_evidence.insert(
            RepositoryCandidateKind::OutcomeOutputRepositoryPath,
            path_string(path),
        );
    }

    let base = declared_workdir.as_deref().or(session_cwd.as_deref());
    let command_analysis = match input.command_disposition {
        CommandEvidenceDisposition::Analyze => analyze(input.command.as_deref(), base),
        CommandEvidenceDisposition::CommandTooLarge => command_too_large(),
    };
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
                    association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
                }),
        );
    if let Some(path) = &command_analysis.derived_effective_cwd {
        annotation.repository_candidate_evidence.insert(
            RepositoryCandidateKind::DerivedEffectiveCwd,
            path_string(path),
        );
    }
    for candidate in &command_analysis.repository_paths {
        let kind = match candidate.evidence_kind {
            RepositoryEvidenceKind::DerivedEffectiveCwd => {
                RepositoryCandidateKind::DerivedEffectiveCwd
            }
            RepositoryEvidenceKind::CommandSpecificRepositoryPath => {
                RepositoryCandidateKind::CommandSpecificRepositoryPath
            }
            _ => continue,
        };
        annotation
            .repository_candidate_evidence
            .insert(kind, path_string(&candidate.path));
    }

    let outcomes_have_provider_binding =
        provider_identity.binds_all_outcomes(&outcome_observations);
    if command_analysis.blocks_session_fallback
        && !outcome_observations.is_empty()
        && !outcomes_have_provider_binding
    {
        push_abstention(
            &mut annotation,
            RepositoryEvidenceKind::ProviderNativeResult,
            RepositoryAbstentionReason::OutcomeRepositoryUnbound,
            "opaque_command_has_no_certified_operation_route",
        );
        outcome_observations.clear();
    }

    let command_candidate_limit_exceeded = command_analysis
        .abstentions
        .iter()
        .any(|abstention| abstention.reason == RepositoryAbstentionReason::CandidateLimitExceeded);
    let admitted_command_candidate_count = if command_candidate_limit_exceeded {
        0
    } else {
        command_analysis.repository_paths.len()
    };

    let requested_candidate_count = admitted_command_candidate_count
        .saturating_add(input.file_observations.len())
        .saturating_add(input.vcs_observations.len())
        .saturating_add(usize::from(input.declared_tool_workdir.is_some()))
        .saturating_add(usize::from(outcome_operation_path.is_some()));

    let mut candidates = Vec::new();
    let more_specific_command =
        !command_candidate_limit_exceeded && !command_analysis.repository_paths.is_empty();
    if more_specific_command {
        candidates.extend(
            command_analysis
                .repository_paths
                .iter()
                .map(|candidate| Candidate {
                    path: candidate.path.clone(),
                    kind: CandidateKind::Directory,
                    evidence_kind: candidate.evidence_kind,
                    observed_at_unix_ms: activity_at_unix_ms,
                }),
        );
    }
    if let Some(workdir) = &declared_workdir {
        candidates.push(Candidate {
            path: workdir.clone(),
            kind: CandidateKind::Directory,
            evidence_kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
            observed_at_unix_ms: activity_at_unix_ms,
        });
    }
    if !command_analysis.blocks_session_fallback {
        if let Some(path) = &outcome_operation_path {
            candidates.push(Candidate {
                path: path.clone(),
                kind: CandidateKind::Directory,
                evidence_kind: RepositoryEvidenceKind::ProviderNativeResult,
                observed_at_unix_ms: activity_at_unix_ms,
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
        annotation.repository_candidate_evidence.insert(
            RepositoryCandidateKind::FileActivityPath,
            path_string(&path),
        );
        if let Some(prior_path) = &prior_path {
            annotation.repository_candidate_evidence.insert(
                RepositoryCandidateKind::FileActivityPath,
                path_string(prior_path),
            );
        }
        candidates.push(Candidate {
            path: path.clone(),
            kind: CandidateKind::File,
            evidence_kind: RepositoryEvidenceKind::FileActivity,
            observed_at_unix_ms: activity_at_unix_ms,
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
            annotation
                .repository_candidate_evidence
                .insert(RepositoryCandidateKind::VcsActivityPath, path_string(path));
            candidates.push(Candidate {
                path: path.clone(),
                kind: CandidateKind::Directory,
                evidence_kind: RepositoryEvidenceKind::VcsActivity,
                observed_at_unix_ms: activity_at_unix_ms,
            });
        }
        vcs_inputs.push(ScopedVcsInput { path, observation });
    }

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

    let attempted_specific = input.declared_tool_workdir.is_some()
        || more_specific_command
        || command_analysis.blocks_session_fallback
        || provider_identity.was_attempted()
        || provider_native_context_ambiguous
        || !file_inputs.is_empty()
        || !vcs_inputs.is_empty()
        || outcome_operation_path.is_some();
    if candidates.is_empty() && !attempted_specific {
        if let Some(cwd) = &session_cwd {
            candidates.push(Candidate {
                path: cwd.clone(),
                kind: CandidateKind::Directory,
                evidence_kind: RepositoryEvidenceKind::SessionCwd,
                observed_at_unix_ms: activity_at_unix_ms,
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
    let outcome_observations = resolve_deferred_commit_observations(
        &mut annotation,
        &certified,
        outcome_observations,
        outcome_operation_path.as_deref(),
        &attributor.certifier,
        &mut probe_budget,
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

fn resolve_deferred_commit_observations(
    annotation: &mut CoreRecordAnnotation,
    certified: &[CertifiedCandidate],
    observations: Vec<UnscopedOutcomeObservation>,
    operation_path: Option<&std::path::Path>,
    certifier: &GitCertifier,
    budget: &mut EventProbeBudget,
) -> Vec<RepositoryOutcomeObservation> {
    let mut exact = Vec::new();
    for observation in observations {
        let deferred = match observation {
            UnscopedOutcomeObservation::Exact(outcome) => {
                exact.push(outcome);
                continue;
            }
            UnscopedOutcomeObservation::DeferredCommit(deferred) => deferred,
        };
        let Some(operation_path) = operation_path else {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::ProviderNativeResult,
                RepositoryAbstentionReason::OutcomeRepositoryUnbound,
                "deferred_commit_has_no_operation_route",
            );
            continue;
        };
        let Some(certificate) = certified
            .iter()
            .filter(|certificate| operation_path.starts_with(&certificate.repository_root))
            .max_by_key(|certificate| certificate.repository_root.components().count())
        else {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::ProviderNativeResult,
                RepositoryAbstentionReason::OutcomeRepositoryUnbound,
                "deferred_commit_route_has_no_certified_binding",
            );
            continue;
        };
        let producer = match (deferred.producer, deferred.rewrites_history) {
            (_, true) => ResolvedCommitProducer::Rewrite,
            (BoundedCommitProducer::Commit, false) => ResolvedCommitProducer::Commit,
            (BoundedCommitProducer::Merge, false) => ResolvedCommitProducer::Merge,
            (BoundedCommitProducer::Rebase, false) => {
                push_abstention(
                    annotation,
                    RepositoryEvidenceKind::ProviderNativeResult,
                    RepositoryAbstentionReason::OutcomeResultInadmissible,
                    "deferred_rebase_result_is_not_supported",
                );
                continue;
            }
        };
        let resolved = match certifier.resolve_commit(
            certificate,
            &deferred.oid_prefix,
            &deferred.subject,
            producer,
            budget,
        ) {
            Ok(resolved) => resolved,
            Err(ProbeFailure::BudgetExceeded) => {
                push_probe_failure(
                    annotation,
                    RepositoryEvidenceKind::ProviderNativeResult,
                    ProbeFailure::BudgetExceeded,
                    false,
                );
                continue;
            }
            Err(ProbeFailure::ConcurrentDrift | ProbeFailure::Missing) => {
                push_abstention(
                    annotation,
                    RepositoryEvidenceKind::ProviderNativeResult,
                    RepositoryAbstentionReason::ConcurrentDrift,
                    "deferred_commit_repository_changed_during_resolution",
                );
                continue;
            }
            Err(ProbeFailure::PlatformUnsupported) => {
                push_probe_failure(
                    annotation,
                    RepositoryEvidenceKind::ProviderNativeResult,
                    ProbeFailure::PlatformUnsupported,
                    false,
                );
                continue;
            }
            Err(_) => {
                push_abstention(
                    annotation,
                    RepositoryEvidenceKind::ProviderNativeResult,
                    RepositoryAbstentionReason::OutcomeResultInadmissible,
                    "deferred_commit_did_not_resolve_exactly",
                );
                continue;
            }
        };

        let binding_id = certificate.binding.binding_id.clone();
        annotation
            .repository_vcs_observations
            .push(RepositoryVcsObservation {
                repository_binding_id: binding_id.clone(),
                kind: RepositoryVcsObservationKind::Commit,
                object_id: Some(resolved.object_id.clone()),
                parent_object_ids: resolved.parent_object_ids.clone(),
                reference: None,
                relative_path: None,
            });
        annotation
            .repository_file_observations
            .extend(
                resolved
                    .files
                    .into_iter()
                    .map(|file| RepositoryFileObservation {
                        repository_binding_id: binding_id.clone(),
                        relative_path: file.path,
                        kind: file.kind,
                        prior_relative_path: file.prior_path,
                    }),
            );
        if !deferred.rewrites_history {
            exact.push(RepositoryOutcomeObservation {
                kind: RepositoryOutcomeKind::Commit,
                produced_object_ids: vec![resolved.object_id],
                replacement_lineage: Vec::new(),
                pull_request: None,
                observed_at_unix_ms: deferred.observed_at_unix_ms,
                linkage: deferred.linkage,
                outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
            });
        }
    }
    exact
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
                association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
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
        ProbeFailure::ConflictingEventTimeIdentity => (
            RepositoryAbstentionReason::ConflictingIdentity,
            "event_time_certificate_conflicts_with_current_route",
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
    certified: &mut [CertifiedCandidate],
    resolution: ProviderIdentityResolution,
) {
    let provider = match resolution {
        ProviderIdentityResolution::Absent => return,
        // Invalid or ambiguous provider metadata is an abstention for that
        // evidence lane only. Independently certified structured activity
        // remains valid and must not be erased.
        ProviderIdentityResolution::Abstained => return,
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
        // Provider-native identity has precedence for its own exact outcome
        // segment. Other structured activity lanes remain independent and may
        // certify additional repositories in the same event.
        annotation.repository_bindings.push(*provider);
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
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
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
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    };
    if !annotation.repository_abstentions.contains(&abstention) {
        annotation.repository_abstentions.push(abstention);
    }
}

#[cfg(test)]
mod tests;
