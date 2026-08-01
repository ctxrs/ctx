use std::path::Path;

use ctx_history_core::{
    CoreRecordAnnotation, RepositoryAbstentionReason, RepositoryEvidence,
    RepositoryEvidenceConfidence, RepositoryEvidenceKind, RepositoryFileObservation,
    RepositoryFileObservationKind, RepositoryOutcomeObservation, RepositoryVcsObservation,
    RepositoryVcsObservationKind,
};

use super::{push_abstention, CertifiedCandidate, ScopedFileInput, ScopedVcsInput};

pub(super) fn scope_files(
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

pub(super) fn scope_vcs(
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

pub(super) fn scope_outcomes(
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

pub(super) fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
