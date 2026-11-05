use super::*;

#[test]
fn stopped_maintenance_and_other_failures_keep_existing_advice() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("config.toml"),
        "[indexing]\nmode=\"automatic\"\n",
    )
    .unwrap();
    for (code, reason) in [
        (
            "needs_rebuild",
            BlameDiagnosticReason::ProjectionIncompatible,
        ),
        ("stale_source", BlameDiagnosticReason::ProjectionStale),
        ("corrupt_graph", BlameDiagnosticReason::GraphCorrupt),
        (
            "protocol_mismatch",
            BlameDiagnosticReason::ProjectionIncompatible,
        ),
        (
            "source_unavailable",
            BlameDiagnosticReason::SourceUnavailable,
        ),
    ] {
        let diagnostic = BlameDiagnostic {
            error: code,
            error_code: code,
            reason,
            message: "Original diagnostic.",
            retryable: false,
            progress: None,
            freshness: None,
            next_action: Some(ctx_attribution_model::BlameNextAction {
                kind: ctx_attribution_model::BlameNextActionKind::ImportAll,
                argv: ["ctx", "import", "--all"].map(str::to_owned).to_vec(),
            }),
            candidates: vec![],
            candidates_truncated: false,
        };
        assert_eq!(
            maintenance_blame_diagnostic(root.path(), diagnostic.clone()),
            diagnostic
        );
        // A live daemon must not turn unrelated failures into a wait promise.
        if !matches!(code, "needs_rebuild" | "stale_source") {
            assert_eq!(
                with_maintenance(
                    root.path(),
                    diagnostic.clone(),
                    Some(&ctx_daemon_cli::AttributionMaintenanceStatus::Running)
                ),
                diagnostic
            );
        }
    }
}

#[test]
fn stale_catchup_requires_enabled_running_maintenance() {
    use ctx_daemon_cli::AttributionMaintenanceStatus as Maintenance;
    let root = tempfile::tempdir().unwrap();
    let diagnostic = BlameDiagnostic {
        error: "stale_source",
        error_code: "stale_source",
        reason: BlameDiagnosticReason::ProjectionStale,
        message: "Blame data is behind the current history.",
        retryable: false,
        progress: None,
        freshness: None,
        next_action: Some(ctx_attribution_model::BlameNextAction {
            kind: ctx_attribution_model::BlameNextActionKind::ImportAll,
            argv: ["ctx", "import", "--all"].map(str::to_owned).to_vec(),
        }),
        candidates: vec![],
        candidates_truncated: false,
    };
    for (mode, enabled, maintenance) in [
        ("manual", true, Some(Maintenance::Running)),
        ("automatic", false, Some(Maintenance::Running)),
        ("automatic", true, Some(Maintenance::Idle)),
        ("automatic", true, Some(Maintenance::Completed)),
        (
            "automatic",
            true,
            Some(Maintenance::Failed {
                error: "writer failed".into(),
            }),
        ),
        ("automatic", true, None),
    ] {
        std::fs::write(
            root.path().join("config.toml"),
            format!("[indexing]\nmode=\"{mode}\"\n[blame]\nenabled={enabled}\n"),
        )
        .unwrap();
        assert_eq!(
            with_maintenance(root.path(), diagnostic.clone(), maintenance.as_ref()),
            diagnostic,
            "mode={mode}, enabled={enabled}, maintenance={maintenance:?}"
        );
    }
}
