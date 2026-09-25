use super::*;

#[cfg(test)]
pub(in crate::materializer) fn validate_projection_output_lengths_for_test(
    lengths: impl IntoIterator<Item = usize>,
) -> Result<(), SegmentMaterializerError> {
    validate_projection_output_lengths(lengths)
}

#[cfg(test)]
pub(in crate::materializer) fn prepare_page_projections_for_test(
    prepared_pages: &[PreparedCoreEventDeltaPage],
    workers: usize,
) -> Result<Vec<PreparedCorePageProjection>, SegmentMaterializerError> {
    let preparer = CoreProjectionPreparer::with_parallelism(workers).map_err(|_| {
        SegmentMaterializerError::Corrupt("provider projection workers are unavailable")
    })?;
    ordered_parallel_map_owned(
        &preparer,
        prepared_pages.iter().collect(),
        MAX_DIRECT_BATCH_PAGES,
        |prepared| Ok(prepare_page_projection(prepared)),
    )
    .map_err(|_| SegmentMaterializerError::Corrupt("provider projection workers are unavailable"))?
    .into_iter()
    .collect()
}

#[cfg(test)]
#[test]
fn omitted_fact_cannot_advertise_unstored_commit_evidence() {
    use crate::graph::segment_state::SegmentCoreCoverage;

    let original = SegmentCoreCoverage {
        repository_candidate_events: 1,
        logical_binding_events: 1,
        certified_live_root_access_events: 1,
        file_evidence_events: 1,
        exact_commit_evidence_events: 1,
        exact_pull_request_evidence_events: 1,
        bounded_omission_events: 0,
    };
    let mut empty = original.clone();
    empty.bounded_omission_events = 1;
    retain_projected_coverage(&mut empty, &EventRecordSummary::default());
    assert_eq!(empty.bounded_omission_events, 1);
    assert_eq!(empty.logical_binding_events, 0);

    let mut file_only = original;
    retain_projected_coverage(
        &mut file_only,
        &EventRecordSummary {
            records: vec![("file-fact".into(), [0; 32])],
            file: true,
            ..Default::default()
        },
    );
    assert_eq!(file_only.file_evidence_events, 1);
    assert_eq!(file_only.exact_commit_evidence_events, 0);
    assert_eq!(file_only.exact_pull_request_evidence_events, 0);
    assert_eq!(file_only.certified_live_root_access_events, 0);
}
