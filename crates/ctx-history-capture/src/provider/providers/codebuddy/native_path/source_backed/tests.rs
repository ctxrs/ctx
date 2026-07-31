use std::{
    fs::{self, File, FileTimes},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind, SourceRecordLocator,
};
use ctx_history_index::{EventRecord, VerifiedIndex, WriterOptions};
use serde_json::json;

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    test_support_paths::tempdir,
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

const IMPORTED_AT: &str = "2026-07-28T12:00:00Z";

fn write_store(root: &Path, cli: &[(&str, &str)], extension: &[(&str, &str)]) {
    write_cli(root, cli);
    let project = root.join("history/shared-project");
    let session = project.join("shared-session");
    fs::create_dir_all(session.join("messages")).unwrap();
    fs::write(
        session.join("index.json"),
        serde_json::to_vec(&json!({
            "messages": extension
                .iter()
                .map(|(id, _)| json!({"id": id, "type": "message", "role": "assistant"}))
                .collect::<Vec<_>>()
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        project.join("index.json"),
        serde_json::to_vec(&json!({
            "conversations": [{
                "id": "shared-session",
                "name": "Shared native IDs",
                "projectPath": "/workspace/codebuddy-ide",
                "createdAt": IMPORTED_AT,
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    for (id, text) in extension {
        fs::write(
            session.join("messages").join(format!("{id}.json")),
            serde_json::to_vec(&json!({
                "id": id,
                "role": "assistant",
                "content": text,
                "createdAt": IMPORTED_AT,
            }))
            .unwrap(),
        )
        .unwrap();
    }
}

fn write_cli(root: &Path, cli: &[(&str, &str)]) {
    let cli_path = root.join("projects/shared-project/shared-session.jsonl");
    fs::create_dir_all(cli_path.parent().unwrap()).unwrap();
    let cli_bytes = cli
        .iter()
        .map(|(id, text)| {
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "id": id,
                    "type": "message",
                    "role": "user",
                    "content": text,
                    "timestamp": IMPORTED_AT,
                    "sessionId": "shared-session",
                    "cwd": "/workspace/codebuddy-cli",
                }))
                .unwrap()
            )
        })
        .collect::<String>();
    fs::write(cli_path, cli_bytes).unwrap();
}

fn write_extension_sessions(root: &Path, sessions: usize) {
    let project = root.join("history/linear-project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("index.json"), b"{}").unwrap();
    for session in 0..sessions {
        let session = project.join(format!("session-{session:04}"));
        fs::create_dir_all(session.join("messages")).unwrap();
        fs::write(session.join("index.json"), b"{\"messages\":[]}").unwrap();
    }
}

fn write_cli_source(
    root: &Path,
    project: &str,
    session: &str,
    native_id: &str,
    text: &str,
) -> PathBuf {
    let path = root
        .join("projects")
        .join(project)
        .join(format!("{session}.jsonl"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string(&json!({
                "id": native_id,
                "type": "message",
                "role": "user",
                "content": text,
                "timestamp": IMPORTED_AT,
                "sessionId": session,
                "cwd": format!("/workspace/{project}"),
            }))
            .unwrap()
        ),
    )
    .unwrap();
    path
}

fn route_source(path: &Path) -> ProviderSource {
    ProviderSource {
        provider: CaptureProvider::CodeBuddy,
        path: path.to_path_buf(),
        exists: true,
        source_format: CODEBUDDY_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    }
}

fn registry_with_parse_count(path: &Path) -> (SourceBackedProviderRegistry, Arc<AtomicUsize>) {
    registry_with_execution(path, None)
}

fn registry_with_execution(
    path: &Path,
    execution: Option<(usize, Arc<CodeBuddyScanActivity>)>,
) -> (SourceBackedProviderRegistry, Arc<AtomicUsize>) {
    let source = route_source(path);
    let parse_count = Arc::new(AtomicUsize::new(0));
    let (leaf_workers, scan_activity) = execution
        .map(|(workers, activity)| (Some(workers), Some(activity)))
        .unwrap_or((None, None));
    let adapter = CodeBuddyDocumentAdapter {
        root: path.to_path_buf(),
        context: ProviderAdapterContext {
            machine_id: "source-backed-codebuddy-test".to_owned(),
            source_path: Some(path.to_path_buf()),
            source_root: Some(path.to_path_buf()),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        },
        parse_count: Some(Arc::clone(&parse_count)),
        leaf_workers,
        scan_activity,
    };
    let mut registry = SourceBackedProviderRegistry::new();
    register_replacement_document_tree_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
        adapter,
    )
    .unwrap();
    (registry, parse_count)
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

#[test]
fn codebuddy_production_route_declares_exact_independent_leaves() {
    let adapter = CodeBuddyDocumentAdapter {
        root: PathBuf::from("unused-codebuddy-policy-path"),
        context: ProviderAdapterContext {
            machine_id: "codebuddy-policy-test".to_owned(),
            source_path: None,
            source_root: None,
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        },
        parse_count: None,
        leaf_workers: None,
        scan_activity: None,
    };
    assert_eq!(
        adapter.leaf_execution_policy(),
        DocumentLeafExecutionPolicy::Independent
    );
}

fn source_events(index_root: &Path, source: &SourceKey) -> Vec<EventRecord> {
    let mut events = VerifiedIndex::open(index_root)
        .unwrap()
        .source_event_page(source, None, 16)
        .unwrap()
        .items;
    events.sort_by_key(|event| event.event_sequence);
    events
}

fn all_source_events(index_root: &Path) -> Vec<EventRecord> {
    let index = VerifiedIndex::open(index_root).unwrap();
    index
        .manifest()
        .sources
        .iter()
        .flat_map(|source| {
            index
                .source_event_page(source.observation().source(), None, 16)
                .unwrap()
                .items
        })
        .collect()
}

#[test]
fn dual_format_cold_scan_emits_independent_full_body_exact_records() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("codebuddy");
    let cli_body = format!("codebuddy-cli-head-{}-clitailsentinel", "c".repeat(20_000));
    let extension_body = format!(
        "codebuddy-extension-head-{}-extensiontailsentinel",
        "e".repeat(20_000)
    );
    write_store(
        &root,
        &[("cli-1", &cli_body), ("cli-2", "second cli body")],
        &[
            ("extension-1", &extension_body),
            ("extension-2", "second extension body"),
        ],
    );
    let (registry, parse_count) = registry_with_parse_count(&root);
    let index_root = temp.path().join("index");
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(parse_count.load(Ordering::Relaxed), 2);
    assert_eq!(receipt.sources.len(), 2);
    assert!(receipt
        .sources
        .iter()
        .all(|source| source.parser_revision() == PARSER_REVISION));

    let cli_source = codebuddy_source_key_for_path(
        CodeBuddySourceShape::Cli,
        &root.join("projects/shared-project/shared-session.jsonl"),
    )
    .unwrap();
    let extension_source = codebuddy_source_key_for_path(
        CodeBuddySourceShape::Extension,
        &root.join("history/shared-project/shared-session"),
    )
    .unwrap();
    let cli = source_events(&index_root, &cli_source);
    let extension = source_events(&index_root, &extension_source);
    let verified = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        verified
            .search_event_candidates("clitailsentinel", 8)
            .unwrap()
            .first()
            .map(|candidate| candidate.event.event_id),
        Some(cli[0].event_id)
    );
    assert_eq!(
        verified
            .search_event_candidates("extensiontailsentinel", 8)
            .unwrap()
            .first()
            .map(|candidate| candidate.event.event_id),
        Some(extension[0].event_id)
    );
    let resolver = registry.resolver_registry();
    assert_eq!(
        cli.iter()
            .map(|event| {
                resolver
                    .hydrate_event(
                        &EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap(),
                    )
                    .unwrap()
                    .provider_bytes
            })
            .collect::<Vec<_>>(),
        [cli_body.as_bytes().to_vec(), b"second cli body".to_vec()]
    );
    assert_eq!(
        extension
            .iter()
            .map(|event| {
                resolver
                    .hydrate_event(
                        &EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap(),
                    )
                    .unwrap()
                    .provider_bytes
            })
            .collect::<Vec<_>>(),
        [
            extension_body.as_bytes().to_vec(),
            b"second extension body".to_vec(),
        ]
    );
    assert!(cli.iter().all(|event| {
        event.locator.source().schema_variant() == CODEBUDDY_CLI_SCHEMA_VARIANT
            && event.provider_session_id.as_deref() == Some("shared-project/shared-session")
            && event.cwd.as_deref() == Some("/workspace/codebuddy-cli")
    }));
    assert!(extension.iter().all(|event| {
        event.locator.source().schema_variant() == CODEBUDDY_EXTENSION_SCHEMA_VARIANT
            && event.provider_session_id.as_deref() == Some("shared-project/shared-session")
            && event.cwd.as_deref() == Some("/workspace/codebuddy-ide")
    }));

    let cli_request = EventHydrationRequest::new(cli[0].event_id, cli[0].locator.clone()).unwrap();
    assert_eq!(
        resolver.hydrate_event(&cli_request).unwrap().provider_bytes,
        cli_body.as_bytes()
    );

    let requests = [1_usize, 0]
        .into_iter()
        .map(|index| {
            EventHydrationRequest::new(extension[index].event_id, extension[index].locator.clone())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let batch = BatchHydrationRequest::new(requests.clone()).unwrap();
    reset_body_reads();
    let hydrated = hydrate_codebuddy_group(&root, &batch).unwrap();
    assert_eq!(body_reads(), 4, "two indexes and two message files");
    assert_eq!(
        hydrated
            .records()
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        requests
            .iter()
            .map(EventHydrationRequest::event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        hydrated
            .records()
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        [
            b"second extension body".as_slice(),
            extension_body.as_bytes()
        ]
    );

    let stale_locator = SourceRecordLocator::new(
        extension[0].locator.source().clone(),
        extension[0].locator.coordinate().clone(),
        extension[0].locator.revision_policy(),
        extension[0]
            .locator
            .certified_source_revision_digest()
            .copied(),
        [0_u8; 32],
    )
    .unwrap();
    let stale = EventHydrationRequest::new(extension[0].event_id, stale_locator).unwrap();
    let partly_valid = BatchHydrationRequest::new(vec![requests[0].clone(), stale]).unwrap();
    assert_eq!(
        hydrate_codebuddy_group(&root, &partly_valid)
            .unwrap_err()
            .kind,
        HydrationFailureKind::StaleRecordEvidence
    );
}

#[test]
fn independent_codebuddy_leaves_have_one_vs_four_generation_and_event_parity() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("parallel-codebuddy");
    write_store(
        &root,
        &[("shared-cli", "shared cli body")],
        &[("shared-extension", "shared extension body")],
    );
    write_cli_source(
        &root,
        "extra-project-a",
        "extra-session-a",
        "extra-message-a",
        "extra cli body a",
    );
    let second_extra = write_cli_source(
        &root,
        "extra-project-b",
        "extra-session-b",
        "extra-message-b",
        "extra cli body b",
    );
    let serial_activity = CodeBuddyScanActivity::new(1);
    let parallel_activity = CodeBuddyScanActivity::new(4);
    let (serial_registry, serial_scans) =
        registry_with_execution(&root, Some((1, Arc::clone(&serial_activity))));
    let (parallel_registry, parallel_scans) =
        registry_with_execution(&root, Some((4, Arc::clone(&parallel_activity))));
    let serial_root = temp.path().join("serial-leaves-index");
    let parallel_root = temp.path().join("parallel-leaves-index");

    let serial_cold =
        refresh_source_backed_generation(&serial_root, &serial_registry, writer_options()).unwrap();
    let parallel_cold =
        refresh_source_backed_generation(&parallel_root, &parallel_registry, writer_options())
            .unwrap();
    assert_eq!(
        parallel_cold.commit.generation_id,
        serial_cold.commit.generation_id
    );
    assert_eq!(parallel_cold.sources, serial_cold.sources);
    assert_eq!(
        all_source_events(&parallel_root),
        all_source_events(&serial_root)
    );
    assert_eq!(serial_activity.peak(), 1);
    assert_eq!(parallel_activity.peak(), 4);
    assert_eq!(serial_scans.load(Ordering::Relaxed), 4);
    assert_eq!(parallel_scans.load(Ordering::Relaxed), 4);

    let serial_noop =
        refresh_source_backed_generation(&serial_root, &serial_registry, writer_options()).unwrap();
    let parallel_noop =
        refresh_source_backed_generation(&parallel_root, &parallel_registry, writer_options())
            .unwrap();
    assert_eq!(
        serial_noop.commit.generation_id,
        serial_cold.commit.generation_id
    );
    assert_eq!(
        parallel_noop.commit.generation_id,
        parallel_cold.commit.generation_id
    );
    assert_eq!(serial_scans.load(Ordering::Relaxed), 4);
    assert_eq!(parallel_scans.load(Ordering::Relaxed), 4);

    serial_activity.disable_barrier();
    parallel_activity.disable_barrier();
    write_cli_source(
        &root,
        "extra-project-a",
        "extra-session-a",
        "extra-message-a",
        "replacement cli body a",
    );
    let serial_changed =
        refresh_source_backed_generation(&serial_root, &serial_registry, writer_options()).unwrap();
    let parallel_changed =
        refresh_source_backed_generation(&parallel_root, &parallel_registry, writer_options())
            .unwrap();
    assert_eq!(
        parallel_changed.commit.generation_id,
        serial_changed.commit.generation_id
    );
    assert_eq!(parallel_changed.sources, serial_changed.sources);
    assert_eq!(
        all_source_events(&parallel_root),
        all_source_events(&serial_root)
    );
    assert_eq!(serial_scans.load(Ordering::Relaxed), 5);
    assert_eq!(parallel_scans.load(Ordering::Relaxed), 5);

    fs::remove_file(second_extra).unwrap();
    let serial_deleted =
        refresh_source_backed_generation(&serial_root, &serial_registry, writer_options()).unwrap();
    let parallel_deleted =
        refresh_source_backed_generation(&parallel_root, &parallel_registry, writer_options())
            .unwrap();
    assert_eq!(
        parallel_deleted.commit.generation_id,
        serial_deleted.commit.generation_id
    );
    assert_eq!(parallel_deleted.sources, serial_deleted.sources);
    assert_eq!(parallel_deleted.removals, serial_deleted.removals);
    assert_eq!(
        all_source_events(&parallel_root),
        all_source_events(&serial_root)
    );
    assert_eq!(serial_scans.load(Ordering::Relaxed), 5);
    assert_eq!(parallel_scans.load(Ordering::Relaxed), 5);
}

#[test]
fn cold_noop_change_delete_parity_and_unavailable_retention() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("codebuddy");
    write_store(
        &root,
        &[("cli-1", "before body")],
        &[("extension-1", "stable extension")],
    );
    let cli_path = root.join("projects/shared-project/shared-session.jsonl");
    let (registry, parse_count) = registry_with_parse_count(&root);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(parse_count.load(Ordering::Relaxed), 2);
    let cli_source = codebuddy_source_key_for_path(CodeBuddySourceShape::Cli, &cli_path).unwrap();
    let cold_cli = source_events(&index_root, &cli_source);
    assert_eq!(cold_cli.len(), 1);
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(parse_count.load(Ordering::Relaxed), 2);

    let original_modified = fs::metadata(&cli_path).unwrap().modified().unwrap();
    let bytes = fs::read(&cli_path).unwrap();
    let replacement = String::from_utf8(bytes)
        .unwrap()
        .replace("before body", "after- body");
    fs::write(&cli_path, replacement).unwrap();
    File::options()
        .write(true)
        .open(&cli_path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(original_modified))
        .unwrap();
    let changed =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(parse_count.load(Ordering::Relaxed), 3);
    assert_ne!(changed.commit.generation_id, cold.commit.generation_id);
    let changed_cli = source_events(&index_root, &cli_source);
    assert_eq!(changed_cli.len(), 1);
    assert_eq!(changed_cli[0].event_id, cold_cli[0].event_id);
    assert!(changed_cli[0]
        .locator
        .source()
        .exact_descriptor_eq(cold_cli[0].locator.source()));
    let changed_request =
        EventHydrationRequest::new(changed_cli[0].event_id, changed_cli[0].locator.clone())
            .unwrap();
    assert_eq!(
        registry
            .resolver_registry()
            .hydrate_event(&changed_request)
            .unwrap()
            .provider_bytes,
        b"after- body"
    );

    fs::remove_file(&cli_path).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(deleted.sources.len(), 1);
    assert_eq!(deleted.removals.len(), 1);
    assert_eq!(parse_count.load(Ordering::Relaxed), 3);

    write_cli(&root, &[("cli-1", "restored cli")]);
    let restored =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(parse_count.load(Ordering::Relaxed), 4);
    let retained_generation = restored.commit.generation_id;
    let retired = temp.path().join("retired-codebuddy");
    fs::rename(&root, &retired).unwrap();
    assert!(refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err());
    assert_eq!(parse_count.load(Ordering::Relaxed), 4);
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        retained_generation
    );
}

#[test]
fn indexed_discovery_preserves_source_identity_and_full_scan_fingerprints() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("codebuddy");
    write_store(&root, &[("cli-1", "cli")], &[("extension-1", "extension")]);
    let first = discover_codebuddy_tree(&root)
        .unwrap()
        .into_complete_tree()
        .unwrap();
    assert!(first
        .authority
        .indexed_extension_fingerprints_match_full_scan(&first.leaves));
    for leaf in &first.leaves {
        let shape = match leaf.provider_leaf.kind {
            DocumentLeafKind::Cli { .. } => CodeBuddySourceShape::Cli,
            DocumentLeafKind::Extension { .. } => CodeBuddySourceShape::Extension,
        };
        let expected =
            codebuddy_source_key_for_path(shape, leaf.provider_leaf.logical_path()).unwrap();
        assert!(expected.exact_descriptor_eq(&leaf.provider_leaf.source));
    }
    let first_identity = first
        .leaves
        .iter()
        .map(|leaf| {
            (
                leaf.provider_leaf.logical_path().to_path_buf(),
                leaf.provider_leaf.source.exact_descriptor_digest(),
                leaf.fingerprint,
            )
        })
        .collect::<Vec<_>>();
    let second = discover_codebuddy_tree(&root)
        .unwrap()
        .into_complete_tree()
        .unwrap();
    let second_identity = second
        .leaves
        .iter()
        .map(|leaf| {
            (
                leaf.provider_leaf.logical_path().to_path_buf(),
                leaf.provider_leaf.source.exact_descriptor_digest(),
                leaf.fingerprint,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(second.tree_fingerprint, first.tree_fingerprint);
    assert_eq!(second_identity, first_identity);
}

#[test]
fn two_thousand_extension_sessions_have_linear_deterministic_discovery() {
    const SESSION_COUNT: usize = 2_000;
    const MAX_ROUTE_INSPECTIONS_PER_ROUTE: usize = 7;
    const MAX_INDEX_LOOKUPS_PER_ROUTE: usize = 4;

    let temp = tempdir().unwrap();
    let root = temp.path().join("codebuddy");
    write_extension_sessions(&root, SESSION_COUNT);

    reset_discovery_operations();
    let tree = discover_codebuddy_tree(&root)
        .unwrap()
        .into_complete_tree()
        .unwrap();
    let discovery = discovery_operations();
    let route_count = tree.authority.route_count();
    assert_eq!(tree.leaves.len(), SESSION_COUNT);
    assert!(
        discovery.route_inspections <= route_count * MAX_ROUTE_INSPECTIONS_PER_ROUTE,
        "discovery inspected routes superlinearly: {discovery:?}, routes={route_count}"
    );
    assert!(
        discovery.indexed_path_lookups <= route_count * MAX_INDEX_LOOKUPS_PER_ROUTE,
        "discovery path lookups exceeded the indexed bound: {discovery:?}, routes={route_count}"
    );
    assert!(
        discovery.indexed_parent_lookups <= route_count * MAX_INDEX_LOOKUPS_PER_ROUTE,
        "discovery parent lookups exceeded the indexed bound: {discovery:?}, routes={route_count}"
    );

    reset_discovery_operations();
    assert_eq!(
        revalidate_codebuddy_tree(&tree).unwrap(),
        tree.tree_fingerprint
    );
    let terminal = discovery_operations();
    assert!(
        terminal.route_inspections <= route_count * MAX_ROUTE_INSPECTIONS_PER_ROUTE,
        "terminal revalidation inspected routes superlinearly: {terminal:?}, routes={route_count}"
    );
    assert!(
        terminal.indexed_path_lookups <= route_count * MAX_INDEX_LOOKUPS_PER_ROUTE,
        "terminal path lookups exceeded the indexed bound: {terminal:?}, routes={route_count}"
    );
    assert!(
        terminal.indexed_parent_lookups <= route_count * MAX_INDEX_LOOKUPS_PER_ROUTE,
        "terminal parent lookups exceeded the indexed bound: {terminal:?}, routes={route_count}"
    );
}

#[test]
fn catalog_is_body_free_bounded_and_collapses_hardlink_aliases() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("many/codebuddy");
    let project = root.join("projects/project");
    fs::create_dir_all(&project).unwrap();
    for index in 0..2_000 {
        fs::write(project.join(format!("{index:04}.jsonl")), b"not parsed").unwrap();
    }
    reset_body_reads();
    let inventory = discover_codebuddy_tree(&root).unwrap();
    assert_eq!(inventory.status, CodeBuddyInventoryStatus::Complete);
    let tree = inventory.into_complete_tree().unwrap();
    assert_eq!(tree.leaves.len(), 2_000);
    assert_eq!(tree.authority.retained_handles(), 1);
    assert!(tree.authority.route_count() >= 2_003);
    assert_eq!(body_reads(), 0);
    drop(tree);

    #[cfg(unix)]
    {
        let links = temp.path().join("links/projects/project");
        fs::create_dir_all(&links).unwrap();
        let original = links.join("b.jsonl");
        let alias = links.join("a.jsonl");
        fs::write(&original, b"{}\n").unwrap();
        fs::hard_link(&original, &alias).unwrap();
        let tree = discover_codebuddy_tree(links.parent().unwrap().parent().unwrap())
            .unwrap()
            .into_complete_tree()
            .unwrap();
        assert_eq!(tree.leaves.len(), 1);
        let DocumentLeafKind::Cli { selected, aliases } = &tree.leaves[0].provider_leaf.kind else {
            panic!("hardlink catalog did not produce a CLI leaf");
        };
        assert_eq!(selected.display_path, alias);
        assert_eq!(aliases, &[alias.clone(), original.clone()]);
        let fingerprint = tree.tree_fingerprint;
        fs::remove_file(&alias).unwrap();
        assert_ne!(revalidate_codebuddy_tree(&tree).unwrap(), fingerprint);
        assert_ne!(
            discover_codebuddy_tree(links.parent().unwrap().parent().unwrap())
                .unwrap()
                .into_complete_tree()
                .unwrap()
                .tree_fingerprint,
            fingerprint
        );
    }
}

#[cfg(unix)]
#[test]
fn catalog_fences_root_leaf_symlink_and_nonregular_swaps() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt, os::unix::fs::symlink};

    let temp = tempdir().unwrap();
    let root = temp.path().join("codebuddy");
    write_store(&root, &[("cli-1", "cli")], &[("extension-1", "extension")]);
    let tree = discover_codebuddy_tree(&root)
        .unwrap()
        .into_complete_tree()
        .unwrap();
    let message = root.join("history/shared-project/shared-session/messages/extension-1.json");
    let bytes = fs::read(&message).unwrap();
    fs::rename(&message, message.with_extension("retired")).unwrap();
    fs::write(&message, bytes).unwrap();
    assert_ne!(
        revalidate_codebuddy_tree(&tree).unwrap(),
        tree.tree_fingerprint
    );

    let tree = discover_codebuddy_tree(&root)
        .unwrap()
        .into_complete_tree()
        .unwrap();
    let retired = temp.path().join("retired-root");
    fs::rename(&root, &retired).unwrap();
    write_store(
        &root,
        &[("cli-1", "replacement")],
        &[("extension-1", "replacement")],
    );
    assert!(revalidate_codebuddy_tree(&tree).is_err());

    let outside = temp.path().join("outside.jsonl");
    fs::write(&outside, b"{}\n").unwrap();
    let attack = temp.path().join("attack");
    fs::create_dir(&attack).unwrap();
    symlink(&outside, attack.join("source.jsonl")).unwrap();
    assert!(discover_codebuddy_tree(&attack).is_err());

    let fifo_root = temp.path().join("fifo");
    fs::create_dir(&fifo_root).unwrap();
    let fifo = fifo_root.join("source.jsonl");
    let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
    assert!(discover_codebuddy_tree(&fifo_root).is_err());
}

#[test]
fn production_route_is_thin_and_below_the_loc_gate() {
    let source_backed = include_str!("../source_backed.rs");
    let discovery = include_str!("../discovery.rs");
    let discovery_catalog = include_str!("../discovery/catalog.rs");
    let discovery_index = include_str!("../discovery/index.rs");
    let discovery_inspection = include_str!("../discovery/inspection.rs");
    let parsing = include_str!("../parsing.rs");
    let hydration = include_str!("hydration.rs");
    let discovery_sources = [
        discovery,
        discovery_catalog,
        discovery_index,
        discovery_inspection,
    ];
    for (name, source, maximum_lines) in [
        ("source_backed", source_backed, 1_000),
        ("discovery", discovery, 1_000),
        ("discovery/catalog", discovery_catalog, 1_000),
        ("discovery/index", discovery_index, 1_000),
        ("discovery/inspection", discovery_inspection, 1_000),
        ("parsing", parsing, 1_000),
        ("hydration", hydration, 1_000),
    ] {
        assert!(
            source.lines().count() < maximum_lines,
            "{name} exceeded its production LOC gate"
        );
    }
    let forbidden_driver = ["captured_route", "_driver"].concat();
    for forbidden in [
        forbidden_driver.as_str(),
        "CodeBuddySourceBackedScan",
        "CodeBuddySourceBackedPage",
        "bind_codebuddy_capability",
        "revalidate_codebuddy_capability",
        "Vec<LexicalDocument>",
    ] {
        assert!(
            !source_backed.contains(forbidden)
                && discovery_sources
                    .iter()
                    .all(|source| !source.contains(forbidden))
                && !hydration.contains(forbidden),
            "CodeBuddy restored obsolete lifecycle code: {forbidden}"
        );
    }
    assert_eq!(
        source_backed
            .matches("register_replacement_document_tree_route(")
            .count(),
        1,
        "exactly one shared-family registration call is expected"
    );
}
