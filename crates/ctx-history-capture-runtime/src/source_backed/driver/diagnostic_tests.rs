use super::*;

#[test]
fn exhausted_resources_preserve_typed_limits_not_accounting_overflow() {
    let resources = crate::CoreRouteResources::with_byte_limits(1, 1, 1);
    for (kind, diagnostic) in [
        (
            crate::CoreRouteResourceKind::CoreOutput,
            SourceBackedRouteFailureDiagnostic::OutputLimit,
        ),
        (
            crate::CoreRouteResourceKind::LogicalSourceScratch,
            SourceBackedRouteFailureDiagnostic::ScratchLimit,
        ),
    ] {
        let error = resources.reserve(kind, 2).unwrap_err();
        let route = SourceBackedRouteError::from(error);
        assert_eq!(route.kind, SourceBackedRouteErrorKind::ResourceUnavailable);
        assert_eq!(route.diagnostic, Some(diagnostic));
        assert!(
            SourceBackedRouteError::from(CoreRouteResourceError::AccountingOverflow {
                kind,
                maximum: 1
            })
            .diagnostic
            .is_none()
        );
    }
}

#[test]
fn io_causes_survive_wrapping_and_ambiguous_cleanup_is_omitted() {
    let primary = SourceBackedRouteError::from_error(
        SourceBackedRouteErrorKind::ResourceUnavailable,
        &std::io::Error::new(std::io::ErrorKind::StorageFull, "/private/token"),
    );
    assert_eq!(
        primary.diagnostic,
        Some(SourceBackedRouteFailureDiagnostic::Io(
            std::io::ErrorKind::StorageFull
        ))
    );
    let unknown = SourceBackedRouteError::new(primary.kind, "StorageFull");
    assert!(unknown.diagnostic.is_none());
    assert!(
        combine_primary_and_cleanup_route_errors(primary.clone(), unknown)
            .diagnostic
            .is_none()
    );
    let stronger = combine_primary_and_cleanup_route_errors(
        SourceBackedRouteError::new(SourceBackedRouteErrorKind::Unavailable, "private"),
        primary.clone(),
    );
    assert_eq!(
        stronger.kind,
        SourceBackedRouteErrorKind::ResourceUnavailable
    );
    assert!(stronger.diagnostic.is_none());
}

#[test]
fn cleanup_diagnostics_require_agreement_independent_of_policy_severity() {
    use SourceBackedRouteErrorKind::{Internal, ResourceUnavailable, Unavailable};
    let disk = Some(SourceBackedRouteFailureDiagnostic::Io(
        std::io::ErrorKind::StorageFull,
    ));
    let permission = Some(SourceBackedRouteFailureDiagnostic::Io(
        std::io::ErrorKind::PermissionDenied,
    ));
    for (left_kind, right_kind, dominant) in [
        (
            ResourceUnavailable,
            ResourceUnavailable,
            ResourceUnavailable,
        ),
        (Unavailable, ResourceUnavailable, ResourceUnavailable),
        (ResourceUnavailable, Internal, Internal),
    ] {
        for (left_reason, right_reason, expected) in [
            (None, disk, None),
            (disk, permission, None),
            (disk, disk, disk),
            (None, None, None),
        ] {
            for reverse in [false, true] {
                let mut left = SourceBackedRouteError::new(left_kind, "primary");
                left.diagnostic = left_reason;
                let mut right = SourceBackedRouteError::new(right_kind, "cleanup");
                right.diagnostic = right_reason;
                let result = if reverse {
                    combine_primary_and_cleanup_route_errors(right, left)
                } else {
                    combine_primary_and_cleanup_route_errors(left, right)
                };
                assert_eq!(result.kind, dominant);
                assert_eq!(result.diagnostic, expected);
            }
        }
    }
}
