//! Physical settings-upgrade coverage owned by refresh execution.

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
            .manifest()
            .lexical_schema_version,
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
        PublishedSourceBackedGeneration::RebuildRequired
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
        upgraded.manifest().lexical_schema_version,
        ctx_history_index::LEXICAL_SCHEMA_VERSION
    );
    assert_eq!(upgraded.count_term("schema22upgrademarker").unwrap(), 1);
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
        .into_generation_snapshot();
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
            .manifest()
            .lexical_schema_version,
        ctx_history_index::LEXICAL_SCHEMA_VERSION
    );
}
