use super::*;
use crate::LiteralPullRequestAssociationObservation;

pub(super) fn enrich_pull_request_associations(
    annotation: &mut RepositoryEvaluation,
    certified: &[CertifiedCandidate],
    associations: Vec<LiteralPullRequestAssociationObservation>,
    certifier: &GitCertifier,
    budget: &mut EventProbeBudget,
) -> Vec<(
    Option<String>,
    crate::model::RepositoryPullRequestAssociationObservation,
)> {
    let mut published = Vec::new();
    for source in associations {
        let mut association = crate::model::RepositoryPullRequestAssociationObservation {
            pull_request: source.pull_request,
            merged_as: source.merged_as,
            contains_commits: Vec::new(),
            linkage: source.linkage,
            association_capture_revision: PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION,
        };
        let operation_path = std::path::Path::new(&source.repository_path);
        let matching = certified
            .iter()
            .filter(|certificate| {
                operation_path.starts_with(&certificate.repository_root)
                    && certificate.binding.git_object_format == Some(association.merged_as.format)
                    && binding_accepts_forge_repository(
                        &certificate.binding,
                        &association.pull_request.forge_repository,
                    )
            })
            .collect::<Vec<_>>();
        let Some(certificate) = matching
            .iter()
            .copied()
            .max_by_key(|certificate| certificate.repository_root.components().count())
        else {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::ProviderNativeResult,
                RepositoryAbstentionReason::OutcomeRepositoryUnbound,
                "pull_request_association_has_no_certified_operation_route",
            );
            published.push((None, association));
            continue;
        };
        if matching
            .iter()
            .filter(|candidate| {
                candidate.repository_root.components().count()
                    == certificate.repository_root.components().count()
            })
            .count()
            != 1
        {
            push_abstention(
                annotation,
                RepositoryEvidenceKind::ProviderNativeResult,
                RepositoryAbstentionReason::ConflictingIdentity,
                "pull_request_association_route_is_ambiguous",
            );
            published.push((None, association));
            continue;
        }
        match certifier.resolve_pull_request_merge_membership(
            certificate,
            &association.merged_as,
            budget,
        ) {
            Ok(resolved) if !resolved.contains_commits.is_empty() => {
                association.contains_commits = resolved.contains_commits;
                published.push((Some(certificate.binding.binding_id.clone()), association));
            }
            Ok(_) => {
                push_abstention(
                    annotation,
                    RepositoryEvidenceKind::ProviderNativeResult,
                    RepositoryAbstentionReason::OutcomeResultInadmissible,
                    "pull_request_association_contains_no_certified_commits",
                );
                published.push((None, association));
            }
            Err(ProbeFailure::Missing | ProbeFailure::ConcurrentDrift) => {
                push_abstention(
                    annotation,
                    RepositoryEvidenceKind::ProviderNativeResult,
                    RepositoryAbstentionReason::ConcurrentDrift,
                    "pull_request_association_repository_or_object_state_drifted",
                );
                published.push((None, association));
            }
            Err(ProbeFailure::BudgetExceeded) => {
                push_probe_failure(
                    annotation,
                    RepositoryEvidenceKind::ProviderNativeResult,
                    ProbeFailure::BudgetExceeded,
                    false,
                );
                published.push((None, association));
            }
            Err(ProbeFailure::PlatformUnsupported) => {
                push_probe_failure(
                    annotation,
                    RepositoryEvidenceKind::ProviderNativeResult,
                    ProbeFailure::PlatformUnsupported,
                    false,
                );
                published.push((None, association));
            }
            Err(_) => {
                push_abstention(
                    annotation,
                    RepositoryEvidenceKind::ProviderNativeResult,
                    RepositoryAbstentionReason::OutcomeResultInadmissible,
                    "pull_request_association_dag_proof_is_inadmissible",
                );
                published.push((None, association));
            }
        }
    }
    published
}
