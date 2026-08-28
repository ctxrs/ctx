use super::*;
use std::fs;

use ctx_history_capture::{
    provider_source_for_path, DiscoveryPlatform, DiscoveryPlatformDirs, ProviderRouteRole,
    ProviderSourceRouteProvenance, SourceBackedSelectorAuthority,
};
use ctx_history_capture_model::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderRootKind, ProviderSource,
    ProviderSourceKind,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey,
    SourceObservation, TypedKey,
};

struct TestPublishedState;

impl PublishedSourceBackedStatePort for TestPublishedState {
    fn open_published_state(&self, data_root: &Path) -> Result<PublishedSourceBackedState> {
        let index_root = source_backed_index_root(data_root);
        if !index_root.is_dir() {
            return Ok(PublishedSourceBackedState {
                verified_index: None,
                explicit_source_catalog: None,
                catalog_route_bindings: Vec::new(),
                route_controls: BTreeMap::new(),
            });
        }
        let verified_index = match VerifiedIndex::open_pinned(&index_root) {
            Ok(index) => Some(index),
            Err(IndexError::MissingActiveGenerationPointer) => None,
            Err(error)
                if ctx_history_index::generation_incompatibility_requires_rebuild(&error) =>
            {
                None
            }
            Err(error) => return Err(error.into()),
        };
        let (explicit_source_catalog, catalog_route_bindings, route_controls) = if let Some(index) =
            verified_index
                .as_ref()
                .filter(|index| index.publication_metadata().is_some())
        {
            let metadata = SourceBackedPublicationMetadata::decode(index)?;
            let receipt = published_refresh_receipt_for_index(&metadata.response_value(), index)?;
            (
                receipt.published_explicit_source_catalog,
                receipt.catalog_route_bindings,
                metadata.route_controls,
            )
        } else {
            (None, Vec::new(), BTreeMap::new())
        };
        Ok(PublishedSourceBackedState {
            verified_index,
            explicit_source_catalog,
            catalog_route_bindings,
            route_controls,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn refresh_all_provider_sources(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    index_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    scope: SourceBackedRefreshScope,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedDetailedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    refresh_all_provider_sources_route_local(
        discovery,
        report,
        discovery_duration,
        "test-refresh",
        RefreshOperation::Refresh,
        data_root,
        index_root,
        explicit_source_catalog,
        scope,
        &TestPublishedState,
        report_progress,
    )
}

fn discovery_fixture(root: &Path) -> (PathBuf, PathBuf, DiscoveryContext) {
    let home = root.join("home");
    let cwd = root.join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    (home, cwd, discovery)
}

#[test]
fn configured_provider_root_identity_matching_rejects_duplicate_stable_ids() {
    let temp = tempfile::tempdir().unwrap();
    let (_, _, discovery) = discovery_fixture(temp.path());
    let roots = [CaptureProvider::Claude, CaptureProvider::Hermes]
        .map(|provider| ctx_history_capture::ProviderRootDefinition {
            id: "duplicate".to_owned(),
            provider,
            path: temp.path().join(provider.as_str()),
            group: None,
            kind: None,
        })
        .to_vec();
    let discovery = discovery.with_configured_provider_roots(roots);

    let error = configured_retained_provider_roots(&discovery, None).unwrap_err();
    assert!(error.to_string().contains("not unique"), "{error:#}");
}

#[test]
fn configured_root_transition_partitions_removals_from_incompatible_replacements() {
    use ctx_history_capture::{ProviderRootDefinition, ProviderRootKind};
    use ctx_history_index::{AppliedProviderRoot, AppliedProviderRootSourceMembership};

    let route = |byte: &str| SourceRouteIdentity::from_sha256(byte.repeat(64)).unwrap();
    let replaced_provider_route = route("1");
    let replaced_kind_route = route("2");
    let compatible_route = route("3");
    let exact_shared_route = route("4");
    let removed_route = route("5");
    let applied = |definition, route| {
        AppliedProviderRoot::with_source_identity(
            definition,
            ProviderRootSourceIdentity::NamedV1,
            vec![route],
        )
        .unwrap()
    };
    let retained = vec![
        applied(
            ProviderRootDefinition {
                id: "provider-replacement".to_owned(),
                provider: CaptureProvider::Claude,
                path: "/old/claude".into(),
                group: None,
                kind: None,
            },
            replaced_provider_route.clone(),
        ),
        applied(
            ProviderRootDefinition {
                id: "kind-replacement".to_owned(),
                provider: CaptureProvider::OpenHands,
                path: "/old/openhands".into(),
                group: None,
                kind: Some(ProviderRootKind::OpenHandsCurrentConversations),
            },
            replaced_kind_route.clone(),
        ),
        applied(
            ProviderRootDefinition {
                id: "compatible-move".to_owned(),
                provider: CaptureProvider::Claude,
                path: "/old/path".into(),
                group: Some("old-group".to_owned()),
                kind: None,
            },
            compatible_route,
        ),
        applied(
            ProviderRootDefinition {
                id: "removed".to_owned(),
                provider: CaptureProvider::Codex,
                path: "/old/codex".into(),
                group: None,
                kind: None,
            },
            removed_route.clone(),
        ),
        AppliedProviderRoot::with_source_identity(
            ProviderRootDefinition {
                id: "removed-exact-subset".to_owned(),
                provider: CaptureProvider::Crush,
                path: "/old/crush.db".into(),
                group: None,
                kind: None,
            },
            ProviderRootSourceIdentity::Released,
            vec![exact_shared_route.clone()],
        )
        .unwrap()
        .with_exact_source_memberships(vec![AppliedProviderRootSourceMembership::exact(
            exact_shared_route,
            vec!["ab".repeat(32)],
        )
        .unwrap()])
        .unwrap(),
    ];
    let desired = vec![
        ProviderRootDefinition {
            id: "provider-replacement".to_owned(),
            provider: CaptureProvider::Codex,
            path: "/new/codex".into(),
            group: None,
            kind: None,
        },
        ProviderRootDefinition {
            id: "kind-replacement".to_owned(),
            provider: CaptureProvider::OpenHands,
            path: "/new/openhands".into(),
            group: None,
            kind: Some(ProviderRootKind::OpenHandsLegacyPersistence),
        },
        ProviderRootDefinition {
            id: "compatible-move".to_owned(),
            provider: CaptureProvider::Claude,
            path: "/new/path".into(),
            group: Some("new-group".to_owned()),
            kind: None,
        },
    ];

    assert_eq!(
        incompatible_configured_provider_root_routes(&retained, &desired),
        BTreeSet::from([
            replaced_provider_route,
            replaced_kind_route,
            removed_route.clone(),
        ])
    );
    assert_eq!(
        removed_configured_provider_root_routes(&retained, &desired),
        BTreeSet::from([removed_route])
    );
}

fn configured_provider_source_for_path(
    provider: CaptureProvider,
    path: PathBuf,
    root_id: &str,
    root_path: PathBuf,
    route_role: &'static str,
) -> ProviderSource {
    let mut source = provider_source_for_path(provider, path);
    source.route_provenance = ProviderSourceRouteProvenance::ConfiguredRoot {
        root_id: root_id.to_owned(),
        root_path,
        route_role: ProviderRouteRole::from_static(route_role),
        automatic_route_role: None,
    };
    source
}

fn run_report(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    data_root: &Path,
    index_root: &Path,
) -> Result<SourceBackedRefreshPublication> {
    let mut progress =
        |_: CaptureSourceBackedDetailedRefreshProgress| Ok::<(), SourceBackedRouteError>(());
    refresh_all_provider_sources(
        discovery,
        report,
        StdDuration::ZERO,
        data_root,
        index_root,
        None,
        SourceBackedRefreshScope::All,
        &mut progress,
    )
}

fn publication_pin_source_with_anchor(anchor: u8) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::CatalogLineage([anchor; 32]),
    )
    .unwrap()
}

fn publication_pin_record(source: &SourceKey) -> CoreRecord {
    let native_session = TypedKey::utf8("publication-pin-session").unwrap();
    let session_key = NativeSessionKey::native_id("session", native_session).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item =
        NativeItemKey::native_id("message", TypedKey::utf8("publication-pin-event").unwrap())
            .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        0,
        "message",
        "publication-pin-test-v1",
        "exact publication pin fixture",
    )
    .unwrap();
    record.provider_session_id = Some("publication-pin-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(0));
    record.role = Some("user".to_owned());
    record.validate_contract().unwrap();
    record
}

fn publication_pin_certificate(source: &SourceKey) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "publication-pin-test-v1",
        [0x92; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 128,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn publish_pin_source(index_root: &Path, source: SourceKey) -> String {
    let mut writer = GenerationWriter::open(index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(publication_pin_record(&source))
        .unwrap();
    writer
        .certify_source(publication_pin_certificate(&source))
        .unwrap();
    writer.commit(|_| true).unwrap().generation_id
}

#[test]
fn watch_catalog_reconstruction_uses_bounded_open_and_no_deep_logical_pass() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    publish_pin_source(&index_root, publication_pin_source_with_anchor(0x99));
    let (_, _, discovery) = discovery_fixture(&temp.path().join("discovery"));

    // This unit target intentionally depends on the production index library,
    // where the exhaustive `VerifiedIndex::open` oracle does not exist. The
    // watch-catalog path therefore cannot compile if it becomes reachable from
    // a deep logical open again.
    source_backed_watch_catalog(&data_root, &discovery).unwrap();
}

fn test_publication(generation_id: impl Into<String>) -> SourceBackedRefreshPublication {
    SourceBackedRefreshPublication {
        route_results: Vec::new(),
        zero_source_authority: Vec::new(),
        catalog_route_bindings: Vec::new(),
        verified_index: None,
        generation_id: generation_id.into(),
        published_explicit_source_catalog: None,
        unsupported_routes: 0,
        certified_source_count: 1,
        certified_source_bytes: 128,
        current: SourceBackedRefreshCurrent {
            source_count: 1,
            indexed_documents: 1,
            complete_records: 1,
            retained_records: 1,
            certified_source_bytes: 128,
            ..SourceBackedRefreshCurrent::default()
        },
        timings: SourceBackedRefreshTimings::default(),
    }
}

#[test]
fn query_readiness_accepts_nonempty_generation_without_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let generation_id = publish_pin_source(temp.path(), publication_pin_source_with_anchor(0x95));
    let verified = VerifiedIndex::open_pinned(temp.path()).unwrap();

    assert_eq!(verified.generation_id(), generation_id);
    assert!(verified.publication_metadata().is_none());
    assert_eq!(
        verify_generation_query_readiness(&verified).unwrap(),
        GenerationQueryReadiness::Ready
    );
}

#[test]
fn query_readiness_decodes_metadata_before_certifying_generation() {
    let temp = tempfile::tempdir().unwrap();
    let generation_id = publish_pin_source(temp.path(), publication_pin_source_with_anchor(0x96));
    let publication = test_publication(generation_id.clone());
    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
        None,
        generation_id.clone(),
        &publication,
    )
    .unwrap();
    let encoded = SourceBackedPublicationMetadata {
        version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
        request_id: "query-readiness-metadata".to_owned(),
        operation: RefreshOperation::Refresh,
        refresh_scope: SourceBackedRefreshScope::All,
        receipt: receipt.to_json(),
        route_observations: BTreeMap::new(),
        route_controls: BTreeMap::new(),
    }
    .encode()
    .unwrap();
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let verified = writer
        .republish_current_publication_metadata(&generation_id, encoded)
        .unwrap();

    assert!(verified.publication_metadata().is_some());
    assert_eq!(
        verify_generation_query_readiness(&verified).unwrap(),
        GenerationQueryReadiness::Ready
    );

    let mut invalid_metadata: Value =
        serde_json::from_slice(verified.publication_metadata().unwrap()).unwrap();
    invalid_metadata["version"] = json!(99);
    drop(verified);
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let invalid = writer
        .republish_current_publication_metadata(
            &generation_id,
            serde_json::to_vec(&invalid_metadata).unwrap(),
        )
        .unwrap();
    assert!(format!(
        "{:#}",
        verify_generation_query_readiness(&invalid).unwrap_err()
    )
    .contains("unsupported Core source-refresh publication metadata version"));
}

#[test]
fn publication_metadata_versions_preserve_receipt_shape_and_gate_the_v4_ledger() {
    let temp = tempfile::tempdir().unwrap();
    let generation_id = publish_pin_source(temp.path(), publication_pin_source_with_anchor(0x98));
    let publication = test_publication(generation_id.clone());
    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
        None,
        generation_id.clone(),
        &publication,
    )
    .unwrap();
    let route_identity = "11".repeat(32);
    let source_identity = "22".repeat(32);
    let ledger = json!({
        (&route_identity): [
            "s",
            false,
            0,
            0,
            [],
            1,
            [[
                source_identity,
                "qwen_code",
                "/tmp/qwen.jsonl",
                3,
                "message",
                "m",
                "invalid shape"
            ]]
        ]
    });
    let metadata = SourceBackedPublicationMetadata {
        version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
        request_id: "metadata-version-ledger".to_owned(),
        operation: RefreshOperation::Refresh,
        refresh_scope: SourceBackedRefreshScope::All,
        receipt: receipt.to_json(),
        route_observations: BTreeMap::new(),
        route_controls: BTreeMap::new(),
    };
    let current = metadata
        .encode_with_committed_rejection_diagnostics(&ledger)
        .unwrap();
    let republish = |bytes: Vec<u8>| {
        let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        writer
            .republish_current_publication_metadata(&generation_id, bytes)
            .unwrap()
    };

    let verified = republish(current.clone());
    let (decoded, decoded_ledger) =
        SourceBackedPublicationMetadata::decode_with_committed_rejection_diagnostics(&verified)
            .unwrap();
    assert_eq!(decoded.version, SOURCE_REFRESH_PUBLICATION_METADATA_VERSION);
    assert_eq!(decoded_ledger.unwrap().len(), 1);
    assert_eq!(decoded.response_value()["receipt"], receipt.to_json());
    assert!(decoded.response_value()["receipt"]
        .get(crate::metadata::COMMITTED_REJECTION_DIAGNOSTICS_FIELD)
        .is_none());
    assert_eq!(
        published_refresh_receipt_for_index(&decoded.response_value(), &verified).unwrap(),
        receipt
    );
    drop(verified);

    for version in [1_u64, 2, 3] {
        let mut legacy: Value = serde_json::from_slice(&current).unwrap();
        legacy["version"] = json!(version);
        legacy
            .as_object_mut()
            .unwrap()
            .remove(crate::metadata::COMMITTED_REJECTION_DIAGNOSTICS_FIELD);
        if version < 3 {
            legacy.as_object_mut().unwrap().remove("route_controls");
        }
        if version == 1 {
            legacy["receipt"]
                .as_object_mut()
                .unwrap()
                .remove("zero_source_authority");
        }
        let verified = republish(serde_json::to_vec(&legacy).unwrap());
        let (decoded, decoded_ledger) =
            SourceBackedPublicationMetadata::decode_with_committed_rejection_diagnostics(&verified)
                .unwrap();
        assert_eq!(decoded.version, version);
        assert!(decoded_ledger.is_none());
        drop(verified);
    }

    for mutation in ["missing", "unknown"] {
        let mut invalid: Value = serde_json::from_slice(&current).unwrap();
        if mutation == "missing" {
            invalid
                .as_object_mut()
                .unwrap()
                .remove(crate::metadata::COMMITTED_REJECTION_DIAGNOSTICS_FIELD);
        } else {
            invalid["unexpected"] = json!(true);
        }
        let verified = republish(serde_json::to_vec(&invalid).unwrap());
        assert!(format!(
            "{:#}",
            SourceBackedPublicationMetadata::decode(&verified).unwrap_err()
        )
        .contains("unknown or missing fields"));
        drop(verified);
    }
}

#[test]
fn persisted_metadata_rejects_malformed_route_identity() {
    let temp = tempfile::tempdir().unwrap();
    let generation_id = publish_pin_source(temp.path(), publication_pin_source_with_anchor(0x97));
    let publication = test_publication(generation_id.clone());
    let receipt = SourceBackedRefreshReceipt::from_verified_publication(
        None,
        generation_id.clone(),
        &publication,
    )
    .unwrap();
    let encoded = SourceBackedPublicationMetadata {
        version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
        request_id: "malformed-route-metadata".to_owned(),
        operation: RefreshOperation::Refresh,
        refresh_scope: SourceBackedRefreshScope::All,
        receipt: receipt.to_json(),
        route_observations: BTreeMap::new(),
        route_controls: BTreeMap::new(),
    }
    .encode()
    .unwrap();
    let mut persisted: Value = serde_json::from_slice(&encoded).unwrap();
    persisted["receipt"]["route_results"] = json!({"AB".repeat(32): ["s", false]});

    let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    let verified = writer
        .republish_current_publication_metadata(
            &generation_id,
            serde_json::to_vec(&persisted).unwrap(),
        )
        .unwrap();
    let error = SourceBackedPublicationMetadata::decode(&verified).unwrap_err();

    assert!(format!("{error:#}").contains("route identity is invalid"));
}

#[path = "tests/execution_path.rs"]
mod execution_path;
#[path = "tests/receipt.rs"]
mod receipt_tests;
#[path = "tests/registry_policy.rs"]
mod registry_policy;
#[path = "tests/settings_upgrade.rs"]
mod settings_upgrade;
