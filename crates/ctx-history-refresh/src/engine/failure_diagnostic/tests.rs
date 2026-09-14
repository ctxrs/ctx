use super::*;

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
