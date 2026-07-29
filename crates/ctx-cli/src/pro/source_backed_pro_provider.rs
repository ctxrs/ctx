use std::{collections::BTreeMap, path::Path};

use anyhow::Result;
use ctx_history_capture::SourceBackedResolverRegistry;
use ctx_history_core::{
    BatchHydrationRequest, ContentSourceResolver, EventHydrationRequest, HydratedProviderRecord,
    HydrationFailureKind, SourceFrontier, SourceKey, StableEntityId, TypedKey,
};
#[cfg(test)]
use ctx_history_index::MAX_SOURCE_EVENT_PAGE_ITEMS;
use ctx_history_index::{EventRecord, IndexError, SourceEventCursor, VerifiedIndex};
use ctx_pro_host_protocol::{
    certified_source_revision_sha256, ErrorClass, SourceCommandFact, SourceManifest,
    SourceMessageFact, SourceOutcome, SourceRecord, SourceRecordMetadata, SourceResultFact,
    SourceSessionRelationships, TransientSourceContent, TransientSourceFact,
    MAX_SOURCE_CONTENT_BYTES, MAX_SOURCE_CONTENT_BYTES_PER_PAGE, MAX_SOURCE_RECORDS_PER_PAGE,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::source_backed_feed::{
    sync_source_backed_pro_feed_deferred_paged,
    SourceBackedProProvider as SourceBackedProProviderContract, SourceBackedProSyncReport,
    SourceBackedProviderPage,
};

const SOURCE_EVENT_FRONTIER_KIND_V1: &str = "ctx-source-event-page-v1";
const SOURCE_EVENT_FRONTIER_VERSION_V1: u16 = 1;

#[derive(Debug, Error)]
pub(super) enum SourceBackedProProviderError {
    #[error(
        "source_generation_mismatch: supplied manifest generation {manifest_generation} \
         does not match pinned index generation {index_generation}"
    )]
    GenerationMismatch {
        manifest_generation: String,
        index_generation: String,
    },
    #[error("source_manifest_mismatch: supplied source set is not the pinned index source set")]
    ManifestSourcesMismatch,
    #[error("source_manifest_mismatch: supplied removal set is not the pinned index removal set")]
    ManifestRemovalsMismatch,
    #[error("source_manifest_invalid: {class:?}: {message}")]
    InvalidManifest { class: ErrorClass, message: String },
    #[error("source_index_error: {0}")]
    Index(#[from] IndexError),
    #[error("source_cursor_invalid: {0}")]
    CursorContract(#[from] ctx_history_core::ProjectionContractError),
    #[error("source_hydration_batch_invalid: {0}")]
    BatchContract(ctx_history_core::SourceResolverContractError),
    #[error("source_cursor_invalid: cursor serialization failed: {0}")]
    CursorSerialization(#[from] serde_json::Error),
    #[error(
        "source_cursor_generation_mismatch: cursor generation {cursor_generation} \
         does not match pinned index generation {index_generation}"
    )]
    CursorGenerationMismatch {
        cursor_generation: String,
        index_generation: String,
    },
    #[error("source_cursor_source_mismatch: cursor belongs to a different exact source")]
    CursorSourceMismatch,
    #[error("source_cursor_invalid: frontier is not a valid versioned source event cursor")]
    InvalidCursor,
    #[error("source_cursor_invalid: frontier cursor checksum does not match its encoded state")]
    CorruptCursor,
    #[error("source_certificate_mismatch: requested certificate is not pinned by the index")]
    SourceCertificateMismatch,
    #[error("source_hydration_failed: {kind:?} while hydrating event {event_id}: {detail}")]
    Hydration {
        kind: HydrationFailureKind,
        event_id: Box<StableEntityId>,
        detail: String,
    },
    #[error("source_hydration_failed: {kind:?} while hydrating an ordered page: {detail}")]
    BatchHydration {
        kind: HydrationFailureKind,
        detail: String,
    },
    #[error(
        "source_hydration_failed: resolver returned event {actual_event_id} \
         while hydrating {expected_event_id}"
    )]
    HydratedIdentityMismatch {
        expected_event_id: Box<StableEntityId>,
        actual_event_id: Box<StableEntityId>,
    },
    #[error(
        "source_hydration_failed: ordered page hydration returned {actual} records for \
         {expected} requests"
    )]
    HydratedPageCardinalityMismatch { expected: usize, actual: usize },
    #[error(
        "source_content_too_large: exact hydrated event {event_id} contains {actual} bytes, \
         maximum {maximum}"
    )]
    ContentBoundExceeded {
        event_id: Box<StableEntityId>,
        actual: usize,
        maximum: usize,
    },
    #[error(
        "source_page_content_too_large: exact hydrated page contains {actual} bytes, \
         maximum {maximum}"
    )]
    PageContentBoundExceeded { actual: usize, maximum: usize },
    #[error("source_record_invalid: {class:?}: {message}")]
    InvalidRecord { class: ErrorClass, message: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceEventFrontierV1 {
    version: u16,
    cursor: SourceEventCursor,
}

pub(super) struct SourceBackedProProvider<'a> {
    index: &'a VerifiedIndex,
    resolver: &'a SourceBackedResolverRegistry,
}

impl<'a> SourceBackedProProvider<'a> {
    fn new(
        index: &'a VerifiedIndex,
        resolver: &'a SourceBackedResolverRegistry,
        manifest: &SourceManifest,
    ) -> std::result::Result<Self, SourceBackedProProviderError> {
        manifest
            .validate()
            .map_err(|error| SourceBackedProProviderError::InvalidManifest {
                class: error.class,
                message: error.message,
            })?;
        if manifest.core_generation_id != index.generation_id() {
            return Err(SourceBackedProProviderError::GenerationMismatch {
                manifest_generation: manifest.core_generation_id.clone(),
                index_generation: index.generation_id().to_owned(),
            });
        }
        validate_manifest_sources(index, manifest)?;
        validate_manifest_removals(index, manifest)?;
        Ok(Self { index, resolver })
    }

    fn validate_source_certificate(
        &self,
        source: &ctx_history_core::CertifiedSource,
    ) -> std::result::Result<(), SourceBackedProProviderError> {
        let Some(pinned) = self.index.manifest().sources.iter().find(|candidate| {
            candidate.observation().source().identity() == source.observation().source().identity()
        }) else {
            return Err(SourceBackedProProviderError::SourceCertificateMismatch);
        };
        let pinned_revision = certified_revision(pinned)?;
        let requested_revision = certified_revision(source)?;
        if !pinned
            .observation()
            .source()
            .exact_descriptor_eq(source.observation().source())
            || pinned_revision != requested_revision
        {
            return Err(SourceBackedProProviderError::SourceCertificateMismatch);
        }
        Ok(())
    }

    fn reread(
        &self,
        source: &ctx_history_core::CertifiedSource,
        expected_prior_frontier: Option<&SourceFrontier>,
    ) -> std::result::Result<SourceBackedProviderPage, SourceBackedProProviderError> {
        self.validate_source_certificate(source)?;
        let source_key = source.observation().source();
        let cursor = expected_prior_frontier
            .map(|frontier| decode_cursor_frontier(self.index, source_key, frontier))
            .transpose()?;
        let page = self.index.source_event_page(
            source_key,
            cursor.as_ref(),
            MAX_SOURCE_RECORDS_PER_PAGE,
        )?;
        if page.generation_id != self.index.generation_id()
            || !page.source.exact_descriptor_eq(source_key)
        {
            return Err(SourceBackedProProviderError::InvalidCursor);
        }

        let events = page.items;
        let requests = events
            .iter()
            .map(|event| {
                EventHydrationRequest::new(event.event_id, event.locator.clone()).map_err(|error| {
                    SourceBackedProProviderError::Hydration {
                        kind: HydrationFailureKind::InvalidLocator,
                        event_id: Box::new(event.event_id),
                        detail: error.to_string(),
                    }
                })
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let hydrated_records = hydrate_source_event_page(self.resolver, &requests)?;
        if hydrated_records.len() != events.len() {
            return Err(
                SourceBackedProProviderError::HydratedPageCardinalityMismatch {
                    expected: events.len(),
                    actual: hydrated_records.len(),
                },
            );
        }

        let mut content_bytes = 0_usize;
        let mut records = Vec::with_capacity(events.len());
        for (event, hydrated) in events.into_iter().zip(hydrated_records) {
            if hydrated.event_id != event.event_id {
                return Err(SourceBackedProProviderError::HydratedIdentityMismatch {
                    expected_event_id: Box::new(event.event_id),
                    actual_event_id: Box::new(hydrated.event_id),
                });
            }
            let exact_bytes = hydrated.provider_bytes;
            if exact_bytes.len() > MAX_SOURCE_CONTENT_BYTES {
                return Err(SourceBackedProProviderError::ContentBoundExceeded {
                    event_id: Box::new(event.event_id),
                    actual: exact_bytes.len(),
                    maximum: MAX_SOURCE_CONTENT_BYTES,
                });
            }
            content_bytes = content_bytes.checked_add(exact_bytes.len()).ok_or(
                SourceBackedProProviderError::PageContentBoundExceeded {
                    actual: usize::MAX,
                    maximum: MAX_SOURCE_CONTENT_BYTES_PER_PAGE,
                },
            )?;
            if content_bytes > MAX_SOURCE_CONTENT_BYTES_PER_PAGE {
                return Err(SourceBackedProProviderError::PageContentBoundExceeded {
                    actual: content_bytes,
                    maximum: MAX_SOURCE_CONTENT_BYTES_PER_PAGE,
                });
            }
            records.push(source_record(event, exact_bytes)?);
        }
        records.sort_by_key(|record| (record.metadata.event_sequence, record.event_id.digest()));

        let next_frontier = if page.terminal {
            source.frontier().cloned()
        } else {
            let cursor = page
                .next_cursor
                .ok_or(SourceBackedProProviderError::InvalidCursor)?;
            Some(encode_cursor_frontier(cursor)?)
        };
        Ok(SourceBackedProviderPage {
            source: source_key.clone(),
            expected_prior_frontier: expected_prior_frontier.cloned(),
            next_frontier,
            terminal: page.terminal,
            records,
        })
    }
}

fn hydrate_source_event_page(
    resolver: &SourceBackedResolverRegistry,
    requests: &[EventHydrationRequest],
) -> std::result::Result<Vec<HydratedProviderRecord>, SourceBackedProProviderError> {
    let batch = BatchHydrationRequest::new(requests.to_vec())
        .map_err(SourceBackedProProviderError::BatchContract)?;
    resolver
        .hydrate_batch(&batch)
        .map_err(|failure| SourceBackedProProviderError::BatchHydration {
            kind: failure.kind,
            detail: failure.detail,
        })
        .map(ctx_history_core::BatchHydrationResult::into_records)
}

impl SourceBackedProProviderContract for SourceBackedProProvider<'_> {
    fn reread_source_page(
        &mut self,
        source: &ctx_history_core::CertifiedSource,
        expected_prior_frontier: Option<&SourceFrontier>,
    ) -> Result<SourceBackedProviderPage> {
        self.reread(source, expected_prior_frontier)
            .map_err(Into::into)
    }
}

pub(super) fn sync_generation_pinned_source_manifest(
    data_root: &Path,
    manifest: SourceManifest,
    index: &VerifiedIndex,
    resolver: &SourceBackedResolverRegistry,
) -> Result<SourceBackedProSyncReport> {
    let mut provider = SourceBackedProProvider::new(index, resolver, &manifest)?;
    sync_source_backed_pro_feed_deferred_paged(data_root, manifest, index.manifest(), &mut provider)
}

fn validate_manifest_sources(
    index: &VerifiedIndex,
    manifest: &SourceManifest,
) -> std::result::Result<(), SourceBackedProProviderError> {
    if manifest.sources.len() != index.manifest().sources.len() {
        return Err(SourceBackedProProviderError::ManifestSourcesMismatch);
    }
    let pinned = index
        .manifest()
        .sources
        .iter()
        .map(|source| (source.observation().source().identity().digest(), source))
        .collect::<BTreeMap<_, _>>();
    for source in &manifest.sources {
        let Some(candidate) = pinned.get(&source.observation().source().identity().digest()) else {
            return Err(SourceBackedProProviderError::ManifestSourcesMismatch);
        };
        if !candidate
            .observation()
            .source()
            .exact_descriptor_eq(source.observation().source())
            || certified_revision(candidate)? != certified_revision(source)?
        {
            return Err(SourceBackedProProviderError::ManifestSourcesMismatch);
        }
    }
    Ok(())
}

fn validate_manifest_removals(
    index: &VerifiedIndex,
    manifest: &SourceManifest,
) -> std::result::Result<(), SourceBackedProProviderError> {
    if manifest.removals.len() != index.manifest().removals.len() {
        return Err(SourceBackedProProviderError::ManifestRemovalsMismatch);
    }
    let pinned = index
        .manifest()
        .removals
        .iter()
        .map(|removal| (removal.source().identity().digest(), removal))
        .collect::<BTreeMap<_, _>>();
    for removal in &manifest.removals {
        let Some(candidate) = pinned.get(&removal.deletion.source().identity().digest()) else {
            return Err(SourceBackedProProviderError::ManifestRemovalsMismatch);
        };
        if !candidate
            .source()
            .exact_descriptor_eq(removal.deletion.source())
            || candidate.deletion() != &removal.deletion
            || candidate.inventory() != &removal.inventory
        {
            return Err(SourceBackedProProviderError::ManifestRemovalsMismatch);
        }
    }
    Ok(())
}

fn certified_revision(
    source: &ctx_history_core::CertifiedSource,
) -> std::result::Result<String, SourceBackedProProviderError> {
    certified_source_revision_sha256(source).map_err(|error| {
        SourceBackedProProviderError::InvalidManifest {
            class: error.class,
            message: error.message,
        }
    })
}

fn encode_cursor_frontier(
    cursor: SourceEventCursor,
) -> std::result::Result<SourceFrontier, SourceBackedProProviderError> {
    let encoded = serde_json::to_vec(&SourceEventFrontierV1 {
        version: SOURCE_EVENT_FRONTIER_VERSION_V1,
        cursor,
    })?;
    let encoded_len =
        u64::try_from(encoded.len()).map_err(|_| SourceBackedProProviderError::InvalidCursor)?;
    let digest: [u8; 32] = Sha256::digest(&encoded).into();
    Ok(SourceFrontier::new(
        SOURCE_EVENT_FRONTIER_KIND_V1,
        TypedKey::bytes(encoded)?,
        encoded_len,
        digest,
    )?)
}

fn decode_cursor_frontier(
    index: &VerifiedIndex,
    source: &SourceKey,
    frontier: &SourceFrontier,
) -> std::result::Result<SourceEventCursor, SourceBackedProProviderError> {
    if frontier.checkpoint_kind() != SOURCE_EVENT_FRONTIER_KIND_V1 {
        return Err(SourceBackedProProviderError::InvalidCursor);
    }
    let TypedKey::Bytes(encoded) = frontier.checkpoint() else {
        return Err(SourceBackedProProviderError::InvalidCursor);
    };
    let encoded_len =
        u64::try_from(encoded.len()).map_err(|_| SourceBackedProProviderError::InvalidCursor)?;
    let digest: [u8; 32] = Sha256::digest(encoded).into();
    if frontier.certified_prefix_bytes() != encoded_len
        || frontier.certified_prefix_digest() != &digest
    {
        return Err(SourceBackedProProviderError::CorruptCursor);
    }
    let decoded: SourceEventFrontierV1 = serde_json::from_slice(encoded)?;
    if decoded.version != SOURCE_EVENT_FRONTIER_VERSION_V1 {
        return Err(SourceBackedProProviderError::InvalidCursor);
    }
    if decoded.cursor.generation_id() != index.generation_id() {
        return Err(SourceBackedProProviderError::CursorGenerationMismatch {
            cursor_generation: decoded.cursor.generation_id().to_owned(),
            index_generation: index.generation_id().to_owned(),
        });
    }
    if !decoded.cursor.source().exact_descriptor_eq(source) {
        return Err(SourceBackedProProviderError::CursorSourceMismatch);
    }
    Ok(decoded.cursor)
}

fn source_record(
    event: EventRecord,
    exact_bytes: Vec<u8>,
) -> std::result::Result<SourceRecord, SourceBackedProProviderError> {
    let content = TransientSourceContent::from_bytes(&exact_bytes).ok_or(
        SourceBackedProProviderError::ContentBoundExceeded {
            event_id: Box::new(event.event_id),
            actual: exact_bytes.len(),
            maximum: MAX_SOURCE_CONTENT_BYTES,
        },
    )?;
    let fact = match event_class(&event.event_type) {
        SourceEventClass::Message => TransientSourceFact::Message(SourceMessageFact { content }),
        SourceEventClass::Command => TransientSourceFact::Command(SourceCommandFact {
            call_id: None,
            tool_name: None,
            command: content,
            working_directory: event.cwd.clone(),
        }),
        SourceEventClass::Result | SourceEventClass::Output => {
            TransientSourceFact::Result(SourceResultFact {
                call_id: None,
                outcome: SourceOutcome::Unknown,
                exit_code: None,
                duration_ms: None,
                content,
            })
        }
    };
    SourceRecord::new(
        event.event_id,
        event.session_id,
        event.locator,
        SourceSessionRelationships {
            direct_session_id: event.session_id,
            root_session_id: event.root_session_id,
            parent_session_id: event.parent_session_id,
            provider_session_id: event.provider_session_id,
            agent_id: None,
        },
        None,
        SourceRecordMetadata {
            event_sequence: event.event_sequence,
            occurred_at_unix_ms: event.occurred_at_unix_ms,
            event_type: event.event_type,
            role: event.role,
            workspace: event.workspace,
            cwd: event.cwd,
            touched_files: event.touched_files,
        },
        vec![fact],
    )
    .map_err(|error| SourceBackedProProviderError::InvalidRecord {
        class: error.class,
        message: error.message,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceEventClass {
    Message,
    Command,
    Result,
    Output,
}

fn event_class(event_type: &str) -> SourceEventClass {
    match event_type {
        "command" | "tool_call" | "command_started" => SourceEventClass::Command,
        "result" | "tool_output" | "command_finished" => SourceEventClass::Result,
        "output" | "command_output" => SourceEventClass::Output,
        _ => SourceEventClass::Message,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
        sync::Arc,
    };

    use ctx_history_capture::{
        ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
        ProviderSourceStatus, SourceBackedProviderRegistry, SourceBackedRoute,
        SourceBackedRouteDriver, SourceBackedSelectorAuthority,
    };
    use ctx_history_core::{
        derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, EventIdentityInput,
        HydrationFailure, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
        NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
        SourceObservation, SourceRecordLocator,
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
            SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &fixture.manifest)
                .unwrap();

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
            SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &fixture.manifest)
                .unwrap();

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
            SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &fixture.manifest)
                .unwrap();
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
    fn production_provider_terminal_frontier_preserves_none() {
        let fixture = runtime_fixture(Vec::new(), false);
        let mut provider =
            SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &fixture.manifest)
                .unwrap();

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
            SourceBackedProProvider::new(&fixture.index, &fixture.resolver, &fixture.manifest)
                .unwrap();

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
        let runtime = include_str!("source_backed_pro_provider.rs");

        assert!(!runtime.contains(&["ctx_history_", "store"].concat()));
        assert!(!runtime.contains(&["Store", "::"].concat()));
        assert!(!runtime.contains(&[".", "preview"].concat()));
        assert!(!runtime.contains(&["body_", "store"].concat()));
    }

    fn runtime_fixture(
        records: Vec<(&'static str, Vec<u8>)>,
        with_frontier: bool,
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
                cwd: Some("/fixture/cwd".to_owned()),
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
            SourceManifest::new(index.generation_id(), vec![certificate.clone()], Vec::new())
                .unwrap();

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
}
