#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ctx_history_capture::{register_landed_source_backed_route, SourceBackedRefreshExecutor};
    use ctx_history_index::WriterOptions;
    use serde_json::json;
    use tempfile::{tempdir, TempDir};

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

    fn test_data_root(temp: &TempDir) -> PathBuf {
        temp.path().join("ctx-data")
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

    fn retained_generation(index_root: &Path) -> Option<VerifiedIndex> {
        VerifiedIndex::active_generation_id(index_root)
            .unwrap()
            .is_some()
            .then(|| VerifiedIndex::open(index_root).unwrap())
    }

    fn refresh_catalog(data_root: &Path, index_root: &Path) {
        let retained = retained_generation(index_root);
        let mut build = empty_build();
        register_explicit_source_catalog_routes(data_root, retained.as_ref(), &mut build).unwrap();
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
        let data_root = test_data_root(&temp);
        write_custom_history(&source_path, "catalog first add");
        let source = custom_source(&source_path);

        let first = upsert_explicit_source(&data_root, &source).unwrap();
        let second = upsert_explicit_source(&data_root, &source).unwrap();

        assert!(first.changed);
        assert!(!second.changed);
        assert_eq!(first.authority, second.authority);
        assert_eq!(first.catalog_lineage, second.catalog_lineage);
        assert_eq!(first.authority.revision(), 1);
        assert!(!data_root.join("work.sqlite").exists());
        let bytes = fs::read(catalog_root(&data_root).join(catalog_revision_filename(1))).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let catalog: serde_json::Value = serde_json::from_str(&text).unwrap();
        let forbidden = [
            "preview",
            "payload",
            "credential",
            "event_id",
            "session_id",
            "raw_source_path",
        ];
        assert_no_forbidden_catalog_keys(&catalog, &forbidden);
    }

    fn assert_no_forbidden_catalog_keys(value: &serde_json::Value, forbidden: &[&str]) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, value) in fields {
                    assert!(
                        !forbidden.contains(&key.as_str()),
                        "{key} leaked into catalog"
                    );
                    assert_no_forbidden_catalog_keys(value, forbidden);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    assert_no_forbidden_catalog_keys(value, forbidden);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn path_move_preserves_lineage_only_after_prior_path_is_absent() {
        let temp = tempdir().unwrap();
        let first_path = temp.path().join("first.jsonl");
        let second_path = temp.path().join("second.jsonl");
        let data_root = test_data_root(&temp);
        write_custom_history(&first_path, "first path");
        write_custom_history(&second_path, "second path");
        let first = upsert_explicit_source(&data_root, &custom_source(&first_path)).unwrap();

        let conflict =
            upsert_explicit_source(&data_root, &custom_source(&second_path)).unwrap_err();
        assert!(conflict.to_string().contains("prior path still exists"));

        fs::remove_file(&first_path).unwrap();
        let moved = upsert_explicit_source(&data_root, &custom_source(&second_path)).unwrap();
        assert_eq!(moved.catalog_lineage, first.catalog_lineage);
        assert_eq!(moved.authority.revision(), 2);
    }

    #[test]
    fn missing_enabled_path_does_not_publish_deletion() {
        let temp = tempdir().unwrap();
        let data_root = test_data_root(&temp);
        let source_path = temp.path().join("custom.jsonl");
        let index_root = data_root.join("index");
        write_custom_history(&source_path, "certified catalog deletion");
        let source = custom_source(&source_path);
        upsert_explicit_source(&data_root, &source).unwrap();
        refresh_catalog(&data_root, &index_root);
        let first = VerifiedIndex::open(&index_root).unwrap();
        let first_generation = first.generation_id().to_owned();
        assert_eq!(first.manifest().sources.len(), 1);

        fs::remove_file(&source_path).unwrap();
        let retained = retained_generation(&index_root);
        let mut missing_build = empty_build();
        let missing = register_explicit_source_catalog_routes(
            &data_root,
            retained.as_ref(),
            &mut missing_build,
        )
        .unwrap_err();
        assert!(missing
            .to_string()
            .contains("missing paths are not deletion authority"));
        let retained = VerifiedIndex::open(&index_root).unwrap();
        assert_eq!(retained.generation_id(), first_generation);
        assert_eq!(retained.manifest().sources.len(), 1);
    }

    #[test]
    fn custom_history_refreshes_end_to_end_without_work_sqlite() {
        let temp = tempdir().unwrap();
        let data_root = test_data_root(&temp);
        let source_path = temp.path().join("custom.jsonl");
        let index_root = data_root.join("index");
        write_custom_history(&source_path, "source catalog end to end");
        upsert_explicit_source(&data_root, &custom_source(&source_path)).unwrap();

        refresh_catalog(&data_root, &index_root);

        let verified = VerifiedIndex::open(&index_root).unwrap();
        assert_eq!(verified.manifest().sources.len(), 1);
        assert_eq!(verified.manifest().indexed_documents, 1);
        assert!(!data_root.join("work.sqlite").exists());
    }

    #[test]
    fn malformed_committed_catalog_fails_closed_but_abandoned_staging_is_ignored() {
        let temp = tempdir().unwrap();
        let data_root = test_data_root(&temp);
        let source_path = temp.path().join("custom.jsonl");
        write_custom_history(&source_path, "catalog integrity");
        upsert_explicit_source(&data_root, &custom_source(&source_path)).unwrap();
        let root = catalog_root(&data_root);
        fs::write(
            root.join(format!(
                "{CATALOG_STAGING_PREFIX}orphan{CATALOG_STAGING_SUFFIX}"
            )),
            b"{malformed",
        )
        .unwrap();
        assert_eq!(load_catalog(&data_root).unwrap().authority.revision(), 1);

        fs::write(
            root.join(catalog_revision_filename(1)),
            b"{\"schema_version\":1}",
        )
        .unwrap();
        let error = load_catalog(&data_root).unwrap_err();
        assert!(error.to_string().contains("decode explicit source catalog"));
    }

    #[test]
    fn unsupported_explicit_format_is_rejected_before_catalog_creation() {
        let temp = tempdir().unwrap();
        let data_root = test_data_root(&temp);
        let rollout = temp.path().join("rollout.jsonl");
        fs::write(
            &rollout,
            r#"{"timestamp":"2026-07-28T12:00:00Z","type":"session_meta","payload":{"id":"supported"}}"#,
        )
        .unwrap();
        let mut source = provider_source_for_path(CaptureProvider::Codex, rollout);
        source.source_format = "unlanded_test_format";
        let error = upsert_explicit_source(&data_root, &source).unwrap_err();
        assert!(error
            .to_string()
            .contains("has no landed source-backed adapter"));
        assert!(!catalog_root(&data_root).exists());
    }

    #[test]
    fn exact_automatic_authority_is_shadowed_while_distinct_roots_merge() {
        let temp = tempdir().unwrap();
        let data_root = test_data_root(&temp);
        let custom = temp.path().join("custom.jsonl");
        write_custom_history(&custom, "automatic explicit merge");
        upsert_explicit_source(&data_root, &custom_source(&custom)).unwrap();

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
        register_explicit_source_catalog_routes(&data_root, None, &mut build).unwrap();
        assert_eq!(build.registry.executable_route_count(), 2);

        let native = tempdir().unwrap();
        let native_data_root = test_data_root(&native);
        let prompt = native.path().join("history.jsonl");
        fs::write(&prompt, r#"{"session_id":"one","ts":1,"text":"prompt"}"#).unwrap();
        let source = provider_source_for_path(CaptureProvider::Codex, prompt);
        let explicit = upsert_explicit_source(&native_data_root, &source).unwrap();
        let mut report = DiscoveryReport {
            sources: vec![source.clone()],
            issues: Vec::new(),
        };
        explicit
            .authority
            .remove_shadowed_automatic_routes(&native_data_root, &mut report)
            .unwrap();
        assert!(report.sources.is_empty());
        let mut exact = empty_build();
        explicit
            .authority
            .register_routes(&native_data_root, None, &mut exact)
            .unwrap();
        assert_eq!(exact.registry.executable_route_count(), 1);

        let automatic_path = native.path().join("automatic-history.jsonl");
        fs::write(
            &automatic_path,
            r#"{"session_id":"two","ts":2,"text":"automatic"}"#,
        )
        .unwrap();
        let automatic = provider_source_for_path(CaptureProvider::Codex, automatic_path);
        let mut distinct_registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route(
            &mut distinct_registry,
            automatic,
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();
        let mut distinct = SourceBackedAutomaticRegistryBuild {
            registry: distinct_registry,
            issues: Vec::new(),
            discovery_duration: Duration::ZERO,
        };
        explicit
            .authority
            .register_routes(&native_data_root, None, &mut distinct)
            .unwrap();
        assert_eq!(distinct.registry.executable_route_count(), 2);
    }
}
