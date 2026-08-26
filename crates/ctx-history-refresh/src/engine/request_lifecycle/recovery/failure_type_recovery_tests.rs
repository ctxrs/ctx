use super::*;

#[test]
fn recovery_accepts_source_backed_legacy_failure_types() {
    for (failure_type, expected) in [
        (
            "unsupported_schema",
            SourceBackedRefreshFailureType::UnsupportedSchema,
        ),
        (
            "malformed_source",
            SourceBackedRefreshFailureType::MalformedSource,
        ),
        (
            "source_unavailable",
            SourceBackedRefreshFailureType::SourceUnavailable,
        ),
        (
            "source_changed",
            SourceBackedRefreshFailureType::SourceChanged,
        ),
        (
            "source_failures",
            SourceBackedRefreshFailureType::SourceFailures,
        ),
        (
            "all_provider_terminal_coverage_unavailable",
            SourceBackedRefreshFailureType::AllProviderTerminalCoverageUnavailable,
        ),
    ] {
        let job = json!({ "failure_type": failure_type });
        assert_eq!(
            recover_optional_failure_type(&job).unwrap(),
            Some(expected),
            "{failure_type}",
        );
        assert_eq!(expected.as_str(), failure_type);
    }
}

#[test]
fn recovery_rejects_failure_types_outside_source_backed_legacy_vocabulary() {
    for failure_type in [
        RefreshOutcomeCode::Completed,
        RefreshOutcomeCode::CompletedWithRejections,
        RefreshOutcomeCode::IndexCorruption,
        RefreshOutcomeCode::SourceRefreshInternal,
    ] {
        let job = json!({ "failure_type": failure_type.as_str() });
        assert!(
            recover_optional_failure_type(&job).is_err(),
            "{}",
            failure_type.as_str(),
        );
    }

    let job = json!({ "failure_type": "not_a_code" });
    assert!(recover_optional_failure_type(&job).is_err());
}
