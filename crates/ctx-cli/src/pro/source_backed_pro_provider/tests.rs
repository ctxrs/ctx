use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

use ctx_history_capture::{
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus, SourceBackedProviderRegistry, SourceBackedRoute, SourceBackedRouteDriver,
    SourceBackedSelectorAuthority,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, EventIdentityInput,
    HydrationFailure, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceObservation,
    SourceRecordLocator,
};
use ctx_history_index::{GenerationWriter, LexicalDocument, WriterOptions};
use tempfile::{tempdir, TempDir};

use super::*;

struct RuntimeFixture {
    index: VerifiedIndex,
    resolver: SourceBackedResolverRegistry,
    manifest: SourceManifest,
    source: CertifiedSource,
    content_by_event: Arc<BTreeMap<[u8; 32], Vec<u8>>>,
    _temp: TempDir,
}

#[test]
fn production_provider_pages_and_resumes_beyond_protocol_limit() {
    let fixture = runtime_fixture(
        (0..=MAX_SOURCE_RECORDS_PER_PAGE)
            .map(|index| {
                (
                    "message",
                    format!("exact-provider-record-{index}").into_bytes(),
                )
            })
            .collect(),
        true,
    );
    let mut provider =
        SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &fixture.manifest).unwrap();

    let first = provider
        .reread_source_page(&fixture.source, None)
        .expect("first bounded source page");
    assert_eq!(first.records.len(), MAX_SOURCE_RECORDS_PER_PAGE);
    assert!(!first.terminal);
    let resume = first
        .next_frontier
        .clone()
        .expect("nonterminal cursor frontier");
    assert_eq!(resume.checkpoint_kind(), SOURCE_EVENT_FRONTIER_KIND_V1);

    let second = provider
        .reread_source_page(&fixture.source, Some(&resume))
        .expect("resumed source page");
    assert_eq!(second.records.len(), 1);
    assert!(second.terminal);
    assert_eq!(second.next_frontier.as_ref(), fixture.source.frontier());
    assert!(first.records.windows(2).all(|records| {
        (
            records[0].metadata.event_sequence,
            records[0].event_id.digest(),
        ) <= (
            records[1].metadata.event_sequence,
            records[1].event_id.digest(),
        )
    }));

    let first_ids = first
        .records
        .iter()
        .map(|record| record.event_id.digest())
        .collect::<BTreeSet<_>>();
    let second_ids = second
        .records
        .iter()
        .map(|record| record.event_id.digest())
        .collect::<BTreeSet<_>>();
    assert!(first_ids.is_disjoint(&second_ids));
    assert_eq!(
        first_ids.len() + second_ids.len(),
        MAX_SOURCE_RECORDS_PER_PAGE + 1
    );
}

#[test]
fn production_provider_rejects_exact_cursor_generation_and_source_mismatch() {
    let fixture = runtime_fixture(vec![("message", b"exact".to_vec())], true);
    let event_id = fixture
        .content_by_event
        .keys()
        .next()
        .copied()
        .and_then(|digest| {
            fixture
                .index
                .source_event_page(
                    fixture.source.observation().source(),
                    None,
                    MAX_SOURCE_EVENT_PAGE_ITEMS,
                )
                .ok()?
                .items
                .into_iter()
                .find(|event| event.event_id.digest() == digest)
                .map(|event| event.event_id)
        })
        .unwrap();
    let mut provider =
        SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &fixture.manifest).unwrap();

    let wrong_generation = encode_cursor_frontier(SourceEventCursor::new(
        "f".repeat(64),
        fixture.source.observation().source().clone(),
        event_id,
    ))
    .unwrap();
    let generation_error = provider
        .reread_source_page(&fixture.source, Some(&wrong_generation))
        .unwrap_err();
    assert!(matches!(
        generation_error.downcast_ref::<SourceBackedProProviderError>(),
        Some(SourceBackedProProviderError::CursorGenerationMismatch { .. })
    ));

    let other_source = source_key([19; 32]);
    let wrong_source = encode_cursor_frontier(SourceEventCursor::new(
        fixture.index.generation_id(),
        other_source,
        event_id,
    ))
    .unwrap();
    let source_error = provider
        .reread_source_page(&fixture.source, Some(&wrong_source))
        .unwrap_err();
    assert!(matches!(
        source_error.downcast_ref::<SourceBackedProProviderError>(),
        Some(SourceBackedProProviderError::CursorSourceMismatch)
    ));
}

#[test]
fn production_provider_uses_exact_hydration_and_maps_event_classes() {
    let fixture = runtime_fixture(
        vec![
            ("message", b"\0exact-message\n".to_vec()),
            ("command", b"exact-command --flag".to_vec()),
            ("result", b"\xffexact-result".to_vec()),
            ("output", b"exact-output".to_vec()),
        ],
        true,
    );
    let mut provider =
        SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &fixture.manifest).unwrap();
    let page = provider
        .reread_source_page(&fixture.source, None)
        .expect("exact hydrated source page");
    assert!(page.terminal);
    assert_eq!(page.records.len(), 4);

    for record in page.records {
        let expected = fixture
            .content_by_event
            .get(&record.event_id.digest())
            .expect("fixture exact bytes");
        assert_eq!(record.relationships.direct_session_id, record.session_id);
        assert_eq!(record.relationships.agent_id, None);
        assert_eq!(record.repository, None);
        assert_eq!(record.facts.len(), 1);
        match (&*record.metadata.event_type, &record.facts[0]) {
            ("message", TransientSourceFact::Message(fact)) => {
                assert_eq!(&fact.content.decode().unwrap(), expected);
            }
            ("command", TransientSourceFact::Command(fact)) => {
                assert_eq!(fact.call_id, None);
                assert_eq!(fact.tool_name, None);
                assert_eq!(fact.working_directory.as_deref(), Some("/fixture/cwd"));
                assert_eq!(&fact.command.decode().unwrap(), expected);
            }
            ("result" | "output", TransientSourceFact::Result(fact)) => {
                assert_eq!(fact.call_id, None);
                assert_eq!(fact.outcome, SourceOutcome::Unknown);
                assert_eq!(fact.exit_code, None);
                assert_eq!(fact.duration_ms, None);
                assert_eq!(&fact.content.decode().unwrap(), expected);
            }
            pair => panic!("unexpected event/fact mapping: {pair:?}"),
        }
    }
}

#[test]
fn production_provider_serializes_generation_bound_repository_authority() {
    let repository = tempdir().unwrap();
    let git = crate::pro::client::git_executable().unwrap();
    for arguments in [
        vec!["init", "-q"],
        vec![
            "remote",
            "add",
            "origin",
            "https://user:secret@example.com/ctxrs/source-authority.git",
        ],
    ] {
        let status = std::process::Command::new(&git)
            .args(["-C", repository.path().to_str().unwrap()])
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let fixture = runtime_fixture_with_cwd(
        vec![("message", b"repository-scoped".to_vec())],
        true,
        repository.path().to_string_lossy().into_owned(),
    );
    let mut provider =
        SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &fixture.manifest).unwrap();

    let page = provider.reread(&fixture.source, None).unwrap();
    let context = page.records[0]
        .repository
        .as_ref()
        .expect("certified repository context");
    assert_eq!(
        context.repository_id,
        "forge:example.com/ctxrs/source-authority"
    );
    assert!(context
        .checkout_id
        .as_deref()
        .unwrap()
        .starts_with("checkout-"));
    assert!(context
        .worktree_id
        .as_deref()
        .unwrap()
        .starts_with("worktree-"));
    assert!(matches!(
        context.object_format.as_deref(),
        Some("sha1" | "sha256")
    ));
    assert_eq!(
        context
            .worktree_root
            .as_ref()
            .map(|locator| PathBuf::from(&locator.absolute_path)),
        Some(repository.path().canonicalize().unwrap())
    );
    let encoded = serde_json::to_string(context).unwrap();
    assert!(!encoded.contains("user"));
    assert!(!encoded.contains("secret"));
}

#[test]
fn production_provider_pins_repository_authority_across_source_pages() {
    let repository = tempdir().unwrap();
    let git = crate::pro::client::git_executable().unwrap();
    for arguments in [
        vec!["init", "-q"],
        vec![
            "remote",
            "add",
            "origin",
            "https://example.com/ctxrs/original.git",
        ],
    ] {
        let status = std::process::Command::new(&git)
            .args(["-C", repository.path().to_str().unwrap()])
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let fixture = runtime_fixture_with_cwd(
        (0..=MAX_SOURCE_RECORDS_PER_PAGE)
            .map(|index| ("message", format!("record-{index}").into_bytes()))
            .collect(),
        true,
        repository.path().to_string_lossy().into_owned(),
    );
    let mut provider =
        SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &fixture.manifest).unwrap();
    let first = provider.reread(&fixture.source, None).unwrap();
    let first_context = first.records[0].repository.clone().unwrap();
    assert_eq!(
        first_context.repository_id,
        "forge:example.com/ctxrs/original"
    );

    let status = std::process::Command::new(&git)
        .args(["-C", repository.path().to_str().unwrap()])
        .args([
            "remote",
            "set-url",
            "origin",
            "https://example.com/ctxrs/replaced.git",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let second = provider
        .reread(&fixture.source, first.next_frontier.as_ref())
        .unwrap();

    assert_eq!(second.records[0].repository.as_ref(), Some(&first_context));
}

#[test]
fn production_provider_terminal_frontier_preserves_none() {
    let fixture = runtime_fixture(Vec::new(), false);
    let mut provider =
        SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &fixture.manifest).unwrap();

    let page = provider
        .reread_source_page(&fixture.source, None)
        .expect("empty terminal source page");

    assert!(page.terminal);
    assert!(page.records.is_empty());
    assert_eq!(fixture.source.frontier(), None);
    assert_eq!(page.next_frontier, None);
}

#[test]
fn production_provider_rejects_oversized_exact_content_without_truncation() {
    let fixture = runtime_fixture(
        vec![("message", vec![b'x'; MAX_SOURCE_CONTENT_BYTES + 1])],
        true,
    );
    let mut provider =
        SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &fixture.manifest).unwrap();

    let error = provider
        .reread_source_page(&fixture.source, None)
        .expect_err("oversized exact content must fail");

    assert!(matches!(
        error.downcast_ref::<SourceBackedProProviderError>(),
        Some(SourceBackedProProviderError::ContentBoundExceeded {
            actual,
            maximum: MAX_SOURCE_CONTENT_BYTES,
            ..
        }) if *actual == MAX_SOURCE_CONTENT_BYTES + 1
    ));
}

#[test]
fn production_provider_requires_exact_pinned_manifest_authority() {
    let fixture = runtime_fixture(vec![("message", b"exact".to_vec())], true);
    let mut wrong_generation = fixture.manifest.clone();
    wrong_generation.core_generation_id = "e".repeat(64);
    assert!(matches!(
        SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &wrong_generation),
        Err(SourceBackedProProviderError::GenerationMismatch { .. })
    ));

    let missing_source =
        SourceManifest::new(fixture.index.generation_id(), Vec::new(), Vec::new()).unwrap();
    assert!(matches!(
        SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &missing_source),
        Err(SourceBackedProProviderError::ManifestSourcesMismatch)
    ));
}

#[test]
fn production_runtime_has_no_store_body_or_preview_dependency() {
    let runtime = include_str!("../source_backed_pro_provider.rs");

    assert!(!runtime.contains(&["ctx_history_", "store"].concat()));
    assert!(!runtime.contains(&["Store", "::"].concat()));
    assert!(!runtime.contains(&[".", "preview"].concat()));
    assert!(!runtime.contains(&["body_", "store"].concat()));
}

fn runtime_fixture(records: Vec<(&'static str, Vec<u8>)>, with_frontier: bool) -> RuntimeFixture {
    runtime_fixture_with_cwd(records, with_frontier, "/fixture/cwd".to_owned())
}

fn runtime_fixture_with_cwd(
    records: Vec<(&'static str, Vec<u8>)>,
    with_frontier: bool,
    cwd: String,
) -> RuntimeFixture {
    let temp = tempdir().unwrap();
    let source = source_key([7; 32]);
    let session_key = NativeSessionKey::native_id("fixture-session", TypedKey::U64(1)).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let mut documents = Vec::with_capacity(records.len());
    let mut content_by_event = BTreeMap::new();
    let mut certified_hasher = Sha256::new();
    let mut certified_bytes = 0_u64;
    for (index, (event_type, exact_bytes)) in records.into_iter().enumerate() {
        let sequence = u64::try_from(index).unwrap().saturating_add(1);
        let native_item_key =
            NativeItemKey::native_id("fixture-event", TypedKey::U64(sequence)).unwrap();
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: event_type,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        let record_digest: [u8; 32] = Sha256::digest(&exact_bytes).into();
        let byte_length = u64::try_from(exact_bytes.len()).unwrap();
        let locator = SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: certified_bytes,
                byte_length,
                physical_ordinal: sequence,
                native_session_key: Some(TypedKey::U64(1)),
                native_event_key: Some(TypedKey::U64(sequence)),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            None,
            record_digest,
        )
        .unwrap();
        certified_hasher.update(&exact_bytes);
        certified_bytes = certified_bytes.saturating_add(byte_length);
        content_by_event.insert(event_id.digest(), exact_bytes);
        documents.push(LexicalDocument {
            event_id,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            source: source.clone(),
            locator,
            provider_session_id: Some("fixture-provider-session".to_owned()),
            branch: Some("fixture-branch".to_owned()),
            source_path: Some("/fixture/source.jsonl".to_owned()),
            agent_type: "primary".to_owned(),
            is_primary: true,
            event_sequence: sequence,
            occurred_at_unix_ms: Some(1_700_000_000_000 + i64::try_from(index).unwrap()),
            event_type: event_type.to_owned(),
            role: Some("assistant".to_owned()),
            body: format!("preview-only-{sequence}"),
            workspace: Some("/fixture/workspace".to_owned()),
            cwd: Some(cwd.clone()),
            touched_files: vec![format!("src/{sequence}.rs")],
        });
    }
    let content_digest: [u8; 32] = certified_hasher.finalize().into();
    let observation =
        SourceObservation::new(source.clone(), "fixture-revision-v1", vec![1]).unwrap();
    let frontier = with_frontier.then(|| {
        SourceFrontier::new(
            "fixture-terminal-v1",
            TypedKey::U64(u64::try_from(documents.len()).unwrap()),
            certified_bytes,
            content_digest,
        )
        .unwrap()
    });
    let certificate = CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "fixture-parser-v1",
        content_digest,
        ScannedSourceCounts {
            complete_records: u64::try_from(documents.len()).unwrap(),
            retained_records: u64::try_from(documents.len()).unwrap(),
            indexed_documents: u64::try_from(documents.len()).unwrap(),
            certified_bytes,
            ..ScannedSourceCounts::default()
        },
        frontier,
    )
    .unwrap();
    let index_root = temp.path().join("index");
    let mut writer = GenerationWriter::open(
        &index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 16 * 1024 * 1024,
        },
    )
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for document in documents {
        writer.add_document(document).unwrap();
    }
    writer.certify_source(certificate.clone()).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let manifest =
        SourceManifest::new(index.generation_id(), vec![certificate.clone()], Vec::new()).unwrap();

    let content_by_event = Arc::new(content_by_event);
    let hydration_records = Arc::clone(&content_by_event);
    let owned_source = source.clone();
    let driver = SourceBackedRouteDriver::new(
        |_sink| Ok(()),
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        |_target| true,
        move |request| {
            hydration_records
                .get(&request.event_id().digest())
                .cloned()
                .map(|provider_bytes| HydratedProviderRecord {
                    event_id: request.event_id(),
                    provider_bytes,
                })
                .ok_or_else(|| HydrationFailure {
                    kind: HydrationFailureKind::MissingRecord,
                    detail: "fixture event is absent".to_owned(),
                })
        },
    );
    let route = SourceBackedRoute::automatic(
        ProviderSource {
            provider: CaptureProvider::Codex,
            path: PathBuf::from("/fixture/codex-sessions"),
            exists: true,
            source_format: "codex_session_jsonl_tree",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::Native,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )
    .unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    let resolver = registry.resolver_registry();

    RuntimeFixture {
        index,
        resolver,
        manifest,
        source: certificate,
        content_by_event,
        _temp: temp,
    }
}

fn source_key(lineage: [u8; 32]) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage(lineage),
    )
    .unwrap()
}
