use super::*;

use crate::provider::source_backed::family::document::DocumentLeafExecutionPolicy;

/// Central policy declaration for production replacement-document routes.
///
/// Cline, Roo, and CodeBuddy discover a content-free exact source descriptor
/// per leaf, and each scan validates and certifies only that leaf. CodeBuddy's
/// extension project index is immutable evidence copied into each affected
/// leaf; it does not create cross-leaf session lineage.
///
/// Rovo Dev resolves immutable parent/root identity for every leaf during
/// complete discovery, so its cross-leaf dependency is fingerprinted before
/// independent scans begin. Auggie and Continue derive exact source identity
/// from document bodies, while NanoClaw is one catalog-lineage compound
/// source; those routes retain the serial default.
pub(crate) fn document_leaf_execution_policy(
    provider: CaptureProvider,
) -> DocumentLeafExecutionPolicy {
    match provider {
        CaptureProvider::Cline
        | CaptureProvider::RooCode
        | CaptureProvider::CodeBuddy
        | CaptureProvider::RovoDev => DocumentLeafExecutionPolicy::Independent,
        CaptureProvider::Auggie | CaptureProvider::Continue | CaptureProvider::NanoClaw => {
            DocumentLeafExecutionPolicy::Serial
        }
        _ => DocumentLeafExecutionPolicy::Serial,
    }
}

const DIRECT_ROUTES: &[RouteEntry] = &[
    RouteEntry::new(
        CaptureProvider::Auggie,
        crate::provider::providers::auggie::native_path::register_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::CodeBuddy,
        crate::provider::providers::codebuddy::native_path::register_source_backed_route,
    ),
];

pub(super) fn register_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    if let Some(register) = direct_route_registration(DIRECT_ROUTES, source.provider) {
        return register(registry, source, selection);
    }
    match source.provider {
        CaptureProvider::Cline | CaptureProvider::RooCode => {
            register_task_json_route(registry, source, selection)
        }
        CaptureProvider::RovoDev => register_rovodev_route(registry, source, selection),
        CaptureProvider::Continue => register_continue_route(registry, source, selection),
        provider => Err(invalid_route(
            provider,
            "this provider is not registered by the document route family",
        )),
    }
}

pub(super) fn register_task_json_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let selected = vec![source.clone()];
    let provider = source.provider;
    let adapter = match provider {
        CaptureProvider::Cline => cline_task_json_source_backed_adapter(&selected),
        CaptureProvider::RooCode => roo_task_json_source_backed_adapter(&selected),
        _ => unreachable!("caller restricts task JSON providers"),
    };
    crate::provider::source_backed::family::document::register_replacement_document_tree_route(
        registry, source, selection, adapter,
    )
}

/// Registers one explicit NanoClaw compound project with caller-owned catalog
/// lineage.
pub fn register_nanoclaw_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    data_root: &Path,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    register_nanoclaw_source_backed_route_with_base_sources(
        registry,
        source,
        data_root,
        catalog_lineage,
        &[],
    )
}

pub fn register_nanoclaw_source_backed_route_with_base_sources(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    data_root: &Path,
    catalog_lineage: [u8; 32],
    base_sources: &[CertifiedSource],
) -> SourceBackedCoordinatorResult<()> {
    register_nanoclaw_source_backed_route_with_selection(
        registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        data_root,
        catalog_lineage,
        base_sources,
    )
}

pub fn register_nanoclaw_source_backed_route_with_selection(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    catalog_lineage: [u8; 32],
    base_sources: &[CertifiedSource],
) -> SourceBackedCoordinatorResult<()> {
    let adapter = NanoClawDocumentTreeAdapter::new_with_base_sources(
        data_root,
        source.path.clone(),
        catalog_lineage,
        base_sources,
    )
    .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    crate::provider::source_backed::family::document::register_replacement_document_tree_route_with_authority(
        registry,
        source,
        selection,
        SourceBackedSelectorAuthority::CatalogLineage,
        adapter,
    )
}

pub(super) fn register_rovodev_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let context = ProviderAdapterContext {
        machine_id: "source-backed-rovodev".to_owned(),
        source_path: Some(source.path.clone()),
        source_root: Some(source.path.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let adapter = RovoDevDocumentTreeAdapter::new(source.path.clone(), context);
    crate::provider::source_backed::family::document::register_replacement_document_tree_route(
        registry, source, selection, adapter,
    )
}
pub(super) fn register_continue_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let outcome: ContinueSourceBackedOutcome =
        ContinueSourceBackedReader::register(registry, source, selection);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ProviderCatalogSupport, ProviderImportSupport, ProviderSourceKind, ProviderSourceStatus,
        ROVODEV_SOURCE_FORMAT,
    };
    use ctx_history_index::{CoreEventRecord, VerifiedIndex, WriterOptions};
    use std::{fs, path::Path};

    #[test]
    fn rovodev_transitive_root_rewrite_invalidates_unchanged_leaf_replay() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        write_rovodev_session(&root, "root-a", None);
        write_rovodev_session(&root, "root-b", None);
        write_rovodev_session(&root, "middle", Some("root-a"));
        write_rovodev_session(&root, "leaf", Some("middle"));
        let index = temp.path().join("index");
        let registry = rovodev_registry(&root);

        let cold =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let (cold_leaf_root, root_b_session) = leaf_and_root_b_ids(&index, &cold);
        assert_ne!(cold_leaf_root, root_b_session);

        write_rovodev_session(&root, "middle", Some("root-b"));
        let refreshed =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let (refreshed_leaf_root, refreshed_root_b_session) =
            leaf_and_root_b_ids(&index, &refreshed);
        assert_ne!(refreshed.commit.generation_id, cold.commit.generation_id);
        assert_ne!(refreshed_leaf_root, cold_leaf_root);
        assert_eq!(refreshed_leaf_root, refreshed_root_b_session);

        let replay =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        assert_eq!(replay.commit.generation_id, refreshed.commit.generation_id);
        assert_eq!(leaf_and_root_b_ids(&index, &replay).0, refreshed_leaf_root);
    }

    #[test]
    fn rovodev_append_rewrite_delete_and_exact_replay_preserve_stable_ids() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        write_rovodev_session_messages(&root, "leaf", None, &["one"]);
        let index = temp.path().join("index");
        let registry = rovodev_registry(&root);

        let cold =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let cold_events = rovodev_events(&index, &cold);
        assert_eq!(cold_events.len(), 1);
        let first_id = cold_events[0].event_id;

        write_rovodev_session_messages(&root, "leaf", None, &["one", "two"]);
        let append =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let append_events = rovodev_events(&index, &append);
        assert_eq!(append_events.len(), 2);
        assert_eq!(append_events[0].event_id, first_id);
        let append_ids = append_events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>();

        write_rovodev_session_messages(&root, "leaf", None, &["one revised", "two"]);
        let rewrite =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let rewrite_events = rovodev_events(&index, &rewrite);
        assert_eq!(
            rewrite_events
                .iter()
                .map(|event| event.event_id)
                .collect::<Vec<_>>(),
            append_ids
        );
        assert_eq!(
            rewrite_events[0].core_record.content.meaningful_text(),
            "one revised"
        );

        let replay =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        assert_eq!(replay.commit.generation_id, rewrite.commit.generation_id);
        assert_eq!(rovodev_events(&index, &replay), rewrite_events);

        fs::remove_dir_all(root.join("leaf")).unwrap();
        let deleted =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        assert_eq!(deleted.commit.indexed_documents, 0);
        assert_eq!(VerifiedIndex::open(&index).unwrap().document_count(), 0);
        let deleted_replay =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        assert_eq!(
            deleted_replay.commit.generation_id,
            deleted.commit.generation_id
        );
    }

    #[test]
    fn rovodev_parent_appearance_and_disappearance_invalidate_leaf_lineage() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        write_rovodev_session(&root, "leaf", Some("middle"));
        let index = temp.path().join("index");
        let registry = rovodev_registry(&root);

        let cold =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let cold_leaf_root = rovodev_events(&index, &cold)
            .into_iter()
            .find(|event| event.provider_session_id.as_deref() == Some("leaf"))
            .unwrap()
            .root_session_id;

        write_rovodev_session(&root, "root", None);
        write_rovodev_session(&root, "middle", Some("root"));
        let appeared =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let appeared_events = rovodev_events(&index, &appeared);
        let leaf = appeared_events
            .iter()
            .find(|event| event.provider_session_id.as_deref() == Some("leaf"))
            .unwrap();
        let lineage_root = appeared_events
            .iter()
            .find(|event| event.provider_session_id.as_deref() == Some("root"))
            .unwrap();
        assert_ne!(leaf.root_session_id, cold_leaf_root);
        assert_eq!(leaf.root_session_id, lineage_root.session_id);

        fs::remove_dir_all(root.join("middle")).unwrap();
        fs::remove_dir_all(root.join("root")).unwrap();
        let disappeared =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let disappeared_leaf_root = rovodev_events(&index, &disappeared)
            .into_iter()
            .find(|event| event.provider_session_id.as_deref() == Some("leaf"))
            .unwrap()
            .root_session_id;
        assert_eq!(disappeared_leaf_root, cold_leaf_root);

        let replay =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        assert_eq!(
            replay.commit.generation_id,
            disappeared.commit.generation_id
        );
    }

    #[test]
    fn rovodev_one_vs_four_workers_have_cold_and_unchanged_parity() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        for index in 0..8 {
            write_rovodev_session(&root, &format!("session-{index}"), None);
        }
        let registry = rovodev_registry(&root);
        let serial_index = temp.path().join("serial-index");
        let parallel_index = temp.path().join("parallel-index");

        let serial = refresh_source_backed_generation_with_work_budget_for_test(
            &serial_index,
            &registry,
            WriterOptions::default(),
            1,
        )
        .unwrap();
        let parallel = refresh_source_backed_generation_with_work_budget_for_test(
            &parallel_index,
            &registry,
            WriterOptions::default(),
            4,
        )
        .unwrap();
        assert_eq!(parallel.commit.generation_id, serial.commit.generation_id);
        assert_eq!(parallel.sources, serial.sources);
        assert_eq!(
            rovodev_events(&parallel_index, &parallel),
            rovodev_events(&serial_index, &serial)
        );

        let serial_replay = refresh_source_backed_generation_with_work_budget_for_test(
            &serial_index,
            &registry,
            WriterOptions::default(),
            1,
        )
        .unwrap();
        let parallel_replay = refresh_source_backed_generation_with_work_budget_for_test(
            &parallel_index,
            &registry,
            WriterOptions::default(),
            4,
        )
        .unwrap();
        assert_eq!(
            serial_replay.commit.generation_id,
            serial.commit.generation_id
        );
        assert_eq!(
            parallel_replay.commit.generation_id,
            parallel.commit.generation_id
        );
    }

    fn leaf_and_root_b_ids(
        index: &Path,
        receipt: &SourceBackedRefreshReceipt,
    ) -> (
        ctx_history_core::StableEntityId,
        ctx_history_core::StableEntityId,
    ) {
        let verified = VerifiedIndex::open(index).unwrap();
        let events = receipt
            .sources
            .iter()
            .flat_map(|source| {
                verified
                    .core_source_event_page(source.observation().source(), None, 8)
                    .unwrap()
                    .items
            })
            .collect::<Vec<_>>();
        let leaf = events
            .iter()
            .find(|event| event.provider_session_id.as_deref() == Some("leaf"))
            .unwrap();
        let root_b = events
            .iter()
            .find(|event| event.provider_session_id.as_deref() == Some("root-b"))
            .unwrap();
        (leaf.root_session_id, root_b.session_id)
    }

    fn rovodev_events(index: &Path, receipt: &SourceBackedRefreshReceipt) -> Vec<CoreEventRecord> {
        let verified = VerifiedIndex::open(index).unwrap();
        let mut events = receipt
            .sources
            .iter()
            .flat_map(|source| {
                verified
                    .core_source_event_page(source.observation().source(), None, 64)
                    .unwrap()
                    .items
            })
            .collect::<Vec<_>>();
        events.sort_by(|left, right| {
            left.provider_session_id
                .cmp(&right.provider_session_id)
                .then_with(|| left.event_sequence.cmp(&right.event_sequence))
        });
        events
    }

    fn write_rovodev_session(root: &Path, session: &str, parent: Option<&str>) {
        write_rovodev_session_messages(root, session, parent, &[&format!("body {session}")]);
    }

    fn write_rovodev_session_messages(
        root: &Path,
        session: &str,
        parent: Option<&str>,
        bodies: &[&str],
    ) {
        let directory = root.join(session);
        fs::create_dir_all(&directory).unwrap();
        let messages = bodies
            .iter()
            .enumerate()
            .map(|(index, body)| {
                serde_json::json!({
                    "id": format!("message-{session}-{index}"),
                    "timestamp": "2026-07-28T12:00:00Z",
                    "role": "assistant",
                    "content": body,
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            directory.join("session_context.json"),
            serde_json::to_vec(&serde_json::json!({
                "session_id": session,
                "message_history": messages,
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            directory.join("metadata.json"),
            serde_json::to_vec(&serde_json::json!({
                "session_id": session,
                "parent_session_id": parent,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn rovodev_registry(path: &Path) -> SourceBackedProviderRegistry {
        let mut registry = SourceBackedProviderRegistry::new();
        register_rovodev_route(
            &mut registry,
            ProviderSource {
                provider: CaptureProvider::RovoDev,
                path: path.to_path_buf(),
                exists: true,
                source_format: ROVODEV_SOURCE_FORMAT,
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
            },
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();
        registry
    }
}
