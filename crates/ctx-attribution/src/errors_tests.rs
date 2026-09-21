use super::*;
use crate::protocol::BlameDiagnosticCandidate;
use crate::query::{QueryOperation, ResourceKind};

fn repositories() -> AmbiguityCandidates {
    AmbiguityCandidates::repositories([
        "forge:github.com/ctxrs/ctx".to_owned(),
        "forge:gitlab.com/ctxrs/ctx".to_owned(),
    ])
}

fn commits() -> AmbiguityCandidates {
    AmbiguityCandidates::commits(
        "forge:github.com/ctxrs/ctx",
        ["a".repeat(40), "b".repeat(40)],
    )
}

#[test]
fn query_failures_preserve_actionable_protocol_classes() {
    for (error, expected_class, expected_reason) in [
        (
            QueryError::InvalidRequest("malformed selector".to_owned()),
            ErrorClass::InvalidRequest,
            None,
        ),
        (
            QueryError::TargetNotFound(ResourceKind::File),
            ErrorClass::ResourceNotFound,
            Some(BlameDiagnosticReason::TargetNotIndexed),
        ),
        (
            QueryError::RepositorySelectorNotFound,
            ErrorClass::ResourceNotFound,
            Some(BlameDiagnosticReason::RepositorySelectorNotIndexed),
        ),
        (
            QueryError::RepositoryNotBound,
            ErrorClass::MissingRepository,
            Some(BlameDiagnosticReason::RepositoryNotBound),
        ),
        (
            QueryError::RepositoryUnavailable,
            ErrorClass::MissingRepository,
            Some(BlameDiagnosticReason::CheckoutUnavailable),
        ),
        (
            QueryError::GitUnavailable,
            ErrorClass::MissingSource,
            Some(BlameDiagnosticReason::GitUnavailable),
        ),
        (
            QueryError::AmbiguousRepositoryCandidates(AmbiguityCandidates::undisclosed()),
            ErrorClass::Ambiguous,
            Some(BlameDiagnosticReason::RepositoryAmbiguous),
        ),
        (
            QueryError::AmbiguousRepositoryCandidates(repositories()),
            ErrorClass::Ambiguous,
            Some(BlameDiagnosticReason::RepositoryAmbiguous),
        ),
        (
            QueryError::AmbiguousTarget(commits()),
            ErrorClass::Ambiguous,
            Some(BlameDiagnosticReason::TargetAmbiguous),
        ),
        (
            QueryError::AmbiguousCommitRewrite(commits()),
            ErrorClass::Ambiguous,
            Some(BlameDiagnosticReason::CommitRewriteAmbiguous),
        ),
        (
            QueryError::OperationUnavailable(QueryOperation::FileBlame),
            ErrorClass::OperationUnavailable,
            Some(BlameDiagnosticReason::FileBlameNotCovered),
        ),
        (
            QueryError::OperationUnavailable(QueryOperation::CommitBlame),
            ErrorClass::OperationUnavailable,
            Some(BlameDiagnosticReason::CommitBlameNotCovered),
        ),
        (
            QueryError::OperationUnavailable(QueryOperation::PullRequestBlame),
            ErrorClass::OperationUnavailable,
            Some(BlameDiagnosticReason::PullRequestBlameNotCovered),
        ),
        (QueryError::LineOutOfRange, ErrorClass::LineOutOfRange, None),
        (QueryError::StaleSnapshot, ErrorClass::StaleSnapshot, None),
    ] {
        let protocol = protocol_error(&AdapterError::Query(error));
        assert_eq!(protocol.class, expected_class);
        assert_eq!(
            protocol.details.as_ref().map(|details| details.reason),
            expected_reason
        );
        protocol.validate().expect("valid typed query diagnostic");
    }
}

#[test]
fn unavailable_causes_remain_typed_and_distinct() {
    for git in [
        protocol_error(&AdapterError::GitAuthorityUnavailable),
        protocol_error(&AdapterError::Query(QueryError::GitUnavailable)),
    ] {
        assert_eq!(git.class, ErrorClass::MissingSource);
        assert_eq!(
            git.details.as_ref().map(|details| details.reason),
            Some(BlameDiagnosticReason::GitUnavailable)
        );
        git.validate().expect("valid Git-unavailable diagnostic");
    }

    let checkout = protocol_error(&AdapterError::Query(QueryError::RepositoryUnavailable));
    assert_eq!(checkout.class, ErrorClass::MissingRepository);
    assert_eq!(
        checkout.details.as_ref().map(|details| details.reason),
        Some(BlameDiagnosticReason::CheckoutUnavailable)
    );
    checkout
        .validate()
        .expect("valid checkout-unavailable diagnostic");

    let ambiguous = protocol_error(&AdapterError::Query(
        QueryError::AmbiguousRepositoryCandidates(AmbiguityCandidates::undisclosed()),
    ));
    assert_eq!(ambiguous.class, ErrorClass::Ambiguous);
    assert_eq!(
        ambiguous.details.as_ref().map(|details| details.reason),
        Some(BlameDiagnosticReason::RepositoryAmbiguous)
    );
    assert!(
        ambiguous
            .details
            .as_ref()
            .is_some_and(|details| details.candidates.is_empty())
    );
}

#[test]
fn repository_mutation_is_retryable_but_ambiguity_is_not() {
    let stale = protocol_error(&AdapterError::Query(QueryError::StaleSnapshot));
    assert_eq!(stale.class, ErrorClass::StaleSnapshot);
    assert!(stale.retryable);

    let ambiguous = protocol_error(&AdapterError::Query(
        QueryError::AmbiguousRepositoryCandidates(AmbiguityCandidates::undisclosed()),
    ));
    assert_eq!(ambiguous.class, ErrorClass::Ambiguous);
    assert!(!ambiguous.retryable);
}

#[test]
fn ambiguity_projection_keeps_only_safe_bounded_protocol_candidates() {
    let details = AmbiguityCandidates::repositories(
        [
            "forge:github.com/example/z",
            "/home/private/repository",
            "forge:github.com/example/e",
            "forge:github.com/example/d",
            "forge:github.com/example/c",
            "forge:github.com/example/b",
            "forge:github.com/example/a",
            "forge:github.com/example/a",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    let protocol = protocol_error(&AdapterError::Query(
        QueryError::AmbiguousRepositoryCandidates(details),
    ));
    let details = protocol.details.expect("typed ambiguity details");
    assert_eq!(details.candidates.len(), 5);
    assert!(details.candidates_truncated);
    assert!(details.candidates.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(details.candidates.iter().all(|candidate| matches!(
        candidate,
        BlameDiagnosticCandidate::Repository { selector }
            if selector.starts_with("forge:") && !selector.contains("/home")
    )));
}

#[test]
fn opaque_repository_ambiguity_remains_typed_without_disclosing_candidates() {
    let details = AmbiguityCandidates::repositories([
        "local:private-repository-a".to_owned(),
        "workspace:private-repository-b".to_owned(),
    ]);
    assert!(details.candidates.is_empty());
    let protocol = protocol_error(&AdapterError::Query(
        QueryError::AmbiguousRepositoryCandidates(details),
    ));
    let details = protocol.details.as_ref().expect("typed ambiguity details");
    assert_eq!(details.reason, BlameDiagnosticReason::RepositoryAmbiguous);
    assert!(details.candidates.is_empty());
    protocol.validate().expect("valid candidate-free ambiguity");
}

#[test]
fn raw_helper_and_path_details_never_cross_protocol_errors() {
    let secret = "/home/private/helper token=secret";
    for error in [
        AdapterError::InvalidInput(secret.to_owned()),
        AdapterError::Query(QueryError::Backend(secret.to_owned())),
    ] {
        let encoded = serde_json::to_string(&protocol_error(&error))
            .expect("serialize sanitized protocol error");
        assert!(!encoded.contains("/home/private"));
        assert!(!encoded.contains("token=secret"));
    }
}

#[test]
fn segment_failures_preserve_stable_protocol_classes_without_diagnostics() {
    for (error, expected) in [
        (AdapterError::MaterializerBounds, ErrorClass::Bounds),
        (AdapterError::MaterializerBusy, ErrorClass::NotMaterialized),
        (AdapterError::SegmentUnavailable, ErrorClass::Corrupt),
        (AdapterError::WritableBackendRequired, ErrorClass::Sequence),
    ] {
        let protocol = protocol_error(&error);
        assert_eq!(protocol.class, expected);
        assert!(!protocol.message.contains("segment"));
    }
    assert!(protocol_error(&AdapterError::MaterializerBusy).retryable);
}
