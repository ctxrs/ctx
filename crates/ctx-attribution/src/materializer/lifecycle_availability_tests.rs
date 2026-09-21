use super::*;

fn coverage(live_roots: u64, files: u64, commits: u64) -> SegmentCoreCoverage {
    SegmentCoreCoverage {
        repository_candidate_events: 1,
        logical_binding_events: 1,
        certified_live_root_access_events: live_roots,
        file_evidence_events: files,
        exact_commit_evidence_events: commits,
        exact_pull_request_evidence_events: 1,
    }
}

#[test]
fn current_complete_durable_evidence_enables_repository_and_file_blame() {
    let (local_repository_access, availability) = projection_availability(
        CoreProjectionCurrentness::Current,
        MaterializedCoverage::Complete,
        &coverage(1, 1, 1),
    );

    assert!(local_repository_access);
    assert!(availability.file_blame);
    assert!(availability.commit_blame);
    assert!(availability.pull_request_blame);
}

#[test]
fn incomplete_or_missing_durable_authority_fails_closed() {
    let cases = [
        (
            "missing certified root",
            CoreProjectionCurrentness::Current,
            MaterializedCoverage::Complete,
            coverage(0, 1, 1),
            (false, false, true),
        ),
        (
            "missing file evidence",
            CoreProjectionCurrentness::Current,
            MaterializedCoverage::Complete,
            coverage(1, 0, 1),
            (true, false, true),
        ),
        (
            "missing exact commit evidence",
            CoreProjectionCurrentness::Current,
            MaterializedCoverage::Complete,
            coverage(1, 1, 0),
            (true, false, false),
        ),
        (
            "partial materialization",
            CoreProjectionCurrentness::Partial,
            MaterializedCoverage::Partial,
            coverage(1, 1, 1),
            (false, false, false),
        ),
        (
            "stale materialization",
            CoreProjectionCurrentness::Stale,
            MaterializedCoverage::Partial,
            coverage(1, 1, 1),
            (false, false, false),
        ),
    ];

    for (case, currentness, materialized, coverage, expected) in cases {
        let (local_repository_access, availability) =
            projection_availability(currentness, materialized, &coverage);
        assert_eq!(local_repository_access, expected.0, "{case}");
        assert_eq!(availability.file_blame, expected.1, "{case}");
        assert_eq!(availability.commit_blame, expected.2, "{case}");
    }
}
