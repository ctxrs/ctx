//! Physical settings-upgrade coverage owned by refresh execution.

use super::execution_path::CompleteLexicalSearch;
use super::*;
use crate::execution::execute_capture_owned_refresh;

#[test]
fn automatic_execution_replaces_an_incompatible_settings_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());
    let empty_sessions = temp.path().join("empty-sessions");
    std::fs::create_dir_all(&empty_sessions).unwrap();
    let report = DiscoveryReport {
        sources: vec![
            provider_source_for_path(CaptureProvider::Codex, empty_sessions),
            ProviderSource {
                provider: CaptureProvider::Warp,
                path: temp.path().join("missing-unsupported.sqlite"),
                exists: false,
                source_format: "warp_sqlite",
                source_kind: ProviderSourceKind::DetectionOnly,
                import_support: ProviderImportSupport::Unsupported,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Unsupported,
                unsupported_reason: Some("fixture has no executable source-backed route"),
                route_provenance: Default::default(),
            },
        ],
        issues: vec![
            DiscoveryIssue {
                provider: CaptureProvider::Warp,
                path: None,
                kind: DiscoveryIssueKind::NoDiskHistory,
                reason: "fixture has no provider history",
            },
            DiscoveryIssue {
                provider: CaptureProvider::Warp,
                path: None,
                kind: DiscoveryIssueKind::InsufficientOfficialEvidence,
                reason: "fixture has no officially supported source",
            },
        ],
    };
    let baseline = run_report(&discovery, report.clone(), &data_root, &index_root).unwrap();
    let pointer_path = index_root.join("active-generation.json");
    let pointer_before = std::fs::read(&pointer_path).unwrap();
    let pointer: Value = serde_json::from_slice(&pointer_before).unwrap();
    let old_directory = pointer["active"]["directory"].as_str().unwrap();
    let old_generation_path = index_root.join("index-generations").join(old_directory);
    let meta_path = old_generation_path.join("meta.json");
    let mut meta: Value = serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    meta["index_settings"]["docstore_compression"] =
        Value::String("zstd(compression_level=1)".to_owned());
    meta["index_settings"]["docstore_blocksize"] = Value::from(64 * 1024);
    std::fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
    assert!(matches!(
        VerifiedIndex::open_pinned(&index_root),
        Err(IndexError::IndexSettingsMismatch(_))
    ));

    let rebuilt = run_report(&discovery, report, &data_root, &index_root).unwrap();

    assert_eq!(rebuilt.generation_id, baseline.generation_id);
    assert_ne!(std::fs::read(&pointer_path).unwrap(), pointer_before);
    assert!(!old_generation_path.exists());
    assert_eq!(
        VerifiedIndex::open_pinned(&index_root)
            .unwrap()
            .generation_id(),
        baseline.generation_id
    );
}

#[test]
fn upgrade_rebuild_emits_exact_scan_progress_and_runs_under_explicit_indexing_limits() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = std::fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&fixture);
    let sessions = fixture.join("codex-upgrade/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("rollout.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-19T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "019fc000-0000-7000-8000-000000000192",
                    "timestamp": "2026-08-19T12:00:00Z",
                    "cwd": "/repo/upgrade-eta",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-19T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "upgrade eta and memory bound check"}]
                }
            })
        ),
    )
    .unwrap();
    let report = DiscoveryReport {
        sources: vec![provider_source_for_path(CaptureProvider::Codex, sessions)],
        issues: Vec::new(),
    };
    let published_state = TestPublishedState;

    // Cold initial refresh with explicit constrained memory/worker limits.
    let admitted = source_backed_admitted_discovery_from_report(
        &discovery,
        report.clone(),
        StdDuration::ZERO,
        &data_root,
        AdmittedRefreshCoverage::CompleteCatalog,
        None,
        &published_state,
    )
    .unwrap();
    let cold_exact_seen = std::sync::atomic::AtomicBool::new(false);
    let report_cold = |update: SourceBackedRefreshProgressUpdate| {
        if update.exact_scan_progress.is_some() {
            cold_exact_seen.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    };
    execute_capture_owned_refresh(
        SourceBackedRefreshExecution::new(
            &data_root,
            &index_root,
            "cold-constrained",
            RefreshOperation::Refresh,
            None,
            admitted,
            &discovery,
            &published_state,
            &report_cold,
        )
        .with_indexing_limits(IndexingResourceLimits::from_memory_mb_and_workers(
            Some(64),
            Some(1),
        )),
    )
    .unwrap();
    assert!(cold_exact_seen.load(std::sync::atomic::Ordering::SeqCst));

    // Warm incremental refresh must still suppress exact_scan_progress.
    let warm_admitted = source_backed_admitted_discovery_from_report(
        &discovery,
        report.clone(),
        StdDuration::ZERO,
        &data_root,
        AdmittedRefreshCoverage::CompleteCatalog,
        None,
        &published_state,
    )
    .unwrap();
    let warm_exact_seen = std::sync::atomic::AtomicBool::new(false);
    let report_warm = |update: SourceBackedRefreshProgressUpdate| {
        if update.exact_scan_progress.is_some() {
            warm_exact_seen.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    };
    execute_capture_owned_refresh(
        SourceBackedRefreshExecution::new(
            &data_root,
            &index_root,
            "warm-constrained",
            RefreshOperation::Refresh,
            None,
            warm_admitted,
            &discovery,
            &published_state,
            &report_warm,
        )
        .with_indexing_limits(IndexingResourceLimits::from_memory_mb_and_workers(
            Some(64),
            Some(1),
        )),
    )
    .unwrap();
    assert!(!warm_exact_seen.load(std::sync::atomic::Ordering::SeqCst));

    // Upgrade rebuild from schema-22 fixture must emit exact_scan_progress.
    let fixture_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ctx-history-index/tests/fixtures/schema-22/index");
    std::fs::remove_dir_all(&index_root).unwrap();
    copy_dir_recursive(&fixture_root, &index_root);
    let upgrade_admitted = source_backed_admitted_discovery_from_report(
        &discovery,
        report,
        StdDuration::ZERO,
        &data_root,
        AdmittedRefreshCoverage::CompleteCatalog,
        None,
        &published_state,
    )
    .unwrap();
    let upgrade_exact_seen = std::sync::atomic::AtomicBool::new(false);
    let report_upgrade = |update: SourceBackedRefreshProgressUpdate| {
        if update.exact_scan_progress.is_some() {
            upgrade_exact_seen.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    };
    execute_capture_owned_refresh(
        SourceBackedRefreshExecution::new(
            &data_root,
            &index_root,
            "upgrade-constrained",
            RefreshOperation::Import,
            None,
            upgrade_admitted,
            &discovery,
            &published_state,
            &report_upgrade,
        )
        .with_indexing_limits(IndexingResourceLimits::from_memory_mb_and_workers(
            Some(64),
            Some(1),
        )),
    )
    .unwrap();
    assert!(upgrade_exact_seen.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        VerifiedIndex::open_pinned(&index_root)
            .unwrap()
            .lexical_schema_version(),
        ctx_history_index::LEXICAL_SCHEMA_VERSION
    );
}

fn copy_fixture_dir(from: &Path, to: &Path) {
    copy_dir_recursive(from, to);
}

fn copy_dir_recursive(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&entry.path(), &target);
        } else {
            assert!(entry.path().is_file());
            std::fs::copy(entry.path(), &target).unwrap();
            // Git preserves bytes, not the published artifact's private/readonly seal.
            let mut permissions = std::fs::metadata(&target).unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(0o600);
            }
            if matches!(
                target.extension().and_then(|ext| ext.to_str()),
                Some("fast" | "fieldnorm" | "idx" | "pos" | "store" | "term")
            ) {
                permissions.set_readonly(true);
            }
            std::fs::set_permissions(&target, permissions).unwrap();
        }
    }
}

#[test]
fn schema_22_state_is_rebuild_required_and_explicit_upgrade_ignores_predecessor_retirements() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ctx-history-index/tests/fixtures/schema-22");
    copy_fixture_dir(&fixture.join("index"), &index_root);

    let state = TestPublishedState.open_published_state(&data_root).unwrap();
    assert!(matches!(
        state.generation,
        PublishedSourceBackedGeneration::RebuildRequired {
            predecessor: Some(_)
        }
    ));
    assert!(state.explicit_source_catalog.is_none());
    assert!(state.catalog_route_bindings.is_empty());
    assert!(state.route_controls.is_empty());

    let (home, _, discovery) = discovery_fixture(temp.path());
    let sessions = home.join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::copy(
        fixture.join("source.jsonl"),
        sessions.join("rollout-019fb000-0000-7000-8000-000000000100.jsonl"),
    )
    .unwrap();
    let codex_source = provider_source_for_path(CaptureProvider::Codex, sessions);
    let report = DiscoveryReport {
        sources: vec![codex_source.clone()],
        issues: Vec::new(),
    };
    let explicit = upsert_explicit_source(&data_root, &codex_source).unwrap();

    // Upgrading from schema 22 with an explicit overlay matching an automatic
    // schema-22 route must rebuild cleanly into schema 23 without trying to
    // retire an uncarried route from the unreadable-for-write predecessor.
    let published = refresh_all_provider_sources_route_local(
        &discovery,
        report,
        StdDuration::from_millis(1),
        "schema-22-explicit-upgrade",
        RefreshOperation::Refresh,
        &data_root,
        &index_root,
        Some(&explicit.authority),
        SourceBackedRefreshScope::All,
        &TestPublishedState,
        &mut |_| Ok(()),
    )
    .unwrap();
    assert_eq!(published.current.source_count, 1);

    let upgraded = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(
        upgraded.lexical_schema_version(),
        ctx_history_index::LEXICAL_SCHEMA_VERSION
    );
    assert_eq!(
        upgraded
            .complete_lexical_search("schema22upgrademarker", 4)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn fresh_candidate_ignores_carried_route_retirements_when_no_base_snapshot_exists() {
    struct FixedPublishedState(VerifiedGenerationSnapshot);

    impl PublishedSourceBackedStatePort for FixedPublishedState {
        fn open_published_state(&self, _data_root: &Path) -> Result<PublishedSourceBackedState> {
            Ok(PublishedSourceBackedState {
                generation: PublishedSourceBackedGeneration::Verified(self.0.clone()),
                explicit_source_catalog: None,
                catalog_route_bindings: Vec::new(),
                route_controls: BTreeMap::new(),
            })
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ctx-history-index/tests/fixtures/schema-22");
    copy_fixture_dir(&fixture.join("index"), &index_root);

    let (home, _, discovery) = discovery_fixture(temp.path());
    let sessions = home.join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::copy(
        fixture.join("source.jsonl"),
        sessions.join("rollout-019fb000-0000-7000-8000-000000000100.jsonl"),
    )
    .unwrap();
    let codex_source = provider_source_for_path(CaptureProvider::Codex, sessions);
    let report = DiscoveryReport {
        sources: vec![codex_source.clone()],
        issues: Vec::new(),
    };
    let explicit = upsert_explicit_source(&data_root, &codex_source).unwrap();

    // Even if a caller supplies a stale Verified schema-22 snapshot containing
    // the automatic Codex route while refreshing with an explicit overlay (which
    // populates `retire_after_success` with that automatic route identity),
    // capture execution against the fresh schema-23 candidate (`base_snapshot == None`)
    // must not fail with "retired route ... was not carried when the active route began".
    let stale_schema_22 = VerifiedIndex::open_pinned(&index_root)
        .unwrap()
        .into_generation_snapshot()
        .unwrap();
    let automatic_route = automatic_source_backed_route_identity(&codex_source).unwrap();
    assert!(
        stale_schema_22
            .manifest()
            .source_route(&automatic_route)
            .is_some(),
        "schema-22 fixture must contain the automatic Codex route so explicit overlay populates retire_after_success"
    );
    let fixed_stale = FixedPublishedState(stale_schema_22);
    let published = refresh_all_provider_sources_route_local(
        &discovery,
        report,
        StdDuration::from_millis(1),
        "schema-22-defense-in-depth",
        RefreshOperation::Refresh,
        &data_root,
        &index_root,
        Some(&explicit.authority),
        SourceBackedRefreshScope::All,
        &fixed_stale,
        &mut |_| Ok(()),
    )
    .unwrap();
    assert_eq!(published.current.source_count, 1);
    assert_eq!(
        VerifiedIndex::open_pinned(&index_root)
            .unwrap()
            .lexical_schema_version(),
        ctx_history_index::LEXICAL_SCHEMA_VERSION
    );
}

fn write_rebuild_marker(index_root: &Path) {
    let pointer: Value =
        serde_json::from_slice(&fs::read(index_root.join("active-generation.json")).unwrap())
            .unwrap();
    let generation_id = pointer["active"]["generation_id"].as_str().unwrap();
    let directory = pointer["active"]["directory"].as_str().unwrap();
    let bytes =
        format!(r#"{{"version":1,"generation_id":"{generation_id}","directory":"{directory}"}}"#);
    let marker_path = index_root.join("active-generation-rebuild-required.json");
    fs::write(&marker_path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&marker_path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn rewrite_schema_22_manifest_metadata(
    index_root: &Path,
    generation_state: Option<&ctx_history_index::GenerationStateEnvelope>,
    extra_empty_routes: &[SourceRouteIdentity],
    provider_roots: Option<&[ctx_history_index::AppliedProviderRoot]>,
    detached_released_provider_roots: Option<
        &[ctx_history_index::DetachedReleasedProviderRootAuthority],
    >,
) -> String {
    let pointer_path = index_root.join("active-generation.json");
    let pointer: Value = serde_json::from_slice(&fs::read(&pointer_path).unwrap()).unwrap();
    let old_generation_id = pointer["active"]["generation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let directory = pointer["active"]["directory"].as_str().unwrap().to_owned();
    let old_manifest_path = index_root
        .join("ctx-generations")
        .join(format!("{old_generation_id}.json"));
    let mut manifest_json: Value =
        serde_json::from_slice(&fs::read(&old_manifest_path).unwrap()).unwrap();
    assert_eq!(manifest_json["lexical_schema_version"], 22);
    if !extra_empty_routes.is_empty() {
        let mut routes: Vec<ctx_history_index::SourceRouteSnapshot> =
            serde_json::from_value(manifest_json["source_routes"].clone()).unwrap();
        for route in extra_empty_routes {
            routes.push(
                ctx_history_index::SourceRouteSnapshot::present(route.clone(), Vec::new()).unwrap(),
            );
        }
        routes.sort_by(|left, right| left.route_identity().cmp(right.route_identity()));
        manifest_json["source_routes"] = serde_json::to_value(&routes).unwrap();
    }
    if let Some(envelope) = generation_state {
        manifest_json["generation_state"] = serde_json::to_value(envelope).unwrap();
    }
    if let Some(roots) = provider_roots {
        let definitions = roots
            .iter()
            .map(|root| root.definition().clone())
            .collect::<Vec<_>>();
        let automatic = manifest_json["automatic_provider_discovery"]
            .as_bool()
            .unwrap_or(true);
        let digest =
            ctx_history_index_format::provider_source_config_digest(automatic, &definitions);
        manifest_json["provider_root_config_digest"] = Value::String(digest);
        manifest_json["provider_roots"] = serde_json::to_value(roots).unwrap();
    }
    if let Some(detached) = detached_released_provider_roots {
        manifest_json["detached_released_provider_roots"] = serde_json::to_value(detached).unwrap();
    }
    let manifest: GenerationManifest = serde_json::from_value(manifest_json).unwrap();
    assert_eq!(manifest.lexical_schema_version, 22);
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let new_generation_id = ctx_history_index_format::sha256_hex(&manifest_bytes);
    let new_manifest_path = index_root
        .join("ctx-generations")
        .join(format!("{new_generation_id}.json"));
    fs::write(&new_manifest_path, &manifest_bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&new_manifest_path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    if new_manifest_path != old_manifest_path {
        let _ = fs::remove_file(&old_manifest_path);
    }

    let generation_dir = index_root.join("index-generations").join(&directory);
    let meta_path = generation_dir.join("meta.json");
    let mut meta: Value = serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
    let old_payload = meta["payload"].as_str().unwrap().to_owned();
    let new_payload = old_payload.replace(&old_generation_id, &new_generation_id);
    assert_ne!(
        old_payload, new_payload,
        "schema-22 meta.json payload must reference the manifest generation id"
    );
    meta["payload"] = Value::String(new_payload);
    fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&meta_path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let slot = ctx_history_index_format::GenerationSlot::new(
        new_generation_id.clone(),
        directory.clone(),
        "0".repeat(64),
    )
    .unwrap();
    let slot_index = ctx_history_index_format::open_slot_index(index_root, &slot).unwrap();
    let audit =
        ctx_history_index_format::physical_integrity_audit(&slot_index, &generation_dir, None)
            .unwrap();
    let updated_slot = ctx_history_index_format::GenerationSlot::new(
        new_generation_id.clone(),
        directory,
        audit.digest().to_owned(),
    )
    .unwrap();
    let updated_pointer =
        ctx_history_index_format::ActiveGenerationPointer::new(updated_slot, None).unwrap();
    fs::write(&pointer_path, serde_json::to_vec(&updated_pointer).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&pointer_path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    ctx_history_index_format::clear_manifest_cache_for_root(index_root).unwrap();
    let reopened = VerifiedIndex::open_pinned(index_root).unwrap();
    assert_eq!(reopened.generation_id(), new_generation_id);
    assert_eq!(reopened.lexical_schema_version(), 22);
    new_generation_id
}

fn write_codex_rollout(dir: &Path, filename: &str, session_id: &str, marker: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(filename),
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-08-25T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "timestamp": "2026-08-25T00:00:00Z",
                    "cwd": "/repo/schema-upgrade",
                    "originator": "codex_cli_rs",
                    "cli_version": "1.0.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            }),
            json!({
                "timestamp": "2026-08-25T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": marker}]
                }
            })
        ),
    )
    .unwrap();
}

#[test]
fn schema_22_all_rebuild_retains_manifest_explicit_source_catalog_and_data() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let data_root = root.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ctx-history-index/tests/fixtures/schema-22");
    copy_fixture_dir(&fixture.join("index"), &index_root);

    let (home, _, discovery) = discovery_fixture(&root);
    let sessions = home.join(".codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::copy(
        fixture.join("source.jsonl"),
        sessions.join("rollout-019fb000-0000-7000-8000-000000000100.jsonl"),
    )
    .unwrap();

    // Create a non-automatic explicit `--path` Codex source and store it in the
    // schema-22 predecessor's `applied_explicit_source_catalog`.
    let explicit_dir = root.join("external-explicit-codex-sessions");
    write_codex_rollout(
        &explicit_dir,
        "rollout-019fb000-0000-7000-8000-000000000333.jsonl",
        "019fb000-0000-7000-8000-000000000333",
        "schema22retainedexplicitpathmarker",
    );
    let explicit_source = explicit_source_for_path(
        &data_root,
        &explicit_dir,
        Some(CaptureProvider::Codex),
        false,
    )
    .unwrap();
    let upsert = upsert_explicit_source(&data_root, &explicit_source).unwrap();
    let prior_route = SourceRouteIdentity::from_sha256("11".repeat(32)).unwrap();
    let prior_binding = ExplicitSourceCatalogRouteBinding {
        catalog_lineage: upsert.catalog_lineage_hex(),
        route_identity: prior_route.as_str().to_owned(),
    };
    let schema_22_state = SourceBackedGenerationState::new(
        Some(upsert.authority.clone()),
        vec![prior_binding],
        BTreeMap::new(),
        BTreeMap::new(),
        Vec::new(),
    )
    .unwrap()
    .envelope()
    .unwrap();
    rewrite_schema_22_manifest_metadata(
        &index_root,
        Some(&schema_22_state),
        &[prior_route],
        None,
        None,
    );

    let opened = TestPublishedState.open_published_state(&data_root).unwrap();
    assert!(matches!(
        opened.generation,
        PublishedSourceBackedGeneration::RebuildRequired {
            predecessor: Some(_)
        }
    ));
    assert_eq!(
        opened.explicit_source_catalog,
        Some(upsert.authority.clone())
    );
    assert_eq!(opened.catalog_route_bindings.len(), 1);
    assert!(opened.route_controls.is_empty());

    // Simulate `ctx import --all` (only automatic sources in DiscoveryReport,
    // `explicit_source_catalog = None`): the rebuild must re-register and scan
    // the retained explicit catalog entry instead of dropping it.
    let automatic_codex = provider_source_for_path(CaptureProvider::Codex, sessions);
    let report = DiscoveryReport {
        sources: vec![automatic_codex],
        issues: Vec::new(),
    };
    let published = refresh_all_provider_sources_route_local(
        &discovery,
        report,
        StdDuration::from_millis(1),
        "schema-22-all-retains-explicit",
        RefreshOperation::Import,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &TestPublishedState,
        &mut |_| Ok(()),
    )
    .unwrap();
    assert_eq!(published.current.source_count, 2);
    assert_eq!(
        published.published_explicit_source_catalog,
        Some(upsert.authority.clone())
    );

    let upgraded = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(
        upgraded.lexical_schema_version(),
        ctx_history_index::LEXICAL_SCHEMA_VERSION
    );
    assert_eq!(
        upgraded
            .complete_lexical_search("schema22upgrademarker", 4)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        upgraded
            .complete_lexical_search("schema22retainedexplicitpathmarker", 4)
            .unwrap()
            .len(),
        1
    );
    let decoded = SourceBackedGenerationState::decode_from_verified_index(&upgraded).unwrap();
    assert_eq!(
        decoded.applied_explicit_source_catalog(),
        Some(&upsert.authority)
    );
}

#[test]
fn rebuild_required_upgrade_preserves_released_and_detached_configured_provider_root_authority() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let data_root = root.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let (_, _, discovery) = discovery_fixture(&root);

    // Part 1: Genuine schema-22 predecessor carrying an active Released root
    // (relocated away from the automatic CODEX_HOME) plus a detached Released root.
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ctx-history-index/tests/fixtures/schema-22");
    copy_fixture_dir(&fixture.join("index"), &index_root);

    let original_codex_home = root.join("schema22-codex-original");
    fs::create_dir_all(original_codex_home.join("sessions")).unwrap();
    let relocated_codex_home = root.join("schema22-codex-relocated");
    write_codex_rollout(
        &relocated_codex_home.join("sessions"),
        "rollout-019fb000-0000-7000-8000-000000000411.jsonl",
        "019fb000-0000-7000-8000-000000000411",
        "schema22releasedrootmarker",
    );
    let active_root_def = ctx_history_capture::ProviderRootDefinition {
        id: "work".to_owned(),
        provider: CaptureProvider::Codex,
        path: original_codex_home.clone(),
        group: Some("work-group".to_owned()),
        kind: None,
    };
    let active_released = ctx_history_index::AppliedProviderRoot::with_source_identity(
        active_root_def.clone(),
        ProviderRootSourceIdentity::Released,
        Vec::new(),
    )
    .unwrap();
    let detached_def = ctx_history_capture::ProviderRootDefinition {
        id: "retired-work".to_owned(),
        provider: CaptureProvider::Codex,
        path: root.join("schema22-codex-detached"),
        group: None,
        kind: None,
    };
    let detached_released = ctx_history_index::DetachedReleasedProviderRootAuthority::from_applied(
        &ctx_history_index::AppliedProviderRoot::with_source_identity(
            detached_def,
            ProviderRootSourceIdentity::Released,
            Vec::new(),
        )
        .unwrap(),
    )
    .unwrap()
    .unwrap();
    rewrite_schema_22_manifest_metadata(
        &index_root,
        None,
        &[],
        Some(&[active_released]),
        Some(std::slice::from_ref(&detached_released)),
    );

    let empty_auto_home = root.join("schema22-empty-auto-home");
    fs::create_dir_all(&empty_auto_home).unwrap();
    let relocated_def = ctx_history_capture::ProviderRootDefinition {
        path: relocated_codex_home.clone(),
        ..active_root_def
    };
    let relocated_discovery = discovery
        .clone()
        .with_env("CODEX_HOME", empty_auto_home.as_os_str())
        .with_configured_provider_roots(vec![relocated_def.clone()]);

    // Watch catalog against the schema-22 predecessor must keep Released root
    // authority rather than downgrading the relocated root to NamedV1.
    let watch_catalog = source_backed_watch_catalog(&data_root, &relocated_discovery).unwrap();
    let expected_released_route = automatic_source_backed_route_identity(
        &provider_source_for_path(CaptureProvider::Codex, original_codex_home.join("sessions")),
    )
    .unwrap();
    assert!(
        watch_catalog
            .route_ids()
            .any(|route| *route == expected_released_route),
        "watch catalog downgraded relocated Released schema-22 root away from automatic identity"
    );

    let report = ctx_history_capture::discover_provider_sources_for_provider_with_context(
        &relocated_discovery,
        CaptureProvider::Codex,
    );
    refresh_all_provider_sources_route_local(
        &relocated_discovery,
        report,
        StdDuration::from_millis(1),
        "schema-22-released-root-upgrade",
        RefreshOperation::Import,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &TestPublishedState,
        &mut |_| Ok(()),
    )
    .unwrap();

    let upgraded = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(
        upgraded.lexical_schema_version(),
        ctx_history_index::LEXICAL_SCHEMA_VERSION
    );
    assert_eq!(
        upgraded
            .complete_lexical_search("schema22releasedrootmarker", 4)
            .unwrap()
            .len(),
        1
    );
    let applied_work = upgraded
        .try_manifest()
        .unwrap()
        .provider_root("work")
        .unwrap();
    assert_eq!(
        applied_work.source_identity(),
        ProviderRootSourceIdentity::Released
    );
    assert_eq!(
        applied_work.routes(),
        std::slice::from_ref(&expected_released_route)
    );
    assert!(
        upgraded
            .try_manifest()
            .unwrap()
            .detached_released_provider_roots()
            .iter()
            .any(|authority| authority.id() == detached_released.id()),
        "schema-22 detached Released provider root authority was dropped on rebuild"
    );

    // Part 2: Mark the schema-23 generation for rebuild and verify exact source,
    // route, session, and event identity bytes remain unchanged across a fresh rebuild.
    let hit_before = upgraded
        .complete_lexical_search("schema22releasedrootmarker", 4)
        .unwrap();
    assert_eq!(hit_before.len(), 1);
    let event_before = upgraded
        .core_record_by_id(hit_before[0].event.event_id)
        .unwrap()
        .unwrap();
    drop(upgraded);

    write_rebuild_marker(&index_root);
    let report = ctx_history_capture::discover_provider_sources_for_provider_with_context(
        &relocated_discovery,
        CaptureProvider::Codex,
    );
    refresh_all_provider_sources_route_local(
        &relocated_discovery,
        report,
        StdDuration::from_millis(1),
        "rebuild-marked-released-root",
        RefreshOperation::Import,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::All,
        &TestPublishedState,
        &mut |_| Ok(()),
    )
    .unwrap();

    let rebuilt = VerifiedIndex::open_pinned(&index_root).unwrap();
    let applied_after = rebuilt
        .try_manifest()
        .unwrap()
        .provider_root("work")
        .unwrap();
    assert_eq!(
        applied_after.source_identity(),
        ProviderRootSourceIdentity::Released
    );
    assert_eq!(applied_after.routes(), &[expected_released_route]);
    assert!(rebuilt
        .try_manifest()
        .unwrap()
        .detached_released_provider_roots()
        .iter()
        .any(|authority| authority.id() == detached_released.id()));
    let hit_after = rebuilt
        .complete_lexical_search("schema22releasedrootmarker", 4)
        .unwrap();
    assert_eq!(hit_after.len(), 1);
    let event_after = rebuilt
        .core_record_by_id(hit_after[0].event.event_id)
        .unwrap()
        .unwrap();
    assert_eq!(event_after.source, event_before.source);
    assert_eq!(event_after.session_id, event_before.session_id);
    assert_eq!(event_after.event_id, event_before.event_id);
}

#[test]
fn exact_publication_scope_is_rejected_against_rebuild_required_generation() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let data_root = root.join("data");
    let index_root = source_backed_index_root(&data_root);
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ctx-history-index/tests/fixtures/schema-22");
    copy_fixture_dir(&fixture.join("index"), &index_root);
    let before_generation = VerifiedIndex::open_pinned(&index_root)
        .unwrap()
        .generation_id()
        .to_owned();

    let (home, _, discovery) = discovery_fixture(&root);
    let sessions = home.join(".codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::copy(
        fixture.join("source.jsonl"),
        sessions.join("rollout-019fb000-0000-7000-8000-000000000100.jsonl"),
    )
    .unwrap();
    let codex_source = provider_source_for_path(CaptureProvider::Codex, sessions);
    let codex_route = automatic_source_backed_route_identity(&codex_source).unwrap();
    let report = DiscoveryReport {
        sources: vec![codex_source],
        issues: Vec::new(),
    };

    // Layer 1: refresh-execution rejects Exact scope when predecessor requires rebuild.
    let error = refresh_all_provider_sources_route_local(
        &discovery,
        report.clone(),
        StdDuration::from_millis(1),
        "schema-22-exact-rejected",
        RefreshOperation::Refresh,
        &data_root,
        &index_root,
        None,
        SourceBackedRefreshScope::exact([codex_route.clone()]),
        &TestPublishedState,
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("exact partial source refresh is forbidden"),
        "{error:#}"
    );
    assert_eq!(
        VerifiedIndex::open_pinned(&index_root)
            .unwrap()
            .generation_id(),
        before_generation
    );

    // Layer 2: capture publication executor rejects Exact publication_scope
    // directly when GenerationWriter has no reusable write base on a rebuild.
    let build = build_automatic_source_backed_registry_from_report_with_retained_roots(
        &discovery,
        &data_root,
        report,
        &BTreeMap::new(),
    );
    let (executor, _) = build.into_refresh_executor(WriterOptions::default());
    let capture_error = executor
        .refresh_scope(
            &index_root,
            SourceBackedRefreshScope::exact([codex_route]),
            |_| Ok(()),
        )
        .unwrap_err();
    assert!(
        matches!(
            &capture_error,
            SourceBackedCoordinatorError::InvalidRoute { detail, .. }
                if detail.contains("exact partial publication is forbidden")
        ),
        "{capture_error:?}"
    );
    assert_eq!(
        VerifiedIndex::open_pinned(&index_root)
            .unwrap()
            .generation_id(),
        before_generation
    );
}
