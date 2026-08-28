use super::*;

use crate::provider::source_backed::family::document::register_replacement_document_tree_route;
use ctx_history_core::SourceAnchorScope;
use ctx_history_providers_task_docs::{
    providers::{
        cline_sdk::ClineSdkDocumentTreeAdapter,
        codebuddy::native_path::CodeBuddyDocumentAdapter,
        continue_cli::native_path::ContinueSourceBackedReader,
        rovodev::native_path::RovoDevDocumentTreeAdapter,
        task_json::cline_nativepath::{
            cline_task_json_source_backed_adapter_scoped,
            roo_task_json_source_backed_adapter_scoped,
        },
    },
    ProviderAdapterContext, CLINE_SDK_SOURCE_FORMAT,
};

pub(super) fn register_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: Option<&Path>,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    if source.provider == CaptureProvider::Auggie {
        return crate::provider::providers::auggie::native_path::register_source_backed_route(
            registry,
            source,
            selection,
            source_root_lineage,
        );
    }
    match source.provider {
        CaptureProvider::Cline if source.source_format == CLINE_SDK_SOURCE_FORMAT => {
            let data_root = data_root.ok_or_else(|| {
                invalid_route(
                    source.provider,
                    "Cline SDK registration requires the selected ctx data root",
                )
            })?;
            register_cline_sdk_route(registry, source, selection, data_root, source_root_lineage)
        }
        CaptureProvider::Cline | CaptureProvider::RooCode => {
            register_task_json_route(registry, source, selection, source_root_lineage)
        }
        CaptureProvider::CodeBuddy => {
            register_codebuddy_route(registry, source, selection, source_root_lineage)
        }
        CaptureProvider::RovoDev => {
            register_rovodev_route(registry, source, selection, source_root_lineage)
        }
        CaptureProvider::Continue => {
            register_continue_route(registry, source, selection, source_root_lineage)
        }
        provider => Err(invalid_route(
            provider,
            "this provider is not registered by the document route family",
        )),
    }
}

fn register_cline_sdk_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let adapter = ClineSdkDocumentTreeAdapter::new_scoped(
        source.path.clone(),
        data_root.to_path_buf(),
        source_root_lineage.map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
    );
    register_replacement_document_tree_route(registry, source, selection, adapter)
}

pub(super) fn register_task_json_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let selected = vec![source.clone()];
    let provider = source.provider;
    let adapter = match provider {
        CaptureProvider::Cline => cline_task_json_source_backed_adapter_scoped(
            &selected,
            source_root_lineage.map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
        ),
        CaptureProvider::RooCode => roo_task_json_source_backed_adapter_scoped(
            &selected,
            source_root_lineage.map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
        ),
        _ => {
            return Err(invalid_route(
                provider,
                "caller restricts task JSON providers",
            ));
        }
    };
    register_replacement_document_tree_route(registry, source, selection, adapter)
}

pub(super) fn register_codebuddy_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let context = ProviderAdapterContext {
        machine_id: "source-backed-codebuddy".to_owned(),
        source_path: Some(source.path.clone()),
        source_root: Some(source.path.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let adapter = CodeBuddyDocumentAdapter::new_scoped(
        source.path.clone(),
        context,
        source_root_lineage.map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
    );
    register_replacement_document_tree_route(registry, source, selection, adapter)
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
    let adapter = NanoClawDocumentTreeAdapter::<CaptureProviderRuntime>::new_with_base_sources(
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
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let context = ProviderAdapterContext {
        machine_id: "source-backed-rovodev".to_owned(),
        source_path: Some(source.path.clone()),
        source_root: Some(source.path.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let adapter = RovoDevDocumentTreeAdapter::new_scoped(
        source.path.clone(),
        context,
        source_root_lineage.map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
    );
    register_replacement_document_tree_route(registry, source, selection, adapter)
}
pub(super) fn register_continue_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    source_root_lineage: Option<[u8; 32]>,
) -> SourceBackedCoordinatorResult<()> {
    let adapter = ContinueSourceBackedReader::new_scoped(
        source.path.clone(),
        source_root_lineage.map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
    );
    register_replacement_document_tree_route(registry, source, selection, adapter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ProviderCatalogSupport, ProviderImportSupport, ProviderSourceKind, ProviderSourceStatus,
    };
    use ctx_history_index::{CoreEventRecord, VerifiedIndex, WriterOptions};
    use std::{fs, path::Path};

    #[test]
    fn rovodev_parent_rewrite_preserves_unchanged_child_identity() {
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
        let cold_leaf_identity = leaf_identity(&index, &cold);

        write_rovodev_session(&root, "middle", Some("root-b"));
        let refreshed =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let refreshed_leaf_identity = leaf_identity(&index, &refreshed);
        assert_ne!(refreshed.commit.generation_id, cold.commit.generation_id);
        assert_eq!(refreshed_leaf_identity, cold_leaf_identity);

        let replay =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        assert_eq!(replay.commit.generation_id, refreshed.commit.generation_id);
        assert_eq!(leaf_identity(&index, &replay), refreshed_leaf_identity);
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
        assert_eq!(
            VerifiedIndex::open_pinned(&index).unwrap().document_count(),
            0
        );
        let deleted_replay =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        assert_eq!(
            deleted_replay.commit.generation_id,
            deleted.commit.generation_id
        );
    }

    #[test]
    fn rovodev_missing_parent_preserves_literal_parent_and_child_identity() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        write_rovodev_session(&root, "leaf", Some("middle"));
        let index = temp.path().join("index");
        let registry = rovodev_registry(&root);

        let unresolved =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let unresolved_events = rovodev_events(&index, &unresolved);
        let [unresolved_leaf] = unresolved_events.as_slice() else {
            panic!("one unresolved Rovo Dev leaf expected");
        };
        let child_event_id = unresolved_leaf.event_id;
        let child_session_id = unresolved_leaf.session_id;
        let direct_parent_id = unresolved_leaf
            .parent_session_id
            .expect("direct parent claim");

        write_rovodev_session(&root, "root", None);
        write_rovodev_session(&root, "middle", Some("root"));
        let published =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let published_events = rovodev_events(&index, &published);
        let leaf = published_events
            .iter()
            .find(|event| event.provider_session_id.as_deref() == Some("leaf"))
            .unwrap();
        let lineage_root = published_events
            .iter()
            .find(|event| event.provider_session_id.as_deref() == Some("root"))
            .unwrap();
        let middle = published_events
            .iter()
            .find(|event| event.provider_session_id.as_deref() == Some("middle"))
            .unwrap();
        assert_eq!(leaf.event_id, child_event_id);
        assert_eq!(leaf.session_id, child_session_id);
        assert_eq!(leaf.parent_session_id, Some(middle.session_id));
        assert_eq!(middle.parent_session_id, Some(lineage_root.session_id));

        fs::remove_dir_all(root.join("middle")).unwrap();
        fs::remove_dir_all(root.join("root")).unwrap();
        let deleted =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        assert_ne!(deleted.commit.generation_id, published.commit.generation_id);
        let deleted_events = rovodev_events(&index, &deleted);
        let [deleted_leaf] = deleted_events.as_slice() else {
            panic!("unchanged child must remain after parent deletion");
        };
        assert_eq!(deleted_leaf.event_id, child_event_id);
        assert_eq!(deleted_leaf.session_id, child_session_id);
        assert_eq!(deleted_leaf.parent_session_id, Some(direct_parent_id));

        let replay =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        assert_eq!(replay.commit.generation_id, deleted.commit.generation_id);
        assert_eq!(rovodev_events(&index, &replay), deleted_events);
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

    #[test]
    fn cline_sdk_compound_route_append_rewrite_malformed_recovery_and_delete() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let provider_root = temp.path().join("cline-data");
        let ctx_data_root = temp.path().join("ctx-data");
        let index = temp.path().join("index");
        fs::create_dir_all(provider_root.join("sessions/session-a")).unwrap();
        fs::create_dir_all(&ctx_data_root).unwrap();
        write_cline_sdk_index(&provider_root, true);
        write_cline_sdk_messages(&provider_root, &["one"]);
        let registry = cline_sdk_registry(&provider_root, &ctx_data_root);

        let cold =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let cold_events = cline_sdk_events(&index, &cold);
        let first_id = cold_events
            .iter()
            .find(|event| event.event_sequence > 0)
            .unwrap()
            .event_id;

        write_cline_sdk_messages(&provider_root, &["one", "two"]);
        let append =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let append_events = cline_sdk_events(&index, &append);
        assert_eq!(
            append_events
                .iter()
                .find(|event| event.event_sequence > 0)
                .unwrap()
                .event_id,
            first_id
        );

        write_cline_sdk_messages(&provider_root, &["one revised", "two"]);
        let rewrite =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let rewrite_events = cline_sdk_events(&index, &rewrite);
        let first = rewrite_events
            .iter()
            .find(|event| event.event_sequence > 0)
            .unwrap();
        assert_eq!(first.event_id, first_id);
        assert_eq!(first.core_record.content.meaningful_text(), "one revised");

        fs::write(
            provider_root.join("sessions/session-a/session-a.messages.json"),
            b"{malformed",
        )
        .unwrap();
        let malformed =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        assert_eq!(malformed.commit.generation_id, rewrite.commit.generation_id);
        assert_eq!(
            VerifiedIndex::open_pinned(&index).unwrap().document_count(),
            3
        );

        write_cline_sdk_messages(&provider_root, &["repaired"]);
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        write_cline_sdk_index(&provider_root, false);
        let deleted =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        assert_eq!(deleted.commit.indexed_documents, 0);
        assert_eq!(
            VerifiedIndex::open_pinned(&index).unwrap().document_count(),
            0
        );
    }

    #[test]
    fn cline_sdk_durable_replay_is_bound_to_the_leaf_root_scope() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let provider_root = temp.path().join("cline-data");
        let ctx_data_root = temp.path().join("ctx-data");
        let index = temp.path().join("index");
        fs::create_dir_all(provider_root.join("sessions/session-a")).unwrap();
        fs::create_dir_all(&ctx_data_root).unwrap();
        write_cline_sdk_index(&provider_root, true);
        write_cline_sdk_messages(&provider_root, &["same content"]);

        let scope_a = ctx_history_core::SourceAnchorScope::Lineage([0x11; 32]);
        let scope_b = ctx_history_core::SourceAnchorScope::Lineage([0x22; 32]);
        let registry_a = cline_sdk_registry_scoped(&provider_root, &ctx_data_root, scope_a);
        let cold_a =
            refresh_source_backed_generation(&index, &registry_a, WriterOptions::default())
                .unwrap();
        assert_eq!(cold_a.sources.len(), 1);
        assert!(cold_a.sources[0].frontier().is_some());
        let source_a = cold_a.sources[0].observation().source().clone();
        let session_a = cline_sdk_events(&index, &cold_a)[0].session_id;

        let replay_a =
            refresh_source_backed_generation(&index, &registry_a, WriterOptions::default())
                .unwrap();
        assert_eq!(replay_a.commit.generation_id, cold_a.commit.generation_id);
        assert_eq!(replay_a.sources, cold_a.sources);

        let registry_b = cline_sdk_registry_scoped(&provider_root, &ctx_data_root, scope_b);
        let refreshed_b =
            refresh_source_backed_generation(&index, &registry_b, WriterOptions::default())
                .unwrap();
        assert_ne!(
            refreshed_b.commit.generation_id,
            cold_a.commit.generation_id
        );
        assert_eq!(refreshed_b.sources.len(), 1);
        let source_b = refreshed_b.sources[0].observation().source();
        assert!(!source_a.exact_descriptor_eq(source_b));
        assert_ne!(source_a.identity(), source_b.identity());
        assert_eq!(
            cold_a.sources[0].frontier(),
            refreshed_b.sources[0].frontier()
        );
        assert_eq!(
            cold_a.sources[0].content_digest(),
            refreshed_b.sources[0].content_digest()
        );

        let events_b = cline_sdk_events(&index, &refreshed_b);
        assert_ne!(events_b[0].session_id, session_a);
        assert!(events_b
            .iter()
            .any(|event| { event.core_record.content.meaningful_text() == "same content" }));
        assert!(matches!(
            VerifiedIndex::open_pinned(&index)
                .unwrap()
                .core_source_event_page(&source_a, None, 64),
            Err(ctx_history_index::IndexError::SourceEventSourceNotRetained(
                _
            ))
        ));

        let replay_b =
            refresh_source_backed_generation(&index, &registry_b, WriterOptions::default())
                .unwrap();
        assert_eq!(
            replay_b.commit.generation_id,
            refreshed_b.commit.generation_id
        );
        assert_eq!(replay_b.sources, refreshed_b.sources);
    }

    #[test]
    fn cline_sdk_real_automatic_and_exact_discovery_import_the_common_data_root() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let home = temp.path().join("home");
        let cwd = temp.path().join("work");
        let provider_root = home.join(".cline/data");
        let ctx_data_root = temp.path().join("ctx-data");
        let automatic_index = temp.path().join("automatic-index");
        let exact_index = temp.path().join("exact-index");
        fs::create_dir_all(provider_root.join("sessions/session-a")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&ctx_data_root).unwrap();
        write_cline_sdk_index(&provider_root, true);
        write_cline_sdk_messages(&provider_root, &["one"]);

        let discovery = crate::DiscoveryContext::new(
            home,
            cwd,
            crate::DiscoveryPlatform::Linux,
            crate::DiscoveryPlatformDirs::default(),
        );
        let automatic = build_automatic_source_backed_registry_with_probes(
            &crate::test_provider_probes(),
            &discovery,
            &ctx_data_root,
        );
        assert!(automatic.issues.iter().all(|issue| !matches!(
            issue,
            SourceBackedAutomaticRegistryIssue::Unavailable { source, .. }
                if source.provider == CaptureProvider::Cline
                    && source.source_format == CLINE_SDK_SOURCE_FORMAT
        )));
        let automatic_registry = automatic.registry;
        let cold = refresh_source_backed_generation(
            &automatic_index,
            &automatic_registry,
            WriterOptions::default(),
        )
        .unwrap();
        let cold_event = cline_sdk_events(&automatic_index, &cold)
            .into_iter()
            .find(|event| event.event_sequence > 0)
            .unwrap();

        write_cline_sdk_messages(&provider_root, &["one", "two"]);
        let appended = refresh_source_backed_generation(
            &automatic_index,
            &automatic_registry,
            WriterOptions::default(),
        )
        .unwrap();
        assert_eq!(
            cline_sdk_events(&automatic_index, &appended)
                .into_iter()
                .find(|event| event.event_sequence > 0)
                .unwrap()
                .event_id,
            cold_event.event_id
        );

        let exact_source =
            crate::provider_source_for_path(CaptureProvider::Cline, provider_root.clone());
        assert_eq!(exact_source.path, provider_root);
        assert_eq!(exact_source.source_format, CLINE_SDK_SOURCE_FORMAT);
        let mut exact_registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route_with_data_root(
            &mut exact_registry,
            exact_source,
            SourceBackedRouteSelection::ExplicitManual,
            &ctx_data_root,
        )
        .unwrap();
        let exact = refresh_source_backed_generation(
            &exact_index,
            &exact_registry,
            WriterOptions::default(),
        )
        .unwrap();
        let exact_event = cline_sdk_events(&exact_index, &exact)
            .into_iter()
            .find(|event| event.event_sequence > 0)
            .unwrap();
        assert_eq!(exact_event.event_id, cold_event.event_id);
        assert_eq!(exact_event.session_id, cold_event.session_id);
    }

    fn leaf_identity(
        index: &Path,
        receipt: &SourceBackedRefreshReceipt,
    ) -> (
        ctx_history_core::StableEntityId,
        ctx_history_core::StableEntityId,
    ) {
        let verified = VerifiedIndex::open_pinned(index).unwrap();
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
        (leaf.event_id, leaf.session_id)
    }

    fn rovodev_events(index: &Path, receipt: &SourceBackedRefreshReceipt) -> Vec<CoreEventRecord> {
        let verified = VerifiedIndex::open_pinned(index).unwrap();
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
                source_format: ctx_history_providers_task_docs::ROVODEV_SOURCE_FORMAT,
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
                route_provenance: Default::default(),
            },
            SourceBackedRouteSelection::Automatic,
            None,
        )
        .unwrap();
        registry
    }

    fn cline_sdk_registry(path: &Path, data_root: &Path) -> SourceBackedProviderRegistry {
        let mut registry = SourceBackedProviderRegistry::new();
        register_cline_sdk_route(
            &mut registry,
            cline_sdk_provider_source(path),
            SourceBackedRouteSelection::Automatic,
            data_root,
            None,
        )
        .unwrap();
        registry
    }

    fn cline_sdk_registry_scoped(
        path: &Path,
        data_root: &Path,
        source_anchor_scope: ctx_history_core::SourceAnchorScope,
    ) -> SourceBackedProviderRegistry {
        let mut registry = SourceBackedProviderRegistry::new();
        let adapter = ClineSdkDocumentTreeAdapter::new_scoped(
            path.to_path_buf(),
            data_root.to_path_buf(),
            source_anchor_scope,
        );
        register_replacement_document_tree_route(
            &mut registry,
            cline_sdk_provider_source(path),
            SourceBackedRouteSelection::Automatic,
            adapter,
        )
        .unwrap();
        registry
    }

    fn cline_sdk_provider_source(path: &Path) -> ProviderSource {
        ProviderSource {
            provider: CaptureProvider::Cline,
            path: path.to_path_buf(),
            exists: true,
            source_format: CLINE_SDK_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
            route_provenance: Default::default(),
        }
    }

    fn write_cline_sdk_index(root: &Path, include_session: bool) {
        let sessions = if include_session {
            serde_json::json!({
                "session-a": {
                    "sessionId": "session-a",
                    "model": "cline-model",
                    "cwd": "/fixture/cwd"
                }
            })
        } else {
            serde_json::json!({})
        };
        fs::write(
            root.join("sessions/sessions.index.json"),
            serde_json::to_vec(&serde_json::json!({"version": 1, "sessions": sessions})).unwrap(),
        )
        .unwrap();
    }

    fn write_cline_sdk_messages(root: &Path, bodies: &[&str]) {
        let messages = bodies
            .iter()
            .enumerate()
            .map(|(index, body)| {
                serde_json::json!({
                    "id": format!("message-{index}"),
                    "role": if index == 0 { "user" } else { "assistant" },
                    "content": [{"type": "text", "text": body}]
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            root.join("sessions/session-a/session-a.messages.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "updated_at": "2026-08-18T12:00:00Z",
                "agent": "lead",
                "sessionId": "session-a",
                "system_prompt": "You are Cline.",
                "messages": messages
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn cline_sdk_events(
        index: &Path,
        receipt: &SourceBackedRefreshReceipt,
    ) -> Vec<CoreEventRecord> {
        let verified = VerifiedIndex::open_pinned(index).unwrap();
        receipt
            .sources
            .iter()
            .flat_map(|source| {
                verified
                    .core_source_event_page(source.observation().source(), None, 64)
                    .unwrap()
                    .items
            })
            .collect()
    }
}
