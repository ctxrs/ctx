#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ctx_history_capture::{
        register_landed_source_backed_route, SourceBackedRefreshExecutor,
    };
    use ctx_history_index::WriterOptions;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn durable_state_path_is_purpose_based() {
        assert_eq!(
            catalog_root(Path::new("ctx-data")),
            Path::new("ctx-data/catalogs/explicit-sources")
        );
    }

    fn custom_source(path: &Path) -> ProviderSource {
        custom_provider_source(path.to_path_buf(), true).unwrap()
    }

    fn write_custom_history(path: &Path, marker: &str) {
        let records = [
            json!({
                "record_type": "manifest",
                "schema_version": "ctx-history-jsonl-v1",
            }),
            json!({
                "record_type": "source",
                "source_id": "catalog-source",
                "provider_key": "catalog-provider",
                "source_format": "catalog-jsonl",
            }),
            json!({
                "record_type": "session",
                "source_id": "catalog-source",
                "session_id": "catalog-session",
                "started_at": "2026-07-28T12:00:00Z",
            }),
            json!({
                "record_type": "event",
                "source_id": "catalog-source",
                "session_id": "catalog-session",
                "event_index": 0,
                "event_type": "message",
                "role": "user",
                "occurred_at": "2026-07-28T12:00:01Z",
                "payload": {"text": marker},
                "preview": marker,
            }),
        ];
        fs::write(
            path,
            records
                .into_iter()
                .map(|record| record.to_string())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
    }

    fn empty_build() -> SourceBackedAutomaticRegistryBuild {
        SourceBackedAutomaticRegistryBuild {
            registry: SourceBackedProviderRegistry::new(),
            issues: Vec::new(),
            discovery_duration: Duration::ZERO,
        }
    }

    fn refresh_catalog(data_root: &Path, index_root: &Path) {
        let mut build = empty_build();
        register_explicit_source_catalog_routes(data_root, index_root, &mut build).unwrap();
        SourceBackedRefreshExecutor::new(build.registry, WriterOptions::default())
            .refresh(
                index_root,
                |_: ctx_history_capture::SourceBackedRefreshProgress| {
                    Ok::<(), SourceBackedRouteError>(())
                },
            )
            .unwrap();
    }

    #[test]
    fn first_add_and_idempotent_upsert_are_metadata_only() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("custom.jsonl");
        write_custom_history(&source_path, "catalog first add");
        let source = custom_source(&source_path);

        let first = upsert_explicit_source(temp.path(), &source).unwrap();
        let second = upsert_explicit_source(temp.path(), &source).unwrap();

        assert!(first.changed);
        assert!(!second.changed);
        assert_eq!(first.authority, second.authority);
        assert_eq!(first.catalog_lineage, second.catalog_lineage);
        assert_eq!(first.authority.revision(), 1);
        assert!(!ctx_history_core::database_path(temp.path().to_path_buf()).exists());
        let bytes = fs::read(catalog_root(temp.path()).join(catalog_revision_filename(1))).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        for forbidden in [
            "preview",
            "payload",
            "credential",
            "event_id",
            "session_id",
            "raw_source_path",
        ] {
            assert!(!text.contains(forbidden), "{forbidden} leaked into catalog");
        }
    }

    #[test]
    fn path_move_preserves_lineage_only_after_prior_path_is_absent() {
        let temp = tempdir().unwrap();
        let first_path = temp.path().join("first.jsonl");
        let second_path = temp.path().join("second.jsonl");
        write_custom_history(&first_path, "first path");
        write_custom_history(&second_path, "second path");
        let first = upsert_explicit_source(temp.path(), &custom_source(&first_path)).unwrap();

        let conflict =
            upsert_explicit_source(temp.path(), &custom_source(&second_path)).unwrap_err();
        assert!(conflict.to_string().contains("prior path still exists"));

        fs::remove_file(&first_path).unwrap();
        let moved = upsert_explicit_source(temp.path(), &custom_source(&second_path)).unwrap();
        assert_eq!(moved.catalog_lineage, first.catalog_lineage);
        assert_eq!(moved.authority.revision(), 2);
    }

    #[test]
    fn disable_publishes_certified_deletion_but_missing_enabled_path_does_not() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("custom.jsonl");
        let index_root = temp.path().join("index");
        write_custom_history(&source_path, "certified catalog deletion");
        let source = custom_source(&source_path);
        upsert_explicit_source(temp.path(), &source).unwrap();
        refresh_catalog(temp.path(), &index_root);
        let first = VerifiedIndex::open(&index_root).unwrap();
        let first_generation = first.generation_id().to_owned();
        assert_eq!(first.manifest().sources.len(), 1);

        fs::remove_file(&source_path).unwrap();
        let mut missing_build = empty_build();
        let missing =
            register_explicit_source_catalog_routes(temp.path(), &index_root, &mut missing_build)
                .unwrap_err();
        assert!(missing
            .to_string()
            .contains("missing paths are not deletion authority"));
        let retained = VerifiedIndex::open(&index_root).unwrap();
        assert_eq!(retained.generation_id(), first_generation);
        assert_eq!(retained.manifest().sources.len(), 1);

        disable_explicit_source(temp.path(), CaptureProvider::Custom, CUSTOM_SOURCE_FORMAT)
            .unwrap();
        refresh_catalog(temp.path(), &index_root);
        let deleted = VerifiedIndex::open(&index_root).unwrap();
        assert!(deleted.manifest().sources.is_empty());
        assert_eq!(deleted.manifest().removals.len(), 1);
        assert!(deleted.manifest().removals[0]
            .deletion()
            .verifies(deleted.manifest().removals[0].inventory()));
    }

    #[test]
    fn custom_history_refreshes_end_to_end_without_work_sqlite() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("custom.jsonl");
        let index_root = temp.path().join("index");
        write_custom_history(&source_path, "source catalog end to end");
        upsert_explicit_source(temp.path(), &custom_source(&source_path)).unwrap();

        refresh_catalog(temp.path(), &index_root);

        let verified = VerifiedIndex::open(&index_root).unwrap();
        assert_eq!(verified.manifest().sources.len(), 1);
        assert_eq!(verified.manifest().indexed_documents, 1);
        assert!(!ctx_history_core::database_path(temp.path().to_path_buf()).exists());
    }

    #[test]
    fn malformed_committed_catalog_fails_closed_but_abandoned_staging_is_ignored() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("custom.jsonl");
        write_custom_history(&source_path, "catalog integrity");
        upsert_explicit_source(temp.path(), &custom_source(&source_path)).unwrap();
        let root = catalog_root(temp.path());
        fs::write(
            root.join(format!(
                "{CATALOG_STAGING_PREFIX}orphan{CATALOG_STAGING_SUFFIX}"
            )),
            b"{malformed",
        )
        .unwrap();
        assert_eq!(load_catalog(temp.path()).unwrap().authority.revision(), 1);

        fs::write(
            root.join(catalog_revision_filename(1)),
            b"{\"schema_version\":1}",
        )
        .unwrap();
        let error = load_catalog(temp.path()).unwrap_err();
        assert!(error.to_string().contains("decode explicit source catalog"));
    }

    #[test]
    fn unsupported_explicit_format_is_rejected_before_catalog_creation() {
        let temp = tempdir().unwrap();
        let rollout = temp.path().join("rollout.jsonl");
        fs::write(
            &rollout,
            r#"{"timestamp":"2026-07-28T12:00:00Z","type":"session_meta","payload":{"id":"supported"}}"#,
        )
        .unwrap();
        let mut source = provider_source_for_path(CaptureProvider::Codex, rollout);
        source.source_format = "unlanded_test_format";
        let error = upsert_explicit_source(temp.path(), &source).unwrap_err();
        assert!(error
            .to_string()
            .contains("has no landed source-backed adapter"));
        assert!(!catalog_root(temp.path()).exists());
    }

    #[test]
    fn automatic_and_explicit_authorities_merge_or_fail_without_double_ingestion() {
        let temp = tempdir().unwrap();
        let custom = temp.path().join("custom.jsonl");
        write_custom_history(&custom, "automatic explicit merge");
        upsert_explicit_source(temp.path(), &custom_source(&custom)).unwrap();

        let automatic_source_path = temp.path().join("automatic.jsonl");
        fs::write(
            &automatic_source_path,
            r#"{"session_id":"one","ts":1,"text":"automatic"}"#,
        )
        .unwrap();
        let automatic_source =
            provider_source_for_path(CaptureProvider::Codex, automatic_source_path);
        let mut automatic_registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route(
            &mut automatic_registry,
            automatic_source,
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();
        let mut build = SourceBackedAutomaticRegistryBuild {
            registry: automatic_registry,
            issues: Vec::new(),
            discovery_duration: Duration::ZERO,
        };
        register_explicit_source_catalog_routes(
            temp.path(),
            &temp.path().join("index"),
            &mut build,
        )
        .unwrap();
        assert_eq!(build.registry.executable_route_count(), 2);

        let native = tempdir().unwrap();
        let prompt = native.path().join("history.jsonl");
        fs::write(&prompt, r#"{"session_id":"one","ts":1,"text":"prompt"}"#).unwrap();
        let source = provider_source_for_path(CaptureProvider::Codex, prompt);
        upsert_explicit_source(native.path(), &source).unwrap();
        let mut duplicate_registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route(
            &mut duplicate_registry,
            source,
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();
        let mut duplicate = SourceBackedAutomaticRegistryBuild {
            registry: duplicate_registry,
            issues: Vec::new(),
            discovery_duration: Duration::ZERO,
        };
        let error = register_explicit_source_catalog_routes(
            native.path(),
            &native.path().join("index"),
            &mut duplicate,
        )
        .unwrap_err();
        assert!(error.to_string().contains("conflicts with automatic"));
        assert_eq!(duplicate.registry.executable_route_count(), 1);
    }
}
