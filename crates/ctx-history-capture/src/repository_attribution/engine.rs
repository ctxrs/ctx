use std::{collections::BTreeMap, path::PathBuf};

use ctx_history_core::{
    CoreRecordAnnotation, RepositoryAbstention, RepositoryAbstentionReason,
    RepositoryCandidateKind, RepositoryEvidenceKind, RepositoryFileObservation,
    RepositoryFileObservationKind, RepositoryOutcomeKind, RepositoryOutcomeObservation,
    RepositoryVcsObservation, RepositoryVcsObservationKind, CORE_BOUNDED_SHELL_SUBSET_REVISION,
    CORE_MISSING_ACTIVITY_TIME_UNIX_MS, CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
    CORE_REPOSITORY_OBSERVATION_REVISION, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
};
use serde_json::json;

use super::{
    attributor::RepositoryAttributor,
    git::{
        CandidateKind, CertifiedCandidate, EventProbeBudget, GitCertifier, ProbeFailure,
        ResolvedCommitProducer,
    },
    identity::{
        push_abstention, reconcile_provider_identity, resolve_provider_native_identity,
        ProviderIdentityResolution,
    },
    outcome::UnscopedOutcomeObservation,
    scoping::{path_string, scope_files, scope_outcomes, scope_vcs},
    shell::{analyze, command_too_large, lexical_absolute},
    AttributionInput, BoundedCommitProducer, CommandEvidenceDisposition, UnscopedVcsObservation,
};

const MAX_REPOSITORY_CANDIDATES: usize = 32;

#[derive(Debug, Clone)]
pub(super) struct Candidate {
    pub(super) path: PathBuf,
    pub(super) kind: CandidateKind,
    pub(super) evidence_kind: RepositoryEvidenceKind,
    pub(super) observed_at_unix_ms: i64,
}

#[derive(Debug, Clone)]
pub(super) struct ScopedFileInput {
    pub(super) path: PathBuf,
    pub(super) prior_path: Option<PathBuf>,
    pub(super) kind: RepositoryFileObservationKind,
}

#[derive(Debug, Clone)]
pub(super) struct ScopedVcsInput {
    pub(super) path: Option<PathBuf>,
    pub(super) observation: UnscopedVcsObservation,
}

#[cfg(test)]
pub(crate) fn attribute(input: AttributionInput) -> CoreRecordAnnotation {
    RepositoryAttributor::default().attribute(input)
}

pub(super) fn attribute_with_attributor(
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
