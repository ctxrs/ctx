#[path = "source_backed_pro_provider/repository.rs"]
mod repository;

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
    SourceMessageFact, SourceOutcome, SourceRecord, SourceRecordMetadata, SourceRepositoryContext,
    SourceResultFact, SourceSessionRelationships, TransientSourceContent, TransientSourceFact,
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
    #[error(transparent)]
    RepositoryAuthority(#[from] repository::RepositoryAuthorityError),
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
    repository: Option<repository::RepositoryAuthority>,
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
        Ok(Self {
            index,
            resolver,
            repository: repository::RepositoryAuthority::discover(),
        })
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
        &mut self,
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
            let repository = self
                .repository
                .as_mut()
                .map(|authority| authority.context_for(event.cwd.as_deref()))
                .transpose()?
                .flatten();
            records.push(source_record(event, exact_bytes, repository)?);
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
    repository: Option<SourceRepositoryContext>,
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
        repository,
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
#[path = "source_backed_pro_provider/tests.rs"]
mod tests;
