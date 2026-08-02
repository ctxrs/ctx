use ctx_history_core::{
    CoreRecordAnnotation, RepositoryFileObservation, RepositoryFileObservationKind,
};

pub(super) fn preserve_cursor_ordinary_file_observations(annotation: &mut CoreRecordAnnotation) {
    let ordinary = annotation
        .repository_file_invocation_evidence
        .iter()
        .map(|evidence| RepositoryFileObservation {
            repository_binding_id: evidence.repository_binding_id.clone(),
            relative_path: evidence.relative_path.clone(),
            kind: RepositoryFileObservationKind::Unknown,
            prior_relative_path: None,
        })
        .collect::<Vec<_>>();
    for observation in ordinary {
        if !annotation
            .repository_file_observations
            .contains(&observation)
        {
            annotation.repository_file_observations.push(observation);
        }
    }
}
