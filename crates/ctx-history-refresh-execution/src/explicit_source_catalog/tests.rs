#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, time::Instant};

    fn custom_source(path: PathBuf) -> ProviderSource {
        custom_provider_source(path, true).unwrap()
    }

    const CODEX_ROLLOUT_FIRST_RECORD: &[u8] = br#"{"timestamp":"2026-08-19T12:00:00Z","type":"session_meta","payload":{"id":"019fc000-0000-7000-8000-000000000001","cwd":"/workspace","source":"cli"}}
"#;

    #[test]
    fn persisted_custom_v1_authority_is_decodable_but_only_replaceable_by_v2() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let old_path = temp.path().join("legacy.jsonl");
        let replacement_path = temp.path().join("replacement.jsonl");
        fs::write(&old_path, b"\n").unwrap();
        fs::write(&replacement_path, b"\n").unwrap();

        let old_lineage = encode_hex(&[0x11; 32]);
        let old = authority_for(
            1,
            &[CatalogEntry {
                provider: CaptureProvider::Custom.as_str().to_owned(),
                source_format: RETIRED_CUSTOM_V1_SOURCE_FORMAT.to_owned(),
                path: old_path,
                catalog_lineage: old_lineage.clone(),
                route_identity: None,
                relocate_from: None,
                enabled: true,
            }],
        )
        .unwrap();
        let decoded = ExplicitSourceCatalogAuthority::from_json(&old.to_json()).unwrap();
        let error = decoded
            .admission_discovery_report(&data_root)
            .expect_err("retired v1 authority must never become executable");
        assert!(
            error.to_string().contains(
                "retired ctx-history-jsonl-v1; rewrite the source as ctx-history-jsonl-v2"
            ),
            "{error:#}"
        );

        let replacement =
            upsert_explicit_source(&data_root, &custom_source(replacement_path)).unwrap();
        let old_route =
            ctx_history_index::SourceRouteIdentity::from_sha256("22".repeat(32)).unwrap();
        let replacement_route =
            ctx_history_index::SourceRouteIdentity::from_sha256("33".repeat(32)).unwrap();
        let retirements = ExplicitSourceCatalogAuthority::replacement_route_retirements(
            Some((
                &decoded,
                &[ExplicitSourceCatalogRouteBinding {
                    catalog_lineage: old_lineage,
                    route_identity: old_route.as_str().to_owned(),
                }],
            )),
            Some((
                &replacement.authority,
                &[ExplicitSourceCatalogRouteBinding {
                    catalog_lineage: replacement.catalog_lineage_hex(),
                    route_identity: replacement_route.as_str().to_owned(),
                }],
            )),
        )
        .unwrap();

        assert_eq!(
            retirements,
            BTreeMap::from([(replacement_route, vec![old_route])])
        );
    }

    #[test]
    fn exact_source_registration_is_an_inline_request_overlay() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"\n").unwrap();

        let request = upsert_explicit_source(&data_root, &custom_source(path.clone())).unwrap();

        assert_eq!(request.path, path);
        assert_eq!(request.authority.route_lineages().len(), 1);
        assert_eq!(
            ExplicitSourceCatalogAuthority::from_json(&request.authority.to_json()).unwrap(),
            request.authority
        );
        assert!(!data_root.join("catalogs/explicit-sources").exists());
    }

    #[test]
    fn request_lineage_is_stable_per_exact_path_and_distinct_across_paths() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let first = temp.path().join("first.jsonl");
        let second = temp.path().join("second.jsonl");
        fs::write(&first, b"\n").unwrap();
        fs::write(&second, b"\n").unwrap();

        let first_request =
            upsert_explicit_source(&data_root, &custom_source(first.clone())).unwrap();
        let repeated = upsert_explicit_source(&data_root, &custom_source(first)).unwrap();
        let second_request = upsert_explicit_source(&data_root, &custom_source(second)).unwrap();

        assert_eq!(first_request.catalog_lineage, repeated.catalog_lineage);
        assert_ne!(
            first_request.catalog_lineage,
            second_request.catalog_lineage
        );
    }

    fn codex_automatic_build(
        temp: &tempfile::TempDir,
        data_root: &Path,
    ) -> (SourceBackedAutomaticRegistryBuild, PathBuf) {
        let sessions = temp.path().join(".codex/sessions");
        fs::create_dir_all(&sessions).unwrap();
        let source = provider_source_for_path(CaptureProvider::Codex, sessions.clone());
        let discovery = ctx_history_capture::DiscoveryContext::new(
            temp.path(),
            temp.path(),
            ctx_history_capture::DiscoveryPlatform::Linux,
            ctx_history_capture::DiscoveryPlatformDirs::default(),
        );
        let build = ctx_history_capture::build_automatic_source_backed_registry_from_report(
            &discovery,
            data_root,
            DiscoveryReport {
                sources: vec![source],
                issues: Vec::new(),
            },
        );
        (build, sessions)
    }

    #[test]
    fn exact_import_selects_the_existing_automatic_route() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let (mut build, sessions) = codex_automatic_build(&temp, &data_root);
        let automatic_route = build
            .registry
            .routes()
            .find_map(|route| route.route_identity.clone())
            .unwrap();
        let request = upsert_explicit_source(
            &data_root,
            &provider_source_for_path(CaptureProvider::Codex, sessions),
        )
        .unwrap();

        let bindings = request
            .authority
            .register_routes_after_discovery_merge(&data_root, None, &mut build)
            .unwrap();

        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].route_identity, automatic_route.as_str());
        assert_eq!(build.registry.routes().count(), 1);
        assert_eq!(
            request
                .authority
                .automatic_route_worksets(&build.registry, &bindings)
                .unwrap(),
            BTreeMap::from([(automatic_route, SourceBackedRefreshWorkset::Exhaustive)])
        );
    }

    #[test]
    fn exact_leaf_import_selects_one_member_of_the_automatic_route() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let (mut build, sessions) = codex_automatic_build(&temp, &data_root);
        let leaf = sessions.join("2026/08/19/rollout.jsonl");
        fs::create_dir_all(leaf.parent().unwrap()).unwrap();
        fs::write(
            &leaf,
            br#"{"timestamp":"2026-08-19T12:00:00Z","type":"session_meta","payload":{"id":"019fc000-0000-7000-8000-000000000001","cwd":"/workspace","source":"cli"}}
"#,
        )
        .unwrap();
        let canonical_leaf = fs::canonicalize(&leaf).unwrap();
        let request = upsert_explicit_source(
            &data_root,
            &provider_source_for_path(CaptureProvider::Codex, leaf.clone()),
        )
        .unwrap();

        let bindings = request
            .authority
            .register_routes_after_discovery_merge(&data_root, None, &mut build)
            .unwrap();
        let route =
            ctx_history_index::SourceRouteIdentity::from_sha256(bindings[0].route_identity.clone())
                .unwrap();

        assert_eq!(build.registry.routes().count(), 1);
        assert_eq!(
            request
                .authority
                .automatic_route_worksets(&build.registry, &bindings)
                .unwrap(),
            BTreeMap::from([(route, SourceBackedRefreshWorkset::members([canonical_leaf]),)])
        );
    }

    #[test]
    fn nested_directory_import_selects_the_automatic_route() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let (mut build, sessions) = codex_automatic_build(&temp, &data_root);
        let nested = sessions.join("2026/08/19");
        fs::create_dir_all(&nested).unwrap();
        let request = upsert_explicit_source(
            &data_root,
            &provider_source_for_path(CaptureProvider::Codex, nested),
        )
        .unwrap();

        let bindings = request
            .authority
            .register_routes_after_discovery_merge(&data_root, None, &mut build)
            .unwrap();
        let route =
            ctx_history_index::SourceRouteIdentity::from_sha256(bindings[0].route_identity.clone())
                .unwrap();

        assert_eq!(build.registry.routes().count(), 1);
        assert_eq!(
            request
                .authority
                .automatic_route_worksets(&build.registry, &bindings)
                .unwrap(),
            BTreeMap::from([(route, SourceBackedRefreshWorkset::Exhaustive)])
        );
    }

    fn configured_root_build(
        provider: CaptureProvider,
        home: &Path,
        source_path: PathBuf,
        data_root: &Path,
    ) -> SourceBackedAutomaticRegistryBuild {
        let discovery = ctx_history_capture::DiscoveryContext::new(
            home.parent().unwrap(),
            home.parent().unwrap(),
            ctx_history_capture::DiscoveryPlatform::Linux,
            ctx_history_capture::DiscoveryPlatformDirs::default(),
        )
        .with_automatic_provider_discovery(false)
        .with_configured_provider_roots(vec![
            ctx_history_capture::ProviderRootDefinition {
                id: "work".to_owned(),
                provider,
                path: home.to_path_buf(),
                group: Some("work".to_owned()),
                kind: None,
            },
        ]);
        ctx_history_capture::build_automatic_source_backed_registry_from_report(
            &discovery,
            data_root,
            DiscoveryReport {
                sources: vec![provider_source_for_path(provider, source_path)],
                issues: Vec::new(),
            },
        )
    }

    #[test]
    fn configured_codex_root_covers_exact_and_descendant_imports() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let home = temp.path().join("codex-work");
        let sessions = home.join("sessions");
        let leaf = sessions.join("2026/08/19/rollout.jsonl");
        fs::create_dir_all(leaf.parent().unwrap()).unwrap();
        fs::write(&leaf, CODEX_ROLLOUT_FIRST_RECORD).unwrap();
        let mut build =
            configured_root_build(CaptureProvider::Codex, &home, sessions.clone(), &data_root);
        let configured_route = build
            .registry
            .routes()
            .find_map(|route| route.route_identity.clone())
            .unwrap();

        for requested in [sessions, leaf] {
            let request = upsert_explicit_source(
                &data_root,
                &provider_source_for_path(CaptureProvider::Codex, requested),
            )
            .unwrap();
            let bindings = request
                .authority
                .register_routes_after_discovery_merge(&data_root, None, &mut build)
                .unwrap();
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].route_identity, configured_route.as_str());
            assert_eq!(build.registry.routes().count(), 1);
        }
    }

    #[test]
    fn configured_claude_root_covers_exact_import() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let home = temp.path().join("claude-work");
        let projects = home.join("projects");
        fs::create_dir_all(&projects).unwrap();
        let mut build =
            configured_root_build(CaptureProvider::Claude, &home, projects.clone(), &data_root);
        let configured_route = build
            .registry
            .routes()
            .find_map(|route| route.route_identity.clone())
            .unwrap();
        let request = upsert_explicit_source(
            &data_root,
            &provider_source_for_path(CaptureProvider::Claude, projects),
        )
        .unwrap();

        let bindings = request
            .authority
            .register_routes_after_discovery_merge(&data_root, None, &mut build)
            .unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].route_identity, configured_route.as_str());
        assert_eq!(build.registry.routes().count(), 1);
    }

    #[test]
    fn grouped_automatic_route_covers_its_secondary_registration_root() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let sessions = temp.path().join(".codex/sessions");
        let archived = temp.path().join(".codex/archived_sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir_all(&archived).unwrap();
        let discovery = ctx_history_capture::DiscoveryContext::new(
            temp.path(),
            temp.path(),
            ctx_history_capture::DiscoveryPlatform::Linux,
            ctx_history_capture::DiscoveryPlatformDirs::default(),
        );
        let mut build = ctx_history_capture::build_automatic_source_backed_registry_from_report(
            &discovery,
            &data_root,
            DiscoveryReport {
                sources: vec![
                    provider_source_for_path(CaptureProvider::Codex, sessions.clone()),
                    provider_source_for_path(CaptureProvider::Codex, archived.clone()),
                ],
                issues: Vec::new(),
            },
        );
        let automatic_route = build
            .registry
            .routes()
            .find_map(|route| route.route_identity.clone())
            .unwrap();
        let request = upsert_explicit_source(
            &data_root,
            &provider_source_for_path(CaptureProvider::Codex, archived),
        )
        .unwrap();

        let bindings = request
            .authority
            .register_routes_after_discovery_merge(&data_root, None, &mut build)
            .unwrap();

        assert_eq!(build.registry.routes().count(), 1);
        assert_eq!(bindings[0].route_identity, automatic_route.as_str());
        assert_eq!(
            request
                .authority
                .automatic_route_worksets(&build.registry, &bindings)
                .unwrap(),
            BTreeMap::from([(automatic_route, SourceBackedRefreshWorkset::Exhaustive)])
        );
    }

    #[test]
    fn admitted_automatic_route_migrates_a_legacy_explicit_binding() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let (build, sessions) = codex_automatic_build(&temp, &data_root);
        let automatic_route = build
            .registry
            .routes()
            .find_map(|route| route.route_identity.clone())
            .unwrap();
        let request = upsert_explicit_source(
            &data_root,
            &provider_source_for_path(CaptureProvider::Codex, sessions),
        )
        .unwrap();
        let previous_route =
            ctx_history_index::SourceRouteIdentity::from_sha256("33".repeat(32)).unwrap();
        let previous_binding = ExplicitSourceCatalogRouteBinding {
            catalog_lineage: request.catalog_lineage_hex(),
            route_identity: previous_route.as_str().to_owned(),
        };

        let canonicalized = request
            .authority
            .canonicalize_published_bindings(
                &[previous_binding],
                &build.registry,
                &BTreeSet::from([automatic_route.clone()]),
            )
            .unwrap();

        assert_eq!(
            canonicalized.bindings[0].route_identity,
            automatic_route.as_str()
        );
        assert_eq!(
            canonicalized.retirements,
            BTreeMap::from([(automatic_route.clone(), vec![previous_route])])
        );
        assert_eq!(
            canonicalized.transitioned_routes,
            BTreeSet::from([automatic_route])
        );
    }

    #[test]
    fn unadmitted_automatic_route_does_not_change_a_published_binding() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let (build, sessions) = codex_automatic_build(&temp, &data_root);
        let request = upsert_explicit_source(
            &data_root,
            &provider_source_for_path(CaptureProvider::Codex, sessions),
        )
        .unwrap();
        let previous_route =
            ctx_history_index::SourceRouteIdentity::from_sha256("44".repeat(32)).unwrap();
        let previous_binding = ExplicitSourceCatalogRouteBinding {
            catalog_lineage: request.catalog_lineage_hex(),
            route_identity: previous_route.as_str().to_owned(),
        };

        let canonicalized = request
            .authority
            .canonicalize_published_bindings(
                std::slice::from_ref(&previous_binding),
                &build.registry,
                &BTreeSet::new(),
            )
            .unwrap();

        assert_eq!(canonicalized.bindings, vec![previous_binding]);
        assert!(canonicalized.retirements.is_empty());
        assert!(canonicalized.transitioned_routes.is_empty());
    }

    #[test]
    fn overlapping_automatic_roots_select_the_most_specific_route() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("sessions");
        let nested = parent.join("2026/08/19");
        let leaf = nested.join("rollout.jsonl");
        fs::create_dir_all(&nested).unwrap();
        fs::write(&leaf, CODEX_ROLLOUT_FIRST_RECORD).unwrap();
        let canonical_leaf = fs::canonicalize(&leaf).unwrap();

        let parent_source = provider_source_for_path(CaptureProvider::Codex, parent);
        let mut nested_source = parent_source.clone();
        nested_source.path = nested;
        let mut requested_source = parent_source.clone();
        requested_source.path = leaf.clone();
        let requested = SourceRouteCoverageKey::from_source(&requested_source).unwrap();
        let parent_route =
            ctx_history_index::SourceRouteIdentity::from_sha256("11".repeat(32)).unwrap();
        let nested_route =
            ctx_history_index::SourceRouteIdentity::from_sha256("22".repeat(32)).unwrap();
        let mut selected = None;

        for candidate in [
            route_coverage_binding(
                &requested,
                RouteCoveragePathKind::File,
                &parent_route,
                [&parent_source],
            )
            .unwrap()
            .unwrap(),
            route_coverage_binding(
                &requested,
                RouteCoveragePathKind::File,
                &nested_route,
                [&nested_source],
            )
            .unwrap()
            .unwrap(),
        ] {
            select_route_coverage_binding(&leaf, &mut selected, candidate).unwrap();
        }

        let selected = selected.unwrap();
        assert_eq!(selected.route_identity, nested_route);
        assert_eq!(
            selected.workset,
            SourceBackedRefreshWorkset::members([canonical_leaf])
        );
    }

    #[test]
    fn unrelated_explicit_source_falls_back_to_its_own_route() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let (mut build, _) = codex_automatic_build(&temp, &data_root);
        let automatic_route = build
            .registry
            .routes()
            .find_map(|route| route.route_identity.clone())
            .unwrap();
        let unrelated = temp.path().join("other/rollout.jsonl");
        fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
        fs::write(&unrelated, CODEX_ROLLOUT_FIRST_RECORD).unwrap();
        let request = upsert_explicit_source(
            &data_root,
            &provider_source_for_path(CaptureProvider::Codex, unrelated.clone()),
        )
        .unwrap();

        let admission = request
            .authority
            .admission_discovery_report_with_automatic_catalog(
                &data_root,
                &build.registry.watch_catalog(),
            )
            .unwrap();
        assert_eq!(admission.sources.len(), 1);
        assert_eq!(admission.sources[0].path, unrelated);

        let bindings = request
            .authority
            .register_routes_after_discovery_merge(&data_root, None, &mut build)
            .unwrap();
        let explicit_route =
            ctx_history_index::SourceRouteIdentity::from_sha256(bindings[0].route_identity.clone())
                .unwrap();
        assert_ne!(explicit_route, automatic_route);
        assert_eq!(build.registry.routes().count(), 2);
        assert!(request
            .authority
            .automatic_route_worksets(&build.registry, &bindings)
            .unwrap()
            .is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn canonicalized_symlink_aliases_match_route_coverage() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real_root = temp.path().join("real/sessions");
        let leaf = real_root.join("2026/08/19/rollout.jsonl");
        fs::create_dir_all(leaf.parent().unwrap()).unwrap();
        fs::write(&leaf, CODEX_ROLLOUT_FIRST_RECORD).unwrap();
        let canonical_leaf = fs::canonicalize(&leaf).unwrap();
        let alias_root = temp.path().join("sessions-alias");
        symlink(&real_root, &alias_root).unwrap();
        let alias_leaf = alias_root.join("2026/08/19/rollout.jsonl");

        let requested = SourceRouteCoverageKey::from_source(&provider_source_for_path(
            CaptureProvider::Codex,
            alias_leaf,
        ))
        .unwrap();
        let registered = provider_source_for_path(CaptureProvider::Codex, real_root);
        let route = ctx_history_index::SourceRouteIdentity::from_sha256("55".repeat(32)).unwrap();
        let binding = route_coverage_binding(
            &requested,
            RouteCoveragePathKind::File,
            &route,
            [&registered],
        )
        .unwrap()
        .unwrap();

        assert_eq!(binding.route_identity, route);
        assert_eq!(
            binding.workset,
            SourceBackedRefreshWorkset::members([canonical_leaf])
        );
    }

    #[test]
    fn exact_admission_uses_the_installed_catalog_without_walking_a_large_tree() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let (build, sessions) = codex_automatic_build(&temp, &data_root);
        let requested_leaf = sessions.join("requested.jsonl");
        fs::write(&requested_leaf, CODEX_ROLLOUT_FIRST_RECORD).unwrap();
        for ordinal in 0..10_000 {
            fs::write(sessions.join(format!("noise-{ordinal:05}.jsonl")), b"{}\n").unwrap();
        }
        let request = upsert_explicit_source(
            &data_root,
            &provider_source_for_path(CaptureProvider::Codex, requested_leaf.clone()),
        )
        .unwrap();
        let catalog = build.registry.watch_catalog();

        let started = Instant::now();
        let report = request
            .authority
            .admission_discovery_report_with_automatic_catalog(&data_root, &catalog)
            .unwrap();
        let elapsed = started.elapsed();

        assert!(report
            .sources
            .iter()
            .all(|source| source.path != requested_leaf));
        assert!(report.sources.len() <= 2);
        assert!(
            elapsed.as_secs_f64() < 2.0,
            "bounded exact admission took {elapsed:?}"
        );
    }

    #[test]
    fn request_overlay_cannot_encode_deletion_authority() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        fs::write(&path, b"\n").unwrap();
        let request =
            upsert_explicit_source(&temp.path().join("data"), &custom_source(path)).unwrap();
        let mut entries = request.authority.entries.clone();
        entries[0].enabled = false;
        let error = sort_and_validate_entries(&mut entries).unwrap_err();
        assert!(format!("{error:#}").contains("cannot authorize deletion"));
    }
}
