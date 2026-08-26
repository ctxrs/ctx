use std::{fs, path::Path};

use ctx_history_capture_composition::{
    build_automatic_source_backed_registry_from_report_with_probes,
    refresh_source_backed_generation, source_backed_refresh_writer_options, DiscoveryContext,
    DiscoveryPlatform, DiscoveryPlatformDirs, SourceBackedAutomaticRegistryBuild,
    SourceBackedAutomaticRegistryIssue, SourceBackedProviderRegistry, StaticProviderProbeCatalog,
};
use ctx_history_capture_model::{DiscoveryIssue, DiscoveryIssueKind, ProviderRootDefinition};
use ctx_history_core::CaptureProvider;
use ctx_history_index::VerifiedIndex;
use ctx_history_source_discovery::{CursorProbeFragment, CursorTranscriptProbeOutcome};

#[path = "support/lexical.rs"]
mod lexical_test_support;

const ALPHA_MARKER: &str = "openclaw alpha retained lifecycle marker";
const BETA_MARKER: &str = "openclaw beta retained lifecycle marker";

fn provider_probes() -> StaticProviderProbeCatalog {
    fn cursor(_: &Path) -> CursorTranscriptProbeOutcome {
        CursorTranscriptProbeOutcome::NotFound
    }

    StaticProviderProbeCatalog::new(CursorProbeFragment::new(cursor))
}

fn build_registry(
    context: &DiscoveryContext,
    data_root: &Path,
) -> SourceBackedAutomaticRegistryBuild {
    let probes = provider_probes();
    let report = ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &probes,
        context,
        CaptureProvider::OpenClaw,
    );
    build_automatic_source_backed_registry_from_report_with_probes(
        &probes, context, data_root, report,
    )
}

fn route_ids(registry: &SourceBackedProviderRegistry) -> Vec<String> {
    let mut routes = registry
        .routes()
        .filter_map(|route| {
            route
                .route_identity
                .as_ref()
                .map(|identity| identity.as_str().to_owned())
        })
        .collect::<Vec<_>>();
    routes.sort();
    routes
}

fn assert_marker(index: &VerifiedIndex, marker: &str) {
    assert!(
        !lexical_test_support::search_event_candidates(index, marker, 8).is_empty(),
        "missing indexed marker {marker:?}"
    );
}

#[test]
fn truncated_openclaw_compound_root_retains_exact_agent_membership() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let state = temp.path().join("openclaw-state");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(
        state.join("openclaw.json"),
        b"{agents:{list:[{id:'Beta'},{id:'Alpha'}]}}",
    )
    .unwrap();
    for (agent_id, marker) in [("alpha", ALPHA_MARKER), ("beta", BETA_MARKER)] {
        let sessions = state.join("agents").join(agent_id).join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let mut bytes = serde_json::to_vec(&serde_json::json!({
            "type": "message",
            "id": format!("openclaw-{agent_id}"),
            "timestamp": "2026-08-24T12:00:00Z",
            "message": {"role": "user", "content": marker},
        }))
        .unwrap();
        bytes.push(b'\n');
        fs::write(sessions.join("lifecycle.jsonl"), bytes).unwrap();
    }

    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_env("OPENCLAW_STATE_DIR", state.as_os_str())
    .with_automatic_provider_discovery(false)
    .with_configured_provider_roots(vec![ProviderRootDefinition {
        id: "openclaw-state".to_owned(),
        provider: CaptureProvider::OpenClaw,
        path: state.clone(),
        group: Some("lifecycle".to_owned()),
        kind: None,
    }]);

    let initial = build_registry(&context, &temp.path().join("initial-data"));
    assert!(initial.issues.is_empty(), "{:?}", initial.issues);
    assert_eq!(initial.executable_route_count(), 2);
    let index_root = temp.path().join("index");
    let initial_receipt = refresh_source_backed_generation(
        &index_root,
        &initial.registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    assert!(initial_receipt.failed_routes.is_empty());
    let initial_index = VerifiedIndex::open(&index_root).unwrap();
    assert_marker(&initial_index, ALPHA_MARKER);
    assert_marker(&initial_index, BETA_MARKER);
    let retained_roots = initial_index.manifest().provider_roots().to_vec();
    assert_eq!(retained_roots.len(), 1);
    assert_eq!(retained_roots[0].routes().len(), 2);
    drop(initial_index);

    let agents = ["alpha".to_owned(), "beta".to_owned()]
        .into_iter()
        .chain((0..127).map(|index| format!("agent-{index:03}")))
        .map(|id| serde_json::json!({"id": id}))
        .collect::<Vec<_>>();
    fs::write(
        state.join("openclaw.json"),
        serde_json::to_vec(&serde_json::json!({"agents": {"list": agents}})).unwrap(),
    )
    .unwrap();

    let mut truncated = build_registry(&context, &temp.path().join("truncated-data"));
    assert_eq!(truncated.issues.len(), 1, "{:?}", truncated.issues);
    assert_eq!(truncated.executable_route_count(), 0);
    assert!(truncated.registry.applied_provider_roots().unwrap().2[0]
        .routes()
        .is_empty());

    let cold_index_root = temp.path().join("cold-index");
    let cold_receipt = refresh_source_backed_generation(
        &cold_index_root,
        &truncated.registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    assert!(cold_receipt.sources.is_empty());
    let cold_index = VerifiedIndex::open(&cold_index_root).unwrap();
    assert!(cold_index.manifest().sources.is_empty());
    assert!(cold_index.manifest().provider_roots()[0]
        .routes()
        .is_empty());
    drop(cold_index);

    truncated
        .registry
        .retain_unavailable_provider_root_routes(&retained_roots)
        .unwrap();
    assert_eq!(
        truncated.registry.applied_provider_roots().unwrap().2[0]
            .routes()
            .len(),
        2
    );
    let retained_receipt = refresh_source_backed_generation(
        &index_root,
        &truncated.registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    assert!(retained_receipt.failed_routes.is_empty());
    let retained_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        retained_index.manifest().provider_roots()[0].routes().len(),
        2
    );
    assert_marker(&retained_index, ALPHA_MARKER);
    assert_marker(&retained_index, BETA_MARKER);
}

#[test]
fn missing_openclaw_compound_root_retains_exact_agent_membership_until_restored() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let state = temp.path().join("openclaw-state");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(
        state.join("openclaw.json"),
        b"{agents:{list:[{id:'Beta'},{id:'Alpha'}]}}",
    )
    .unwrap();
    for (agent_id, marker) in [("alpha", ALPHA_MARKER), ("beta", BETA_MARKER)] {
        let sessions = state.join("agents").join(agent_id).join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let mut bytes = serde_json::to_vec(&serde_json::json!({
            "type": "message",
            "id": format!("openclaw-{agent_id}"),
            "timestamp": "2026-08-24T12:00:00Z",
            "message": {"role": "user", "content": marker},
        }))
        .unwrap();
        bytes.push(b'\n');
        fs::write(sessions.join("lifecycle.jsonl"), bytes).unwrap();
    }

    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_env("OPENCLAW_STATE_DIR", state.as_os_str())
    .with_automatic_provider_discovery(false)
    .with_configured_provider_roots(vec![ProviderRootDefinition {
        id: "openclaw-state".to_owned(),
        provider: CaptureProvider::OpenClaw,
        path: state.clone(),
        group: Some("lifecycle".to_owned()),
        kind: None,
    }]);

    let initial = build_registry(&context, &temp.path().join("initial-data"));
    assert!(initial.issues.is_empty(), "{:?}", initial.issues);
    assert_eq!(initial.executable_route_count(), 2);
    let initial_routes = route_ids(&initial.registry);
    assert_eq!(initial_routes.len(), 2);
    let index_root = temp.path().join("index");
    let initial_receipt = refresh_source_backed_generation(
        &index_root,
        &initial.registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    assert!(initial_receipt.failed_routes.is_empty());
    let initial_index = VerifiedIndex::open(&index_root).unwrap();
    assert_marker(&initial_index, ALPHA_MARKER);
    assert_marker(&initial_index, BETA_MARKER);
    let retained_roots = initial_index.manifest().provider_roots().to_vec();
    assert_eq!(retained_roots.len(), 1);
    assert_eq!(retained_roots[0].routes().len(), 2);
    drop(initial_index);

    let displaced = temp.path().join("openclaw-state-displaced");
    fs::rename(&state, &displaced).unwrap();
    let mut missing = build_registry(&context, &temp.path().join("missing-data"));
    assert_eq!(
        missing.issues,
        vec![SourceBackedAutomaticRegistryIssue::Discovery(
            DiscoveryIssue {
                provider: CaptureProvider::OpenClaw,
                path: Some(state.clone()),
                kind: DiscoveryIssueKind::ConfiguredRootMissing,
                reason: "the configured provider history root is missing",
            }
        )]
    );
    assert_eq!(missing.executable_route_count(), 0);
    assert!(missing.registry.applied_provider_roots().unwrap().2[0]
        .routes()
        .is_empty());

    let cold_index_root = temp.path().join("cold-index");
    let cold_receipt = refresh_source_backed_generation(
        &cold_index_root,
        &missing.registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    assert!(cold_receipt.sources.is_empty());
    let cold_index = VerifiedIndex::open(&cold_index_root).unwrap();
    assert!(cold_index.manifest().sources.is_empty());
    assert!(cold_index.manifest().provider_roots()[0]
        .routes()
        .is_empty());
    drop(cold_index);

    missing
        .registry
        .retain_unavailable_provider_root_routes(&retained_roots)
        .unwrap();
    assert_eq!(
        missing.registry.applied_provider_roots().unwrap().2[0]
            .routes()
            .len(),
        2
    );
    let missing_receipt = refresh_source_backed_generation(
        &index_root,
        &missing.registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    assert!(missing_receipt.failed_routes.is_empty());
    let retained_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        retained_index.manifest().provider_roots()[0].routes().len(),
        2
    );
    assert_marker(&retained_index, ALPHA_MARKER);
    assert_marker(&retained_index, BETA_MARKER);
    drop(retained_index);

    fs::rename(&displaced, &state).unwrap();
    let restored = build_registry(&context, &temp.path().join("restored-data"));
    assert!(restored.issues.is_empty(), "{:?}", restored.issues);
    assert_eq!(restored.executable_route_count(), 2);
    assert_eq!(route_ids(&restored.registry), initial_routes);
    let restored_receipt = refresh_source_backed_generation(
        &index_root,
        &restored.registry,
        source_backed_refresh_writer_options(),
    )
    .unwrap();
    assert!(restored_receipt.failed_routes.is_empty());
    let restored_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        restored_index.manifest().provider_roots()[0].routes().len(),
        2
    );
    assert_marker(&restored_index, ALPHA_MARKER);
    assert_marker(&restored_index, BETA_MARKER);
}
