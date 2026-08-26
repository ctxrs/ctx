#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use ctx_history_capture_composition::{
    refresh_source_backed_generation, register_landed_source_backed_route, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceStatus,
    SourceBackedCoordinatorError, SourceBackedProviderRegistry, SourceBackedRecordRejectionClass,
    SourceBackedRouteSelection, SourceBackedSourceFailureClass,
};
use ctx_history_core::CaptureProvider;
use ctx_history_index::{VerifiedIndex, WriterOptions};

fn write_session(root: &Path, session: &str, uuid: &str, marker: &str) {
    write_project_session(root, "project", session, uuid, marker);
}

fn write_project_session(root: &Path, project: &str, session: &str, uuid: &str, marker: &str) {
    let path = root.join(project).join(format!("{session}.jsonl"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        serde_json::json!({
            "type": "user",
            "uuid": uuid,
            "sessionId": session,
            "message": {"role": "user", "content": marker},
        })
        .to_string()
            + "\n",
    )
    .unwrap();
}

fn assert_duplicate_source_failure(
    receipt: &ctx_history_capture_composition::SourceBackedRefreshReceipt,
    carried_forward: bool,
) {
    assert!(receipt.failed_routes.is_empty());
    assert_eq!(receipt.logical_source_failures.total(), 1);
    let [failure] = receipt.logical_source_failures.failures() else {
        panic!("one duplicate Claude source failure expected");
    };
    assert_eq!(failure.class, SourceBackedSourceFailureClass::Unreadable);
    assert_eq!(failure.carried_forward, carried_forward);
    assert_eq!(failure.source.provider(), CaptureProvider::Claude.as_str());
    assert!(
        failure.detail.contains("repeats a native session identity"),
        "{failure:?}"
    );
}

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Claude,
            path: root.to_path_buf(),
            exists: true,
            source_format: "claude_projects_jsonl_tree",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
            route_provenance: Default::default(),
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn refresh(
    root: &Path,
    index: &Path,
) -> ctx_history_capture_composition::SourceBackedRefreshReceipt {
    refresh_source_backed_generation(
        index,
        &registry(root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap()
}

fn marker_count(index: &Path, marker: &str) -> usize {
    VerifiedIndex::open(index)
        .unwrap()
        .search_event_candidates(marker, 16)
        .unwrap()
        .len()
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn assert_unreadable_failure(
    receipt: &ctx_history_capture_composition::SourceBackedRefreshReceipt,
    carried_forward: bool,
) {
    assert!(receipt.failed_routes.is_empty());
    assert_eq!(receipt.logical_source_failures.total(), 1);
    let [failure] = receipt.logical_source_failures.failures() else {
        panic!("one Claude logical-source failure expected");
    };
    assert_eq!(failure.class, SourceBackedSourceFailureClass::Unreadable);
    assert_eq!(failure.carried_forward, carried_forward);
    assert_eq!(failure.source.provider(), CaptureProvider::Claude.as_str());
    assert!(failure.detail.contains("is unreadable"), "{failure:?}");
}

#[test]
fn cold_permission_denied_claude_file_quarantines_only_that_session_and_repairs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("projects");
    let index = temp.path().join("index");
    write_session(&root, "healthy", "healthy-cold", "healthycoldmarker");
    write_session(&root, "broken", "broken-cold", "mustnotpublish");
    let broken = root.join("project/broken.jsonl");
    set_mode(&broken, 0o000);

    let cold = refresh(&root, &index);
    assert_unreadable_failure(&cold, false);
    assert_eq!(marker_count(&index, "healthycoldmarker"), 1);
    assert_eq!(marker_count(&index, "mustnotpublish"), 0);

    set_mode(&broken, 0o600);
    write_session(&root, "broken", "broken-repaired", "coldrepairedmarker");
    let repaired = refresh(&root, &index);
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    assert_eq!(marker_count(&index, "healthycoldmarker"), 1);
    assert_eq!(marker_count(&index, "coldrepairedmarker"), 1);
}

#[test]
fn warm_permission_denied_claude_file_carries_last_good_while_sibling_advances() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("projects");
    let index = temp.path().join("index");
    write_session(&root, "healthy", "healthy-before", "healthybeforemarker");
    write_session(&root, "fragile", "fragile-before", "fragilebeforemarker");
    let initial = refresh(&root, &index);
    assert!(initial.logical_source_failures.is_empty());

    let fragile = root.join("project/fragile.jsonl");
    set_mode(&fragile, 0o000);
    write_session(&root, "healthy", "healthy-after", "healthyaftermarker");

    let quarantined = refresh(&root, &index);
    assert_unreadable_failure(&quarantined, true);
    assert_eq!(marker_count(&index, "healthybeforemarker"), 0);
    assert_eq!(marker_count(&index, "healthyaftermarker"), 1);
    assert_eq!(marker_count(&index, "fragilebeforemarker"), 1);

    set_mode(&fragile, 0o600);
    write_session(&root, "fragile", "fragile-after", "fragileaftermarker");
    let repaired = refresh(&root, &index);
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    assert_eq!(marker_count(&index, "healthyaftermarker"), 1);
    assert_eq!(marker_count(&index, "fragilebeforemarker"), 0);
    assert_eq!(marker_count(&index, "fragileaftermarker"), 1);
}

#[test]
fn cold_duplicate_claude_source_quarantines_only_the_ambiguous_session_and_repairs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("projects");
    let index = temp.path().join("index");
    write_project_session(
        &root,
        "project-a",
        "duplicate",
        "duplicate-a",
        "duplicateamarker",
    );
    write_project_session(
        &root,
        "project-b",
        "duplicate",
        "duplicate-b",
        "duplicatebmarker",
    );
    write_project_session(
        &root,
        "project-c",
        "healthy",
        "healthy-duplicate-peer",
        "healthyduplicatemarker",
    );

    let cold = refresh(&root, &index);
    assert_duplicate_source_failure(&cold, false);
    assert_eq!(marker_count(&index, "duplicateamarker"), 0);
    assert_eq!(marker_count(&index, "duplicatebmarker"), 0);
    assert_eq!(marker_count(&index, "healthyduplicatemarker"), 1);

    fs::remove_file(root.join("project-b/duplicate.jsonl")).unwrap();
    let repaired = refresh(&root, &index);
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    assert_eq!(marker_count(&index, "duplicateamarker"), 1);
    assert_eq!(marker_count(&index, "duplicatebmarker"), 0);
    assert_eq!(marker_count(&index, "healthyduplicatemarker"), 1);
}

#[test]
fn warm_unreadable_duplicate_claude_source_carries_last_good_while_sibling_advances() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("projects");
    let index = temp.path().join("index");
    write_project_session(
        &root,
        "project-a",
        "fragile",
        "fragile-before",
        "fragileduplicatebeforemarker",
    );
    write_project_session(
        &root,
        "project-c",
        "healthy",
        "healthy-before",
        "healthyduplicatebeforemarker",
    );
    let initial = refresh(&root, &index);
    assert!(initial.logical_source_failures.is_empty());

    write_project_session(
        &root,
        "project-a",
        "fragile",
        "fragile-after",
        "fragileduplicateaftermarker",
    );
    write_project_session(
        &root,
        "project-b",
        "fragile",
        "fragile-unreadable",
        "mustnotpublishduplicate",
    );
    let duplicate = root.join("project-b/fragile.jsonl");
    set_mode(&duplicate, 0o000);
    write_project_session(
        &root,
        "project-c",
        "healthy",
        "healthy-after",
        "healthyduplicateaftermarker",
    );

    let quarantined = refresh(&root, &index);
    assert_duplicate_source_failure(&quarantined, true);
    assert_eq!(marker_count(&index, "fragileduplicatebeforemarker"), 1);
    assert_eq!(marker_count(&index, "fragileduplicateaftermarker"), 0);
    assert_eq!(marker_count(&index, "mustnotpublishduplicate"), 0);
    assert_eq!(marker_count(&index, "healthyduplicatebeforemarker"), 0);
    assert_eq!(marker_count(&index, "healthyduplicateaftermarker"), 1);

    fs::remove_file(&duplicate).unwrap();
    let repaired = refresh(&root, &index);
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    assert_eq!(marker_count(&index, "fragileduplicatebeforemarker"), 0);
    assert_eq!(marker_count(&index, "fragileduplicateaftermarker"), 1);
    assert_eq!(marker_count(&index, "healthyduplicateaftermarker"), 1);
}

#[test]
fn malformed_claude_record_is_reported_without_losing_valid_records() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("projects");
    let index = temp.path().join("index");
    write_session(&root, "mixed", "mixed-valid", "mixedvalidmarker");
    let transcript = root.join("project/mixed.jsonl");
    let valid = fs::read_to_string(&transcript).unwrap();
    fs::write(&transcript, format!("not-json\n{valid}")).unwrap();

    let receipt = refresh(&root, &index);
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    assert_eq!(receipt.record_rejections.total(), 1);
    let [rejection] = receipt.record_rejections.rejections() else {
        panic!("one malformed Claude record rejection expected");
    };
    assert_eq!(
        rejection.class,
        SourceBackedRecordRejectionClass::MalformedRecord
    );
    assert_eq!(rejection.line_number, 1);
    assert_eq!(marker_count(&index, "mixedvalidmarker"), 1);
}

#[test]
fn fully_quarantined_claude_route_returns_no_usable_logical_sources() {
    // Claude does not opt in to the empty-quarantined-generation capability, so
    // a route whose every source is quarantined must fail the whole route with
    // NoUsableLogicalSources rather than committing an empty generation.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("projects");
    let index = temp.path().join("index");
    // Two files share one native session identity with no healthy peer: the
    // shared family quarantines the duplicate and leaves the route with no
    // usable logical source.
    write_project_session(
        &root,
        "project-a",
        "duplicate",
        "duplicate-a",
        "duplicateamarker",
    );
    write_project_session(
        &root,
        "project-b",
        "duplicate",
        "duplicate-b",
        "duplicatebmarker",
    );

    let result = refresh_source_backed_generation(
        &index,
        &registry(&root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    );
    let error = result.expect_err("a fully quarantined non-opt-in route must fail");
    assert!(
        matches!(
            error,
            SourceBackedCoordinatorError::NoUsableLogicalSources { .. }
        ),
        "unexpected error: {error:?}"
    );
}
