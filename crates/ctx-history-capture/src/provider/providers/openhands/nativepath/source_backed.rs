use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    io,
    path::{Component, Path, PathBuf},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceInventory,
    ContentSourceResolver, EventHydrationRequest, EventIdentityInput, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ScannedSourceCounts, SessionHydrationRequest,
    SessionIdentityInput, SourceAnchor, SourceInventoryObservation, SourceKey, SourceObservation,
    SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::output::openhands_output_outcome;
use crate::{
    common::io::{open_provider_source_path, OpenedProviderSourcePath, ProviderSourceDirectory},
    provider::file_touches::{
        event_type_supports_structured_file_touches, visit_provider_file_touch_drafts_with_limit,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
    },
    CaptureError, OutputOutcome, OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
};

use crate::provider::providers::openhands::{
    event::{decode_openhands_event, OpenHandsDecodedEvent},
    source::{
        discover_openhands_event_paths, normalized_openhands_authority_path,
        OpenHandsFileObservation, OpenHandsInventory, OpenHandsObservedFile,
    },
};

const OPENHANDS_SOURCE_ANCHOR_NAMESPACE: &str = "openhands.v1-conversation";
const OPENHANDS_NATIVE_SESSION_NAMESPACE: &str = "openhands.v1-conversation";
const OPENHANDS_NATIVE_EVENT_NAMESPACE: &str = "openhands.v1-event";
const OPENHANDS_LOGICAL_SESSION_KIND: &str = "openhands-conversation";
const OPENHANDS_LOGICAL_EVENT_KIND: &str = "openhands-event";
const OPENHANDS_SOURCE_SCHEMA_VARIANT: &str = "openhands-v1-conversation-tree-v1";
const OPENHANDS_SOURCE_REVISION_KIND: &str = "openhands-v1-conversation-leaves-v1";
const OPENHANDS_INVENTORY_AUTHORITY_NAMESPACE: &str = "openhands.v1-selected-tree";
const OPENHANDS_INVENTORY_REVISION_KIND: &str = "openhands-v1-event-file-inventory-v1";
const OPENHANDS_PARSER_REVISION: &str = "openhands-source-backed-v1";
const OPENHANDS_OBJECT_COORDINATE_KIND: &str = "openhands-event-object-v1";
const OPENHANDS_CURRENT_CLI_MAX_ENTRIES: usize = 16_384;

const OPENHANDS_LEAF_REVISION_DOMAIN: &[u8] = b"ctx.openhands.leaf-revision.v1\0";
const OPENHANDS_CONVERSATION_REVISION_DOMAIN: &[u8] = b"ctx.openhands.conversation-revision.v1\0";
const OPENHANDS_CONVERSATION_CONTENT_DOMAIN: &[u8] = b"ctx.openhands.conversation-content.v1\0";
const OPENHANDS_INVENTORY_REVISION_DOMAIN: &[u8] = b"ctx.openhands.inventory-revision.v1\0";

#[derive(Debug, Error)]
pub(crate) enum OpenHandsSourceBackedErrorV1 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ctx_history_core::ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("no OpenHands V1 event files were found below {root:?}")]
    NoV1EventFiles { root: PathBuf },
    #[error(
        "OpenHands CLI conversations/*/events history at {root:?} is detected but unsupported"
    )]
    UnsupportedCurrentCliFormat { root: PathBuf },
    #[error("OpenHands V1 event path {path:?} has no conversation coordinate")]
    MissingConversationCoordinate { path: PathBuf },
    #[error("OpenHands V1 conversation ID {0:?} appears in more than one tree")]
    DuplicateConversationId(String),
    #[error(
        "OpenHands V1 event ID {event_id:?} is duplicated in conversation {conversation_id:?}"
    )]
    DuplicateEventId {
        conversation_id: String,
        event_id: String,
    },
    #[error("OpenHands event path {path:?} has no bounded relative UTF-8 key")]
    InvalidRelativeEventKey { path: PathBuf },
    #[error("OpenHands event file {path:?} exceeds the bounded provider-record limit")]
    RecordTooLarge { path: PathBuf },
    #[error("OpenHands V1 source changed while its conversation tree was being projected")]
    SourceChangedDuringProjection,
    #[error("OpenHands source-backed count overflow")]
    CountOverflow,
    #[error("locator is not an OpenHands V1 conversation-tree event")]
    InvalidLocator,
    #[error("OpenHands locator conversation {0:?} was not found below the selected V1 tree")]
    LocatorConversationNotFound(String),
    #[error("OpenHands locator leaf {0:?} was not found below the selected V1 tree")]
    LocatorLeafNotFound(String),
    #[error("OpenHands locator leaf revision no longer matches the exact event file")]
    LeafRevisionMismatch,
    #[error("OpenHands locator record digest no longer matches the exact event file")]
    RecordDigestMismatch,
    #[error("OpenHands locator object coordinate no longer matches the decoded event")]
    ObjectCoordinateMismatch,
    #[error("OpenHands hydrated event identity does not match its locator coordinate")]
    EventIdentityMismatch,
    #[error("OpenHands hydrated session identity does not match its locator source")]
    SessionIdentityMismatch,
    #[error("OpenHands exact event file no longer decodes: {0}")]
    DecodeFailed(String),
}

pub(crate) type OpenHandsSourceBackedResultV1<T> = Result<T, OpenHandsSourceBackedErrorV1>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenHandsRejectedEventV1 {
    pub relative_file_key: String,
    pub reason: String,
}

#[derive(Debug)]
pub(crate) struct OpenHandsSourceBackedProjectionV1 {
    inventory: CertifiedSourceInventory,
    sources: Vec<CertifiedSource>,
    documents: Vec<LexicalDocument>,
    rejections: Vec<OpenHandsRejectedEventV1>,
}

impl OpenHandsSourceBackedProjectionV1 {
    pub(crate) fn inventory(&self) -> &CertifiedSourceInventory {
        &self.inventory
    }

    pub(crate) fn sources(&self) -> &[CertifiedSource] {
        &self.sources
    }

    pub(crate) fn documents(&self) -> &[LexicalDocument] {
        &self.documents
    }

    pub(crate) fn rejections(&self) -> &[OpenHandsRejectedEventV1] {
        &self.rejections
    }
}

#[derive(Debug)]
pub(crate) struct OpenHandsSourceBackedAdapterV1 {
    selected_root: PathBuf,
    inventory: OpenHandsInventory,
}

impl OpenHandsSourceBackedAdapterV1 {
    pub(crate) fn discover(selected_root: impl AsRef<Path>) -> OpenHandsSourceBackedResultV1<Self> {
        let selected_root = selected_root.as_ref();
        let inventory = discover_openhands_event_paths(selected_root)?;
        if inventory.paths.is_empty() {
            return if detects_current_cli_format(inventory.selected_path())? {
                Err(OpenHandsSourceBackedErrorV1::UnsupportedCurrentCliFormat {
                    root: selected_root.to_path_buf(),
                })
            } else {
                Err(OpenHandsSourceBackedErrorV1::NoV1EventFiles {
                    root: selected_root.to_path_buf(),
                })
            };
        }
        Ok(Self {
            selected_root: inventory.selected_path().to_path_buf(),
            inventory,
        })
    }

    pub(crate) fn project(
        &self,
    ) -> OpenHandsSourceBackedResultV1<OpenHandsSourceBackedProjectionV1> {
        let opening_paths = discover_required_paths(&self.inventory)?;
        let opening_inventory = inventory_observation(&self.selected_root, &opening_paths)?;
        let plans = bind_conversations(&opening_paths)?;
        let source_keys = plans
            .values()
            .map(|plan| plan.source.clone())
            .collect::<Vec<_>>();

        let mut sources = Vec::with_capacity(plans.len());
        let mut documents = Vec::new();
        let mut rejections = Vec::new();
        let mut witnesses = Vec::with_capacity(opening_paths.len());

        for plan in plans.values() {
            let projected = project_conversation(&self.inventory, plan)?;
            witnesses.extend(projected.witnesses);
            documents.extend(projected.documents);
            rejections.extend(projected.rejections);
            sources.push(projected.source);
        }

        if witnesses
            .iter()
            .any(|source| source.revalidate().map(|same| !same).unwrap_or(true))
        {
            return Err(OpenHandsSourceBackedErrorV1::SourceChangedDuringProjection);
        }

        let closing_paths = discover_required_paths(&self.inventory)?;
        let closing_inventory = inventory_observation(&self.selected_root, &closing_paths)?;
        let inventory = CertifiedSourceInventory::certify(
            opening_inventory,
            closing_inventory,
            OPENHANDS_PARSER_REVISION,
            source_keys,
        )?;
        self.inventory.revalidate()?;

        Ok(OpenHandsSourceBackedProjectionV1 {
            inventory,
            sources,
            documents,
            rejections,
        })
    }
}

pub(crate) fn project_openhands_source_backed_v1(
    selected_root: impl AsRef<Path>,
) -> OpenHandsSourceBackedResultV1<OpenHandsSourceBackedProjectionV1> {
    OpenHandsSourceBackedAdapterV1::discover(selected_root)?.project()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenHandsHydratedRecordV1 {
    pub provider_bytes: Vec<u8>,
    pub decoded_display_text: String,
}

#[derive(Debug)]
struct LocatorRoute {
    path: PathBuf,
    relative_file_key: String,
}

#[derive(Debug)]
pub(crate) struct OpenHandsLocatorResolverV1 {
    inventory: OpenHandsInventory,
    routes: BTreeMap<([u8; 32], String), LocatorRoute>,
}

impl OpenHandsLocatorResolverV1 {
    pub(crate) fn discover(selected_root: impl AsRef<Path>) -> OpenHandsSourceBackedResultV1<Self> {
        let adapter = OpenHandsSourceBackedAdapterV1::discover(selected_root)?;
        let paths = discover_required_paths(&adapter.inventory)?;
        let plans = bind_conversations(&paths)?;
        let mut routes = BTreeMap::new();
        for plan in plans.values() {
            for event in &plan.events {
                let key = (
                    plan.source.identity().digest(),
                    event.relative_file_key.clone(),
                );
                if routes
                    .insert(
                        key,
                        LocatorRoute {
                            path: event.path.clone(),
                            relative_file_key: event.relative_file_key.clone(),
                        },
                    )
                    .is_some()
                {
                    return Err(OpenHandsSourceBackedErrorV1::InvalidLocator);
                }
            }
        }
        Ok(Self {
            inventory: adapter.inventory,
            routes,
        })
    }

    pub(crate) fn hydrate(
        &self,
        locator: &SourceRecordLocator,
    ) -> OpenHandsSourceBackedResultV1<OpenHandsHydratedRecordV1> {
        locator.validate_contract()?;
        let coordinate = validate_locator(locator)?;
        let key = (
            locator.source().identity().digest(),
            coordinate.relative_file_key.clone(),
        );
        let route = self.routes.get(&key).ok_or_else(|| {
            if self
                .routes
                .keys()
                .any(|(source_digest, _)| source_digest == &key.0)
            {
                OpenHandsSourceBackedErrorV1::LocatorLeafNotFound(
                    coordinate.relative_file_key.clone(),
                )
            } else {
                OpenHandsSourceBackedErrorV1::LocatorConversationNotFound(
                    coordinate.conversation_id.clone(),
                )
            }
        })?;
        if route.relative_file_key != coordinate.relative_file_key {
            return Err(OpenHandsSourceBackedErrorV1::InvalidLocator);
        }

        let mut observed = self.inventory.open_source(&route.path)?;
        let provider_bytes = observed.raw_bytes.take().ok_or_else(|| {
            OpenHandsSourceBackedErrorV1::RecordTooLarge {
                path: route.path.clone(),
            }
        })?;
        let content_digest = observed.content_sha256.ok_or_else(|| {
            OpenHandsSourceBackedErrorV1::RecordTooLarge {
                path: route.path.clone(),
            }
        })?;
        let leaf_revision = leaf_revision_digest(
            &route.relative_file_key,
            &observed.observation,
            content_digest,
        )?;
        if leaf_revision != coordinate.leaf_revision {
            return Err(OpenHandsSourceBackedErrorV1::LeafRevisionMismatch);
        }
        if content_digest != *locator.record_digest() {
            return Err(OpenHandsSourceBackedErrorV1::RecordDigestMismatch);
        }
        let decoded = decode_openhands_event(&route.path, &provider_bytes)
            .map_err(|error| OpenHandsSourceBackedErrorV1::DecodeFailed(error.to_string()))?;
        if decoded.event_id() != coordinate.event_id {
            return Err(OpenHandsSourceBackedErrorV1::ObjectCoordinateMismatch);
        }
        if !observed.revalidate()? {
            return Err(OpenHandsSourceBackedErrorV1::SourceChangedDuringProjection);
        }
        self.inventory.revalidate()?;
        let decoded_display_text =
            lexical_body(&decoded).ok_or(OpenHandsSourceBackedErrorV1::ObjectCoordinateMismatch)?;
        Ok(OpenHandsHydratedRecordV1 {
            provider_bytes: decoded_display_text.as_bytes().to_vec(),
            decoded_display_text,
        })
    }

    fn hydrate_request(
        &self,
        request: &EventHydrationRequest,
    ) -> OpenHandsSourceBackedResultV1<OpenHandsHydratedRecordV1> {
        let coordinate = validate_locator(request.locator())?;
        let (session_id, event_id) = identities(
            request.locator().source(),
            &coordinate.conversation_id,
            &coordinate.event_id,
        )?;
        if event_id != request.event_id() {
            return Err(OpenHandsSourceBackedErrorV1::EventIdentityMismatch);
        }
        let _ = session_id;
        self.hydrate(request.locator())
    }
}

impl ContentSourceResolver for OpenHandsLocatorResolverV1 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        self.hydrate_request(request)
            .map(|record| HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: record.decoded_display_text.into_bytes(),
            })
            .map_err(hydration_failure)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        if let Some(first) = request.events().first() {
            let coordinate = validate_locator(first.locator()).map_err(hydration_failure)?;
            let (session_id, _) = identities(
                first.locator().source(),
                &coordinate.conversation_id,
                &coordinate.event_id,
            )
            .map_err(hydration_failure)?;
            if session_id != request.session_id() {
                return Err(hydration_failure(
                    OpenHandsSourceBackedErrorV1::SessionIdentityMismatch,
                ));
            }
        }
        request
            .events()
            .iter()
            .map(|event| {
                self.hydrate_request(event)
                    .map(|record| HydratedProviderRecord {
                        event_id: event.event_id(),
                        provider_bytes: record.decoded_display_text.into_bytes(),
                    })
                    .map_err(hydration_failure)
            })
            .collect()
    }
}

#[derive(Debug)]
struct EventPlan {
    path: PathBuf,
    relative_file_key: String,
}

#[derive(Debug)]
struct ConversationPlan {
    conversation_id: String,
    conversation_root: PathBuf,
    source: SourceKey,
    events: Vec<EventPlan>,
}

#[derive(Debug)]
struct ProjectedConversation {
    source: CertifiedSource,
    documents: Vec<LexicalDocument>,
    rejections: Vec<OpenHandsRejectedEventV1>,
    witnesses: Vec<OpenHandsObservedFile>,
}

fn discover_required_paths(
    inventory: &OpenHandsInventory,
) -> OpenHandsSourceBackedResultV1<Vec<PathBuf>> {
    let paths = inventory.refresh_paths()?;
    if paths.is_empty() {
        return Err(OpenHandsSourceBackedErrorV1::SourceChangedDuringProjection);
    }
    Ok(paths)
}

fn bind_conversations(
    paths: &[PathBuf],
) -> OpenHandsSourceBackedResultV1<BTreeMap<String, ConversationPlan>> {
    let mut conversations = BTreeMap::new();
    for path in paths {
        let (conversation_id, conversation_root) = conversation_coordinate(path)?;
        let relative_file_key = relative_event_file_key(&conversation_root, path)?;
        let source = source_key(&conversation_id)?;
        let plan = conversations
            .entry(conversation_id.clone())
            .or_insert_with(|| ConversationPlan {
                conversation_id: conversation_id.clone(),
                conversation_root: conversation_root.clone(),
                source,
                events: Vec::new(),
            });
        if plan.conversation_root != conversation_root {
            return Err(OpenHandsSourceBackedErrorV1::DuplicateConversationId(
                conversation_id,
            ));
        }
        plan.events.push(EventPlan {
            path: path.clone(),
            relative_file_key,
        });
    }
    for plan in conversations.values_mut() {
        plan.events
            .sort_by(|left, right| left.relative_file_key.cmp(&right.relative_file_key));
    }
    Ok(conversations)
}

fn project_conversation(
    inventory: &OpenHandsInventory,
    plan: &ConversationPlan,
) -> OpenHandsSourceBackedResultV1<ProjectedConversation> {
    let session_id = session_identity(&plan.source, &plan.conversation_id)?;
    let mut documents = Vec::new();
    let mut rejections = Vec::new();
    let mut witnesses = Vec::with_capacity(plan.events.len());
    let mut event_ids = BTreeSet::new();
    let mut revision_digest = Sha256::new();
    revision_digest.update(OPENHANDS_CONVERSATION_REVISION_DOMAIN);
    revision_digest.update((plan.events.len() as u64).to_be_bytes());
    let mut content_digest = Sha256::new();
    content_digest.update(OPENHANDS_CONVERSATION_CONTENT_DOMAIN);
    content_digest.update((plan.events.len() as u64).to_be_bytes());
    let mut counts = ScannedSourceCounts::default();

    for (sequence, event) in plan.events.iter().enumerate() {
        let mut observed = inventory.open_source(&event.path)?;
        let provider_bytes = observed.raw_bytes.take().ok_or_else(|| {
            OpenHandsSourceBackedErrorV1::RecordTooLarge {
                path: event.path.clone(),
            }
        })?;
        let record_digest = observed.content_sha256.ok_or_else(|| {
            OpenHandsSourceBackedErrorV1::RecordTooLarge {
                path: event.path.clone(),
            }
        })?;
        let leaf_revision = leaf_revision_digest(
            &event.relative_file_key,
            &observed.observation,
            record_digest,
        )?;
        digest_leaf(
            &mut revision_digest,
            &event.relative_file_key,
            &leaf_revision,
            observed.observation.length,
        );
        digest_leaf(
            &mut content_digest,
            &event.relative_file_key,
            &record_digest,
            observed.observation.length,
        );
        counts.complete_records = checked_add(counts.complete_records, 1)?;
        counts.certified_bytes = checked_add(counts.certified_bytes, observed.observation.length)?;

        match decode_openhands_event(&event.path, &provider_bytes) {
            Ok(decoded) => {
                if !event_ids.insert(decoded.event_id().to_owned()) {
                    return Err(OpenHandsSourceBackedErrorV1::DuplicateEventId {
                        conversation_id: plan.conversation_id.clone(),
                        event_id: decoded.event_id().to_owned(),
                    });
                }
                if let Some(body) = lexical_body(&decoded) {
                    let event_id = event_identity(&plan.source, session_id, decoded.event_id())?;
                    let locator = source_locator(
                        &plan.source,
                        &event.relative_file_key,
                        decoded.event_id(),
                        leaf_revision,
                        record_digest,
                    )?;
                    documents.push(LexicalDocument {
                        event_id,
                        session_id,
                        parent_session_id: None,
                        root_session_id: session_id,
                        source: plan.source.clone(),
                        locator,
                        provider_session_id: Some(plan.conversation_id.clone()),
                        branch: None,
                        source_path: Some(observed.canonical_path_text.clone()),
                        agent_type: ctx_history_core::AgentType::Primary.as_str().to_owned(),
                        is_primary: true,
                        event_sequence: u64::try_from(sequence)
                            .map_err(|_| OpenHandsSourceBackedErrorV1::CountOverflow)?,
                        occurred_at_unix_ms: Some(decoded.timestamp().timestamp_millis()),
                        event_type: decoded.event_type().as_str().to_owned(),
                        role: Some(decoded.role().as_str().to_owned()),
                        body,
                        workspace: None,
                        cwd: None,
                        touched_files: touched_files(&decoded),
                    });
                    counts.retained_records = checked_add(counts.retained_records, 1)?;
                    counts.indexed_documents = checked_add(counts.indexed_documents, 1)?;
                } else {
                    counts.ignored_records = checked_add(counts.ignored_records, 1)?;
                }
            }
            Err(error) => {
                counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                rejections.push(OpenHandsRejectedEventV1 {
                    relative_file_key: event.relative_file_key.clone(),
                    reason: bounded_reason(error.to_string()),
                });
            }
        }
        witnesses.push(observed);
    }

    let revision: [u8; 32] = revision_digest.finalize().into();
    let content: [u8; 32] = content_digest.finalize().into();
    let observation = SourceObservation::new(
        plan.source.clone(),
        OPENHANDS_SOURCE_REVISION_KIND,
        revision.to_vec(),
    )?;
    let source = CertifiedSource::certify(
        observation.clone(),
        observation,
        OPENHANDS_PARSER_REVISION,
        content,
        counts,
    )?;
    Ok(ProjectedConversation {
        source,
        documents,
        rejections,
        witnesses,
    })
}

fn lexical_body(decoded: &OpenHandsDecodedEvent) -> Option<String> {
    let event_type = decoded.event_type();
    let text = if matches!(
        event_type,
        ctx_history_core::EventType::ToolOutput | ctx_history_core::EventType::CommandOutput
    ) {
        let outcome = openhands_output_outcome(decoded);
        if !matches!(
            outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        ) {
            return None;
        }
        if decoded.text().trim().is_empty() {
            if outcome.outcome == OutputOutcome::Timeout {
                "OpenHands command timed out".to_owned()
            } else {
                "OpenHands command failed".to_owned()
            }
        } else {
            decoded.text().to_owned()
        }
    } else {
        decoded.text().to_owned()
    };
    (!text.trim().is_empty()).then_some(text)
}

fn touched_files(decoded: &OpenHandsDecodedEvent) -> Vec<String> {
    let mut paths = Vec::new();
    let outcome = visit_provider_file_touch_drafts_with_limit(
        decoded.value(),
        event_type_supports_structured_file_touches(decoded.event_type()),
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(_, draft)| -> Result<(), Infallible> {
            paths.push(draft.path);
            Ok(())
        },
    );
    match outcome {
        Ok(_) => paths,
        Err(error) => match error {},
    }
}

fn source_key(conversation_id: &str) -> OpenHandsSourceBackedResultV1<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        OPENHANDS_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(conversation_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::OpenHands.as_str(),
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        OPENHANDS_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn session_identity(
    source: &SourceKey,
    conversation_id: &str,
) -> OpenHandsSourceBackedResultV1<StableEntityId> {
    let key = NativeSessionKey::native_id(
        OPENHANDS_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(conversation_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: OPENHANDS_LOGICAL_SESSION_KIND,
        native_session_key: &key,
    })?)
}

fn event_identity(
    source: &SourceKey,
    session_id: StableEntityId,
    event_id: &str,
) -> OpenHandsSourceBackedResultV1<StableEntityId> {
    let key =
        NativeItemKey::native_id(OPENHANDS_NATIVE_EVENT_NAMESPACE, TypedKey::utf8(event_id)?)?;
    Ok(derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: OPENHANDS_LOGICAL_EVENT_KIND,
        native_item_key: &key,
        subrecord_selector: None,
    })?)
}

fn identities(
    source: &SourceKey,
    conversation_id: &str,
    event_id: &str,
) -> OpenHandsSourceBackedResultV1<(StableEntityId, StableEntityId)> {
    let session_id = session_identity(source, conversation_id)?;
    let event_id = event_identity(source, session_id, event_id)?;
    Ok((session_id, event_id))
}

fn source_locator(
    source: &SourceKey,
    relative_file_key: &str,
    event_id: &str,
    leaf_revision: [u8; 32],
    record_digest: [u8; 32],
) -> OpenHandsSourceBackedResultV1<SourceRecordLocator> {
    let record_coordinate = TypedKey::composite(vec![
        TypedKey::utf8(OPENHANDS_OBJECT_COORDINATE_KIND)?,
        TypedKey::utf8(event_id)?,
        TypedKey::bytes(leaf_revision.to_vec())?,
    ])?;
    Ok(SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::utf8(relative_file_key)?,
            record_coordinate,
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        record_digest,
    )?)
}

struct LocatorCoordinate {
    conversation_id: String,
    relative_file_key: String,
    event_id: String,
    leaf_revision: [u8; 32],
}

fn validate_locator(
    locator: &SourceRecordLocator,
) -> OpenHandsSourceBackedResultV1<LocatorCoordinate> {
    let source = locator.source();
    if source.provider() != CaptureProvider::OpenHands.as_str()
        || source.source_format() != OPENHANDS_FILE_EVENTS_SOURCE_FORMAT
        || source.schema_variant() != OPENHANDS_SOURCE_SCHEMA_VARIANT
        || source.provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(OpenHandsSourceBackedErrorV1::InvalidLocator);
    }
    let SourceAnchor::ProviderNative { namespace, key } = source.anchor() else {
        return Err(OpenHandsSourceBackedErrorV1::InvalidLocator);
    };
    let TypedKey::Utf8(conversation_id) = key else {
        return Err(OpenHandsSourceBackedErrorV1::InvalidLocator);
    };
    if namespace != OPENHANDS_SOURCE_ANCHOR_NAMESPACE {
        return Err(OpenHandsSourceBackedErrorV1::InvalidLocator);
    }
    let NativeRecordCoordinate::TreeRecord {
        relative_file_key,
        record_coordinate,
    } = locator.coordinate()
    else {
        return Err(OpenHandsSourceBackedErrorV1::InvalidLocator);
    };
    let TypedKey::Utf8(relative_file_key) = relative_file_key else {
        return Err(OpenHandsSourceBackedErrorV1::InvalidLocator);
    };
    let TypedKey::Composite(parts) = record_coordinate else {
        return Err(OpenHandsSourceBackedErrorV1::InvalidLocator);
    };
    let [TypedKey::Utf8(kind), TypedKey::Utf8(event_id), TypedKey::Bytes(leaf_revision)] =
        parts.as_slice()
    else {
        return Err(OpenHandsSourceBackedErrorV1::InvalidLocator);
    };
    if kind != OPENHANDS_OBJECT_COORDINATE_KIND || leaf_revision.len() != 32 {
        return Err(OpenHandsSourceBackedErrorV1::InvalidLocator);
    }
    let mut exact_leaf_revision = [0_u8; 32];
    exact_leaf_revision.copy_from_slice(leaf_revision);
    Ok(LocatorCoordinate {
        conversation_id: conversation_id.clone(),
        relative_file_key: relative_file_key.clone(),
        event_id: event_id.clone(),
        leaf_revision: exact_leaf_revision,
    })
}

fn inventory_observation(
    root: &Path,
    paths: &[PathBuf],
) -> OpenHandsSourceBackedResultV1<SourceInventoryObservation> {
    let mut keys = paths
        .iter()
        .map(|path| relative_selected_key(root, path))
        .collect::<OpenHandsSourceBackedResultV1<Vec<_>>>()?;
    keys.sort();
    let mut digest = Sha256::new();
    digest.update(OPENHANDS_INVENTORY_REVISION_DOMAIN);
    digest.update((keys.len() as u64).to_be_bytes());
    for key in keys {
        digest.update((key.len() as u64).to_be_bytes());
        digest.update(key.as_bytes());
    }
    Ok(SourceInventoryObservation::new(
        CaptureProvider::OpenHands.as_str(),
        OPENHANDS_INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::utf8(root.to_string_lossy().into_owned())?,
        OPENHANDS_INVENTORY_REVISION_KIND,
        digest.finalize().to_vec(),
    )?)
}

fn leaf_revision_digest(
    relative_file_key: &str,
    observation: &OpenHandsFileObservation,
    content_digest: [u8; 32],
) -> OpenHandsSourceBackedResultV1<[u8; 32]> {
    let observation = serde_json::to_vec(observation)?;
    let mut digest = Sha256::new();
    digest.update(OPENHANDS_LEAF_REVISION_DOMAIN);
    digest.update((relative_file_key.len() as u64).to_be_bytes());
    digest.update(relative_file_key.as_bytes());
    digest.update((observation.len() as u64).to_be_bytes());
    digest.update(observation);
    digest.update(content_digest);
    Ok(digest.finalize().into())
}

fn digest_leaf(digest: &mut Sha256, relative_file_key: &str, evidence: &[u8; 32], length: u64) {
    digest.update((relative_file_key.len() as u64).to_be_bytes());
    digest.update(relative_file_key.as_bytes());
    digest.update(length.to_be_bytes());
    digest.update(evidence);
}

fn conversation_coordinate(path: &Path) -> OpenHandsSourceBackedResultV1<(String, PathBuf)> {
    let components = path.components().collect::<Vec<_>>();
    let Some(v1_index) = components
        .iter()
        .position(|component| component.as_os_str() == "v1_conversations")
    else {
        return Err(
            OpenHandsSourceBackedErrorV1::MissingConversationCoordinate {
                path: path.to_path_buf(),
            },
        );
    };
    let Some(conversation) = components.get(v1_index.saturating_add(1)) else {
        return Err(
            OpenHandsSourceBackedErrorV1::MissingConversationCoordinate {
                path: path.to_path_buf(),
            },
        );
    };
    let conversation_id = conversation
        .as_os_str()
        .to_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(
            || OpenHandsSourceBackedErrorV1::MissingConversationCoordinate {
                path: path.to_path_buf(),
            },
        )?
        .to_owned();
    let mut conversation_root = PathBuf::new();
    for component in components.iter().take(v1_index.saturating_add(2)) {
        conversation_root.push(component.as_os_str());
    }
    Ok((conversation_id, conversation_root))
}

fn relative_event_file_key(
    conversation_root: &Path,
    path: &Path,
) -> OpenHandsSourceBackedResultV1<String> {
    let relative = path.strip_prefix(conversation_root).map_err(|_| {
        OpenHandsSourceBackedErrorV1::InvalidRelativeEventKey {
            path: path.to_path_buf(),
        }
    })?;
    relative_path_key(relative, path)
}

fn relative_selected_key(root: &Path, path: &Path) -> OpenHandsSourceBackedResultV1<String> {
    if root == path {
        let relative = path.file_name().map(Path::new).ok_or_else(|| {
            OpenHandsSourceBackedErrorV1::InvalidRelativeEventKey {
                path: path.to_path_buf(),
            }
        })?;
        return relative_path_key(relative, path);
    }
    let relative = path.strip_prefix(root).map_err(|_| {
        OpenHandsSourceBackedErrorV1::InvalidRelativeEventKey {
            path: path.to_path_buf(),
        }
    })?;
    relative_path_key(relative, path)
}

fn relative_path_key(relative: &Path, original: &Path) -> OpenHandsSourceBackedResultV1<String> {
    let parts = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_owned),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| OpenHandsSourceBackedErrorV1::InvalidRelativeEventKey {
            path: original.to_path_buf(),
        })?;
    if parts.is_empty() {
        return Err(OpenHandsSourceBackedErrorV1::InvalidRelativeEventKey {
            path: original.to_path_buf(),
        });
    }
    Ok(parts.join("/"))
}

fn checked_add(left: u64, right: u64) -> OpenHandsSourceBackedResultV1<u64> {
    left.checked_add(right)
        .ok_or(OpenHandsSourceBackedErrorV1::CountOverflow)
}

fn bounded_reason(mut reason: String) -> String {
    const MAX_REASON_BYTES: usize = 4 * 1024;
    if reason.len() <= MAX_REASON_BYTES {
        return reason;
    }
    let mut end = MAX_REASON_BYTES;
    while !reason.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    reason.truncate(end);
    reason
}

fn detects_current_cli_format(path: &Path) -> OpenHandsSourceBackedResultV1<bool> {
    let path = normalized_openhands_authority_path(path)?;
    let opened = match open_provider_source_path(&path) {
        Ok(opened) => opened,
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(false)
        }
        Err(error) => return Err(error.into()),
    };
    if let OpenedProviderSourcePath::File(file) = opened {
        let detected = current_cli_event_file(&path)
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "events");
        file.revalidate()?;
        return Ok(detected);
    }
    let OpenedProviderSourcePath::Directory(directory) = opened else {
        return Err(CaptureError::SystemInvariant(
            "OpenHands CLI format root classification is incomplete",
        )
        .into());
    };
    if path.file_name().is_some_and(|name| name == "events")
        && directory_has_current_cli_event(&directory)?
    {
        return Ok(true);
    }
    let entries = directory.entries(OPENHANDS_CURRENT_CLI_MAX_ENTRIES.saturating_add(1))?;
    for name in &entries {
        if name == "events" {
            if let OpenedProviderSourcePath::Directory(events) = directory.open_child(name)? {
                if directory_has_current_cli_event(&events)? {
                    return Ok(true);
                }
            }
        }
    }
    for name in entries {
        let OpenedProviderSourcePath::Directory(child) = directory.open_child(&name)? else {
            continue;
        };
        match child.open_child(std::ffi::OsStr::new("events")) {
            Ok(OpenedProviderSourcePath::Directory(events))
                if directory_has_current_cli_event(&events)? =>
            {
                return Ok(true);
            }
            Ok(_) => {}
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        child.revalidate()?;
    }
    directory.revalidate()?;
    Ok(false)
}

fn directory_has_current_cli_event(
    directory: &ProviderSourceDirectory,
) -> OpenHandsSourceBackedResultV1<bool> {
    let names = directory.entries(OPENHANDS_CURRENT_CLI_MAX_ENTRIES.saturating_add(1))?;
    if names.len() > OPENHANDS_CURRENT_CLI_MAX_ENTRIES {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: directory.relative_path().to_path_buf(),
            reason: "OpenHands CLI history selector exceeds its bounded entry limit",
        }
        .into());
    }
    for name in names {
        if !current_cli_event_file(Path::new(&name)) {
            continue;
        }
        if let OpenedProviderSourcePath::File(file) = directory.open_child(&name)? {
            file.revalidate()?;
            directory.revalidate()?;
            return Ok(true);
        }
    }
    directory.revalidate()?;
    Ok(false)
}

fn current_cli_event_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("event-") && name.ends_with(".json"))
}

pub(super) fn hydration_failure(error: OpenHandsSourceBackedErrorV1) -> HydrationFailure {
    let kind = match &error {
        OpenHandsSourceBackedErrorV1::UnsupportedCurrentCliFormat { .. } => {
            HydrationFailureKind::UnsupportedParserRevision
        }
        OpenHandsSourceBackedErrorV1::LocatorConversationNotFound(_)
        | OpenHandsSourceBackedErrorV1::LocatorLeafNotFound(_) => {
            HydrationFailureKind::MissingRecord
        }
        OpenHandsSourceBackedErrorV1::LeafRevisionMismatch => {
            HydrationFailureKind::StaleSourceEvidence
        }
        OpenHandsSourceBackedErrorV1::RecordDigestMismatch
        | OpenHandsSourceBackedErrorV1::ObjectCoordinateMismatch
        | OpenHandsSourceBackedErrorV1::DecodeFailed(_) => {
            HydrationFailureKind::StaleRecordEvidence
        }
        OpenHandsSourceBackedErrorV1::SourceChangedDuringProjection
        | OpenHandsSourceBackedErrorV1::Capture(CaptureError::SourceChangedDuringCapture) => {
            HydrationFailureKind::TemporarilyUnavailable
        }
        _ => HydrationFailureKind::InvalidLocator,
    };
    HydrationFailure {
        kind,
        detail: bounded_reason(error.to_string()),
    }
}
