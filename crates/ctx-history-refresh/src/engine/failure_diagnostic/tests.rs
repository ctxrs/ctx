use super::*;

#[test]
fn provider_policy_severity_does_not_supply_a_missing_reason() {
    // The route owner's cleanup-combination test exercises the actual merge.
    // At this boundary its dominant class must not stand in for a known cause.
    let error: anyhow::Error = SourceBackedCoordinatorError::RouteScan {
        provider: CaptureProvider::Codex,
        source: SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::ResourceUnavailable,
            "primary unavailable; cleanup StorageFull /private/token",
        ),
    }
    .into();
    let diagnostic = RefreshFailureDiagnostic::new(RefreshFailureStage::Execution, Some(&error));
    assert_eq!(diagnostic.kind, RefreshFailureKind::Provider);
    assert_eq!(diagnostic.reason, None);
}

#[test]
fn failure_diagnostic_uses_typed_causes_not_error_text() {
    let cases: Vec<(anyhow::Error, RefreshFailureKind)> = vec![
        (
            anyhow!("route_scan codex permission denied /private/history token=secret"),
            RefreshFailureKind::Unknown,
        ),
        (
            anyhow::Error::new(SourceBackedCoordinatorError::RouteScan {
                provider: CaptureProvider::Codex,
                source: SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "/private/history token=secret",
                ),
            })
            .context("private wrapper text"),
            RefreshFailureKind::Provider,
        ),
        (
            SourceBackedCoordinatorError::Index(IndexError::WriterInvariant(
                "private invariant detail",
            ))
            .into(),
            RefreshFailureKind::Index,
        ),
        (
            anyhow::Error::new(IndexError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "/private/history token=secret",
            )))
            .context("private wrapper"),
            RefreshFailureKind::Io,
        ),
        (
            SourceBackedCoordinatorError::Index(IndexError::Io(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "private detail",
            )))
            .into(),
            RefreshFailureKind::Io,
        ),
    ];
    for (error, expected) in cases {
        let diagnostic =
            RefreshFailureDiagnostic::new(RefreshFailureStage::Execution, Some(&error));
        assert_eq!(diagnostic.kind, expected);
        assert_eq!(diagnostic.stage, RefreshFailureStage::Execution);
    }
}

#[test]
fn failure_diagnostic_recovery_requires_a_complete_closed_pair() {
    let value = json!({"refresh_failure_stage": "admission", "refresh_failure_kind": "provider"});
    assert_eq!(
        RefreshFailureDiagnostic::from_job(&value),
        Some(RefreshFailureDiagnostic {
            stage: RefreshFailureStage::Admission,
            kind: RefreshFailureKind::Provider,
            coverage_reason: None,
            reason: None,
        })
    );
    for value in [
        json!({}),
        json!({"refresh_failure_stage": "execution"}),
        json!({"refresh_failure_kind": "io"}),
        json!({"refresh_failure_stage": "/private/path", "refresh_failure_kind": "unknown"}),
        json!({"refresh_failure_stage": "execution", "refresh_failure_kind": "private error"}),
    ] {
        assert!(RefreshFailureDiagnostic::from_job(&value).is_none());
    }
}

#[test]
fn io_evidence_survives_route_and_admission_wrappers_but_mixed_causes_do_not() {
    use ctx_history_refresh_execution::SourceBackedAdmissionRouteFailure as Failure;
    for kind in [
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::StorageFull,
        std::io::ErrorKind::ReadOnlyFilesystem,
        std::io::ErrorKind::OutOfMemory,
        std::io::ErrorKind::TimedOut,
    ] {
        let route = SourceBackedRouteError::from_error(
            SourceBackedRouteErrorKind::ResourceUnavailable,
            &std::io::Error::new(kind, "/private/token"),
        );
        let error: anyhow::Error = SourceBackedCoordinatorError::RouteRegistration {
            provider: CaptureProvider::Codex,
            source: route.clone(),
        }
        .into();
        assert_eq!(
            RefreshFailureDiagnostic::new(RefreshFailureStage::Admission, Some(&error)).reason,
            RefreshFailureReason::from_io(kind)
        );
        let failure = Failure::new(
            SourceRouteIdentity::from_sha256("aa".repeat(32)).unwrap(),
            route.kind,
            route.detail,
        )
        .with_diagnostic(route.diagnostic);
        let error: anyhow::Error =
            SourceBackedAdmissionRouteFailures::try_from_failures([failure.clone()])
                .unwrap()
                .into();
        assert_eq!(
            RefreshFailureDiagnostic::new(RefreshFailureStage::Admission, Some(&error)).reason,
            RefreshFailureReason::from_io(kind)
        );
        let unknown = Failure::new(
            SourceRouteIdentity::from_sha256("bb".repeat(32)).unwrap(),
            failure.kind(),
            "io_storage_full",
        );
        let error: anyhow::Error =
            SourceBackedAdmissionRouteFailures::try_from_failures([failure, unknown])
                .unwrap()
                .into();
        assert!(
            RefreshFailureDiagnostic::new(RefreshFailureStage::Admission, Some(&error))
                .reason
                .is_none()
        );
    }
    for text in [
        "io_storage_full",
        "index_writer_invariant",
        "route_output_limit",
        "/private/token",
    ] {
        let error = anyhow!(text);
        assert!(
            RefreshFailureDiagnostic::new(RefreshFailureStage::Execution, Some(&error))
                .reason
                .is_none()
        );
    }
}

#[test]
fn coverage_reason_is_typed_not_inferred_from_private_error_text() {
    for text in [
        "catalog_unavailable",
        "unsafe_root",
        "missing_terminal_authority",
        "route_failed",
        "invalid_route_identity",
        "missing_empty_authority",
    ] {
        let reason = ZeroSourcePublicationBlockReason::parse(text).unwrap();
        let typed = anyhow::Error::new(ZeroSourcePublicationBlocked::with_reason(
            reason,
            "/private/source token=secret",
        ))
        .context("private wrapper");
        let diagnostic =
            RefreshFailureDiagnostic::new(RefreshFailureStage::Execution, Some(&typed));
        assert_eq!(diagnostic.kind, RefreshFailureKind::Provider);
        assert_eq!(diagnostic.coverage_reason, Some(reason));
        for error in [
            anyhow!("{text}"),
            ZeroSourcePublicationBlocked::new(text).into(),
        ] {
            assert!(
                RefreshFailureDiagnostic::new(RefreshFailureStage::Execution, Some(&error))
                    .coverage_reason
                    .is_none()
            );
        }
    }
}
