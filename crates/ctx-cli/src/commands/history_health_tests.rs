use super::*;
use ctx_history_capture::{
    DiscoveryIssueKind, ProviderCatalogSupport, ProviderImportSupport, ProviderRouteRole,
    ProviderSourceKind,
};
use ctx_history_core::CaptureProvider;

fn automatic_source(
    provider: CaptureProvider,
    status: ProviderSourceStatus,
    path: std::path::PathBuf,
    route_role: &'static str,
) -> ProviderSource {
    ProviderSource {
        provider,
        path,
        exists: true,
        source_format: "codex_session_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::Native,
        status,
        unsupported_reason: None,
        route_provenance: ProviderSourceRouteProvenance::Automatic {
            route_role: ProviderRouteRole::from_static(route_role),
        },
    }
}

fn configured_codex_source(
    root_id: &str,
    root_path: &str,
    subpath: &str,
    status: ProviderSourceStatus,
    exists: bool,
    route_role: &'static str,
) -> ProviderSource {
    let root_path = std::path::PathBuf::from(root_path);
    ProviderSource {
        provider: CaptureProvider::Codex,
        path: root_path.join(subpath),
        exists,
        source_format: if subpath == "history.jsonl" {
            "codex_history_jsonl"
        } else {
            "codex_session_jsonl_tree"
        },
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::Native,
        status,
        unsupported_reason: None,
        route_provenance: ProviderSourceRouteProvenance::ConfiguredRoot {
            root_id: root_id.to_owned(),
            root_path,
            route_role: ProviderRouteRole::from_static(route_role),
            automatic_route_role: None,
        },
    }
}

fn evaluate_sources(sources: &[ProviderSource]) -> HistoryRootCoverage {
    let mut roots = HashMap::<RootIdentity, RootState>::new();
    for source in sources {
        let state = roots.entry(source_root_identity(source)).or_default();
        record_source_coverage(state, source);
    }
    root_coverage(&roots).unwrap()
}

#[test]
fn unknown_automatic_source_and_matching_issue_count_as_one_root() {
    let path = std::path::PathBuf::from("/history/codex/sessions");
    let source = automatic_source(
        CaptureProvider::Codex,
        ProviderSourceStatus::Unknown,
        path.clone(),
        "codex-sessions",
    );
    let issue = DiscoveryIssue {
        provider: CaptureProvider::Codex,
        path: Some(path),
        kind: DiscoveryIssueKind::SelectorUnreconstructible,
        reason: "test issue",
    };

    let lookup = source_root_lookup(std::slice::from_ref(&source));
    let mut roots = HashMap::<RootIdentity, RootState>::new();
    roots
        .entry(source_root_identity(&source))
        .or_default()
        .unknown = true;
    roots
        .entry(issue_root_identity(&issue, &lookup, &[]))
        .or_default()
        .unknown = true;

    assert_eq!(roots.len(), 1);
    assert_eq!(
        root_coverage(&roots).unwrap(),
        HistoryRootCoverage {
            unknown: 1,
            ..HistoryRootCoverage::default()
        }
    );
}

#[test]
fn empty_automatic_root_is_omitted_from_human_coverage() {
    let source = automatic_source(
        CaptureProvider::Gemini,
        ProviderSourceStatus::Empty,
        std::path::PathBuf::from("/history/gemini/empty"),
        "gemini-projects",
    );
    let mut roots = HashMap::<RootIdentity, RootState>::new();
    let state = roots.entry(source_root_identity(&source)).or_default();

    record_source_coverage(state, &source);
    assert_eq!(
        root_coverage(&roots).unwrap(),
        HistoryRootCoverage::default()
    );

    let mut health = HistoryHealthReport::default();
    health.record_inventory(HistoryRootCoverage::default(), Some(0));
    assert!(health.contributing_agent_histories.is_empty());
    assert!(!health.is_partial());
}

#[test]
fn configured_multi_subpath_root_with_missing_optional_locations_is_included() {
    let coverage = evaluate_sources(&[
        configured_codex_source(
            "t3-codex",
            "/history/t3-codex",
            "sessions",
            ProviderSourceStatus::Available,
            true,
            "codex-sessions",
        ),
        configured_codex_source(
            "t3-codex",
            "/history/t3-codex",
            "archived_sessions",
            ProviderSourceStatus::Missing,
            false,
            "codex-archived-sessions",
        ),
        configured_codex_source(
            "t3-codex",
            "/history/t3-codex",
            "history.jsonl",
            ProviderSourceStatus::Missing,
            false,
            "codex-prompt-history",
        ),
    ]);

    assert_eq!(
        coverage,
        HistoryRootCoverage {
            included: 1,
            ..HistoryRootCoverage::default()
        }
    );
}

#[test]
fn configured_multi_subpath_root_with_present_empty_subsource_is_not_unknown() {
    let coverage = evaluate_sources(&[
        configured_codex_source(
            "empty-codex",
            "/history/empty-codex",
            "sessions",
            ProviderSourceStatus::Empty,
            true,
            "codex-sessions",
        ),
        configured_codex_source(
            "empty-codex",
            "/history/empty-codex",
            "archived_sessions",
            ProviderSourceStatus::Missing,
            false,
            "codex-archived-sessions",
        ),
        configured_codex_source(
            "empty-codex",
            "/history/empty-codex",
            "history.jsonl",
            ProviderSourceStatus::Missing,
            false,
            "codex-prompt-history",
        ),
    ]);

    assert_eq!(coverage, HistoryRootCoverage::default());
}

#[test]
fn configured_root_with_all_subsources_missing_remains_unknown() {
    let coverage = evaluate_sources(&[
        configured_codex_source(
            "missing-codex",
            "/history/missing-codex",
            "sessions",
            ProviderSourceStatus::Missing,
            false,
            "codex-sessions",
        ),
        configured_codex_source(
            "missing-codex",
            "/history/missing-codex",
            "archived_sessions",
            ProviderSourceStatus::Missing,
            false,
            "codex-archived-sessions",
        ),
        configured_codex_source(
            "missing-codex",
            "/history/missing-codex",
            "history.jsonl",
            ProviderSourceStatus::Missing,
            false,
            "codex-prompt-history",
        ),
    ]);

    assert_eq!(
        coverage,
        HistoryRootCoverage {
            unknown: 1,
            ..HistoryRootCoverage::default()
        }
    );
}

#[test]
fn configured_root_with_present_unsupported_or_unknown_subsource_remains_partial_or_excluded() {
    let partial = evaluate_sources(&[
        configured_codex_source(
            "partial-codex",
            "/history/partial-codex",
            "sessions",
            ProviderSourceStatus::Available,
            true,
            "codex-sessions",
        ),
        configured_codex_source(
            "partial-codex",
            "/history/partial-codex",
            "history.jsonl",
            ProviderSourceStatus::Unsupported,
            true,
            "codex-prompt-history",
        ),
    ]);
    assert_eq!(
        partial,
        HistoryRootCoverage {
            partial: 1,
            ..HistoryRootCoverage::default()
        }
    );

    let excluded = evaluate_sources(&[
        configured_codex_source(
            "excluded-codex",
            "/history/excluded-codex",
            "sessions",
            ProviderSourceStatus::Unsupported,
            true,
            "codex-sessions",
        ),
        configured_codex_source(
            "excluded-codex",
            "/history/excluded-codex",
            "history.jsonl",
            ProviderSourceStatus::Missing,
            false,
            "codex-prompt-history",
        ),
    ]);
    assert_eq!(
        excluded,
        HistoryRootCoverage {
            excluded: 1,
            ..HistoryRootCoverage::default()
        }
    );
}
