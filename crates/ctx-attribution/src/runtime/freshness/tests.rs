use super::*;

fn outcome(attribution: BlameAttribution, proven: u32, none: u32) -> BlameOutcome {
    BlameOutcome {
        attribution,
        coverage: BlameCoverage {
            unit: BlameCoverageUnit::CommitFact,
            evaluated: proven + none,
            proven,
            possible: 0,
            conflicting: 0,
            none,
        },
    }
}

#[test]
fn current_empty_and_negative_are_valid_but_stale_absence_is_not() {
    for value in [
        outcome(BlameAttribution::None, 0, 0),
        outcome(BlameAttribution::None, 0, 1),
        outcome(BlameAttribution::Possible, 1, 1),
    ] {
        assert_eq!(
            classify(&value, true).unwrap(),
            BlameResultFreshness::Current
        );
        let error = classify(&value, false).unwrap_err();
        assert_eq!(error.reason, BlameDiagnosticReason::ProjectionStale);
        assert_eq!(error.error_code, "stale_source");
        assert_eq!(
            error.freshness.unwrap().state,
            BlameFreshnessState::StaleCommitted
        );
        assert_eq!(error.next_action.unwrap().argv, ["ctx", "import", "--all"]);
    }
    assert_eq!(
        classify(&outcome(BlameAttribution::Proven, 1, 0), false).unwrap(),
        BlameResultFreshness::StaleCommitted
    );
}

#[test]
fn stale_generation_bound_failures_require_completion_but_live_failures_keep_their_reason() {
    let diagnostic = |reason| {
        let mut value =
            crate::diagnostic::projection_diagnostic(CoreProjectionCurrentness::NotMaterialized)
                .unwrap();
        value.reason = reason;
        value
    };
    for reason in [
        BlameDiagnosticReason::TargetNotIndexed,
        BlameDiagnosticReason::RepositoryNotBound,
        BlameDiagnosticReason::TargetAmbiguous,
        BlameDiagnosticReason::CommitBlameNotCovered,
    ] {
        assert_eq!(
            complete(Err(diagnostic(reason)), "a", Some("a"), Some("a"))
                .unwrap_err()
                .reason,
            reason
        );
        for (before, after) in [
            (Some("a"), Some("b")),
            (Some("b"), Some("a")),
            (None, Some("a")),
        ] {
            let stale = complete(Err(diagnostic(reason)), "a", before, after).unwrap_err();
            assert_eq!(stale.reason, BlameDiagnosticReason::ProjectionStale);
            assert_eq!(
                stale.next_action.unwrap().kind,
                BlameNextActionKind::ImportAll
            );
        }
    }
    assert_eq!(
        complete(
            Err(diagnostic(BlameDiagnosticReason::CheckoutUnavailable)),
            "a",
            Some("b"),
            Some("b")
        )
        .unwrap_err()
        .reason,
        BlameDiagnosticReason::CheckoutUnavailable
    );
}
