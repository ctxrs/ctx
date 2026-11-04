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
            upgrade_blame_diagnostic(root.path(), diagnostic.clone()),
            diagnostic
        );
    }
}
