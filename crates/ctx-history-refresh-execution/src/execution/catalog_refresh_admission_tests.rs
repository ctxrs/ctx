use super::*;
use std::fs;

use ctx_history_capture::{
    provider_source_for_path, DiscoveryPlatform, DiscoveryPlatformDirs, SourceBackedRoute,
    SourceBackedRouteDriver,
};

struct UnusedPublishedState;

impl PublishedSourceBackedStatePort for UnusedPublishedState {
    fn open_published_state(&self, _data_root: &Path) -> Result<PublishedSourceBackedState> {
        unreachable!("catalog admission does not open published state")
    }
}

fn admitted_refresh(
    route_identity: SourceRouteIdentity,
    report: DiscoveryReport,
    watch_catalog: SourceBackedWatchCatalog,
    route_worksets: BTreeMap<SourceRouteIdentity, SourceBackedRefreshWorkset>,
) -> AdmittedRefresh {
    AdmittedRefresh::for_test(
        AdmittedRefreshCoverage::SelectedRoutes,
        BTreeSet::from([route_identity]),
        SourceBackedAdmittedDiscovery::new(report, StdDuration::ZERO, watch_catalog),
    )
    .unwrap()
    .with_execution_facts(route_worksets)
    .unwrap()
}

#[test]
fn exhaustive_exact_route_reuses_catalog_without_claiming_member_work() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let data_root = temp.path().join("data");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let database = home.join("history.db");
    fs::write(&database, b"sqlite").unwrap();
    let source = provider_source_for_path(CaptureProvider::OpenCode, database);
    let route = SourceBackedRoute::automatic(
        source.clone(),
        SourceBackedSelectorAuthority::DiscoveredWinner,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )
    .unwrap();
    let route_identity = route.metadata().route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let progress = |_: SourceBackedRefreshProgressUpdate| Ok(());
    let admitted = admitted_refresh(
        route_identity,
        DiscoveryReport {
            sources: vec![source.clone()],
            issues: Vec::new(),
        },
        registry.watch_catalog(),
        BTreeMap::new(),
    );
    let execution = SourceBackedRefreshExecution::new(
        &data_root,
        &index_root,
        "route-local-exhaustive",
        RefreshOperation::Refresh,
        None,
        admitted,
        &discovery,
        &UnusedPublishedState,
        &progress,
    )
    .with_reconciliation_demand(SourceBackedReconciliationDemand::Exhaustive);

    let admission = catalog_refresh_admission(&execution);
    assert!(!admission.exact_members);
    assert_eq!(admission.report.sources, vec![source]);
}

#[test]
fn catalog_selector_distinguishes_exact_members_from_global_fallbacks() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let data_root = temp.path().join("data");
    let index_root = temp.path().join("index");
    let root = home.join("claude-projects");
    let member = root.join("project/session.jsonl");
    fs::create_dir_all(member.parent().unwrap()).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::write(&member, b"{}\n").unwrap();
    let source = provider_source_for_path(CaptureProvider::Claude, root.clone());
    let route = SourceBackedRoute::automatic(
        source.clone(),
        SourceBackedSelectorAuthority::DiscoveredWinner,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )
    .unwrap();
    let route_identity = route.metadata().route_identity.clone().unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let progress = |_: SourceBackedRefreshProgressUpdate| Ok(());
    let admitted = admitted_refresh(
        route_identity.clone(),
        DiscoveryReport {
            sources: vec![source.clone()],
            issues: Vec::new(),
        },
        registry.watch_catalog(),
        BTreeMap::from([(
            route_identity.clone(),
            SourceBackedRefreshWorkset::members([member.clone()]),
        )]),
    );
    let execution = SourceBackedRefreshExecution::new(
        &data_root,
        &index_root,
        "exact-member",
        RefreshOperation::Refresh,
        None,
        admitted,
        &discovery,
        &UnusedPublishedState,
        &progress,
    );

    let exact = catalog_refresh_admission(&execution);
    assert!(exact.exact_members);
    assert_eq!(exact.report.sources, vec![source.clone()]);

    let invalid_member = admitted_refresh(
        route_identity.clone(),
        DiscoveryReport {
            sources: vec![source.clone()],
            issues: Vec::new(),
        },
        registry.watch_catalog(),
        BTreeMap::from([(
            route_identity,
            SourceBackedRefreshWorkset::members([root.join("missing.jsonl")]),
        )]),
    );
    let invalid_member = SourceBackedRefreshExecution::new(
        &data_root,
        &index_root,
        "invalid-exact-member",
        RefreshOperation::Refresh,
        None,
        invalid_member,
        &discovery,
        &UnusedPublishedState,
        &progress,
    );
    let invalid_member = catalog_refresh_admission(&invalid_member);
    assert!(!invalid_member.exact_members);
    assert_eq!(invalid_member.report.sources, vec![source.clone()]);

    fs::remove_dir_all(root).unwrap();
    let removed_member = catalog_refresh_admission(&execution);
    assert!(!removed_member.exact_members);
    assert_eq!(removed_member.report.sources, vec![source]);
}

#[test]
fn member_fallback_requires_failure_and_runs_only_once() {
    let route = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();
    let members = BTreeMap::from([(
        route.clone(),
        BTreeSet::from([PathBuf::from("changed.json")]),
    )]);
    let mut outcome = SourceBackedSuccessfulRouteOutcome {
        route_identity: route,
        changed: true,
        logical_source_failure_total: 0,
        logical_source_retryable_failure_total: 0,
    };

    assert!(!exact_member_family_fallback_required(
        &members,
        std::slice::from_ref(&outcome),
        &[],
    ));
    outcome.logical_source_failure_total = 1;
    assert!(exact_member_family_fallback_required(
        &members,
        std::slice::from_ref(&outcome),
        &[]
    ));
    assert!(!exact_member_family_fallback_required(
        &BTreeMap::new(),
        &[outcome],
        &[],
    ));
}
