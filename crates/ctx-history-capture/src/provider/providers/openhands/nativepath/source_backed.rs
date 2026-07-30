use std::{
    collections::BTreeSet,
    convert::Infallible,
    path::{Component, Path, PathBuf},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceInventory,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceInventoryObservation, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit, MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        },
        normalization::provider_result_outcome_evidence,
        source_backed::{
            SourceBackedCoordinatorError, SourceBackedGenerationSink, SourceBackedRouteError,
            SourceBackedRouteErrorKind,
        },
    },
    provider_sources::{
        EventFileCoordinates, EventFileGroup, EventFileInventory, EventFileInventoryError,
        EventFileLimits,
    },
    CaptureError, OutputOutcome, MAX_PROVIDER_JSONL_LINE_BYTES,
    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
};

use crate::provider::providers::openhands::{
    event::{decode_openhands_event, OpenHandsDecodedEvent},
    source::{
        normalized_openhands_authority_path, openhands_checked_path_text,
        openhands_json_path_is_event, OpenHandsFileObservation, OPENHANDS_MAX_PATH_BYTES,
    },
};

mod detection;
mod hydration;

use detection::detects_current_cli_format;
#[cfg(test)]
pub(super) use hydration::{hydration_failure, validate_locator};

const OPENHANDS_SOURCE_ANCHOR_NAMESPACE: &str = "openhands.v1-conversation";
const OPENHANDS_NATIVE_SESSION_NAMESPACE: &str = "openhands.v1-conversation";
const OPENHANDS_NATIVE_EVENT_NAMESPACE: &str = "openhands.v1-event";
const OPENHANDS_LOGICAL_SESSION_KIND: &str = "openhands-conversation";
const OPENHANDS_LOGICAL_EVENT_KIND: &str = "openhands-event";
const OPENHANDS_SOURCE_SCHEMA_VARIANT: &str = "openhands-v1-conversation-tree-v1";
const OPENHANDS_SOURCE_REVISION_KIND: &str = "openhands-v1-conversation-leaves-v2";
const OPENHANDS_INVENTORY_AUTHORITY_NAMESPACE: &str = "openhands.v1-selected-tree";
const OPENHANDS_INVENTORY_REVISION_KIND: &str = "openhands-v1-event-file-inventory-v2";
const OPENHANDS_PARSER_REVISION: &str = "openhands-source-backed-v1";
const OPENHANDS_OBJECT_COORDINATE_KIND: &str = "openhands-event-object-v1";
const OPENHANDS_LEAF_REVISION_DOMAIN: &[u8] = b"ctx.openhands.leaf-revision.v1\0";
const OPENHANDS_CONVERSATION_CONTENT_DOMAIN: &[u8] = b"ctx.openhands.conversation-content.v1\0";
const OPENHANDS_DISCOVERY_MAX_DEPTH: usize = 16;
const OPENHANDS_DISCOVERY_MAX_ENTRIES: usize = 16_384;

#[derive(Debug, Error)]
pub(crate) enum OpenHandsSourceBackedErrorV2 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    EventFiles(#[from] EventFileInventoryError),
    #[error(transparent)]
    Coordinator(#[from] SourceBackedCoordinatorError),
    #[error(transparent)]
    Projection(#[from] ctx_history_core::ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
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

pub(crate) type OpenHandsSourceBackedResultV2<T> = Result<T, OpenHandsSourceBackedErrorV2>;
pub(super) type OpenHandsSourceBackedResultV1<T> = OpenHandsSourceBackedResultV2<T>;

#[derive(Debug, Clone)]
pub(crate) struct OpenHandsEventFileAdapterV2 {
    selected: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenHandsEventFileSourcePlan {
    pub source: SourceKey,
    pub conversation_id: String,
    pub session_id: StableEntityId,
    pub opening: SourceObservation,
}

impl OpenHandsEventFileAdapterV2 {
    pub(crate) fn new(selected: impl Into<PathBuf>) -> Self {
        Self {
            selected: selected.into(),
        }
    }

    pub(crate) fn open_inventory(&self) -> OpenHandsSourceBackedResultV2<EventFileInventory> {
        let selected = normalized_openhands_authority_path(&self.selected)?;
        let inventory = match EventFileInventory::open(
            &selected,
            EventFileLimits {
                max_depth: OPENHANDS_DISCOVERY_MAX_DEPTH,
                max_entries: OPENHANDS_DISCOVERY_MAX_ENTRIES,
                max_path_bytes: OPENHANDS_MAX_PATH_BYTES,
                max_record_bytes: MAX_PROVIDER_JSONL_LINE_BYTES,
            },
            classify_openhands_event,
        ) {
            Ok(inventory) => inventory,
            Err(error @ EventFileInventoryError::NoAcceptedExactFile { .. }) => {
                if detects_current_cli_format(&selected)? {
                    return Err(OpenHandsSourceBackedErrorV2::UnsupportedCurrentCliFormat {
                        root: selected,
                    });
                }
                return Err(error.into());
            }
            Err(error) => return Err(error.into()),
        };
        if inventory.is_empty() && detects_current_cli_format(&selected)? {
            return Err(OpenHandsSourceBackedErrorV2::UnsupportedCurrentCliFormat {
                root: selected,
            });
        }
        Ok(inventory)
    }

    pub(crate) fn bind_group(
        &self,
        group: EventFileGroup<'_>,
    ) -> OpenHandsSourceBackedResultV2<OpenHandsEventFileSourcePlan> {
        let mut conversation_root = None;
        for leaf in group.leaves() {
            let (conversation_id, root) = conversation_coordinate(leaf.display_path())?;
            if conversation_id != group.group_key() {
                return Err(OpenHandsSourceBackedErrorV2::InvalidRelativeEventKey {
                    path: leaf.display_path().to_path_buf(),
                });
            }
            match &conversation_root {
                Some(expected) if expected != &root => {
                    return Err(OpenHandsSourceBackedErrorV2::DuplicateConversationId(
                        conversation_id,
                    ));
                }
                Some(_) => {}
                None => conversation_root = Some(root),
            }
        }
        let conversation_id = group.group_key().to_owned();
        let source = source_key(&conversation_id)?;
        let session_id = session_identity(&source, &conversation_id)?;
        let opening = SourceObservation::new(
            source.clone(),
            OPENHANDS_SOURCE_REVISION_KIND,
            group.observation_digest().to_vec(),
        )?;
        Ok(OpenHandsEventFileSourcePlan {
            source,
            conversation_id,
            session_id,
            opening,
        })
    }

    pub(crate) fn exact_replay_matches(
        &self,
        base: &CertifiedSource,
        plan: &OpenHandsEventFileSourcePlan,
    ) -> bool {
        base.observation()
            .source()
            .exact_descriptor_eq(&plan.source)
            && base.observation() == &plan.opening
            && base.parser_revision() == OPENHANDS_PARSER_REVISION
            && base.frontier().is_none()
    }

    #[allow(
        dead_code,
        reason = "called by the shared direct-staging driver in the integration commit"
    )]
    pub(crate) fn project_replacement(
        &self,
        group: EventFileGroup<'_>,
        plan: &OpenHandsEventFileSourcePlan,
        sink: &mut SourceBackedGenerationSink<'_>,
    ) -> OpenHandsSourceBackedResultV2<CertifiedSource> {
        validate_plan_for_group(group, plan)?;
        sink.begin_source(plan.source.clone())?;
        let certificate = project_group(group, plan, |document| {
            sink.add_document(document)?;
            Ok(())
        })?;
        sink.certify_source(certificate.clone())?;
        Ok(certificate)
    }

    pub(crate) fn certify_complete_inventory(
        &self,
        inventory: &EventFileInventory,
    ) -> OpenHandsSourceBackedResultV2<CertifiedSourceInventory> {
        let mut sources = Vec::with_capacity(inventory.groups().len());
        for group in inventory.groups() {
            sources.push(self.bind_group(group)?.source);
        }
        let selected = openhands_checked_path_text(inventory.selected_path())?;
        let observation = SourceInventoryObservation::new(
            CaptureProvider::OpenHands.as_str(),
            OPENHANDS_INVENTORY_AUTHORITY_NAMESPACE,
            TypedKey::utf8(selected)?,
            OPENHANDS_INVENTORY_REVISION_KIND,
            inventory.observation_digest().to_vec(),
        )?;
        inventory.revalidate_all()?;
        Ok(CertifiedSourceInventory::certify(
            observation.clone(),
            observation,
            OPENHANDS_PARSER_REVISION,
            sources,
        )?)
    }

    #[allow(
        dead_code,
        reason = "called by shared terminal callbacks in the integration commit"
    )]
    pub(crate) fn revalidate_inventory(
        &self,
        inventory: &EventFileInventory,
    ) -> OpenHandsSourceBackedResultV2<()> {
        inventory.revalidate_all()?;
        Ok(())
    }
}

pub(crate) fn openhands_owns_source(source: &SourceKey) -> bool {
    if source.provider() != CaptureProvider::OpenHands.as_str()
        || source.source_format() != OPENHANDS_FILE_EVENTS_SOURCE_FORMAT
        || source.schema_variant() != OPENHANDS_SOURCE_SCHEMA_VARIANT
        || source.provider_identity_version() != 1
    {
        return false;
    }
    matches!(
        source.anchor(),
        SourceAnchor::ProviderNative { namespace, key: TypedKey::Utf8(value) }
            if namespace == OPENHANDS_SOURCE_ANCHOR_NAMESPACE && !value.is_empty()
    )
}

pub(crate) fn openhands_route_error(error: OpenHandsSourceBackedErrorV2) -> SourceBackedRouteError {
    let kind = match &error {
        OpenHandsSourceBackedErrorV2::UnsupportedCurrentCliFormat { .. } => {
            SourceBackedRouteErrorKind::Unsupported
        }
        OpenHandsSourceBackedErrorV2::EventFiles(EventFileInventoryError::Unavailable {
            ..
        }) => SourceBackedRouteErrorKind::Unavailable,
        OpenHandsSourceBackedErrorV2::EventFiles(EventFileInventoryError::SourceChanged {
            ..
        })
        | OpenHandsSourceBackedErrorV2::Capture(CaptureError::SourceChangedDuringCapture) => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        OpenHandsSourceBackedErrorV2::Coordinator(_) => SourceBackedRouteErrorKind::Internal,
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

fn classify_openhands_event(path: &Path) -> crate::Result<Option<EventFileCoordinates>> {
    if !openhands_json_path_is_event(path) {
        return Ok(None);
    }
    let (conversation_id, conversation_root) =
        conversation_coordinate(path).map_err(openhands_error_as_capture)?;
    let relative_file_key =
        relative_event_file_key(&conversation_root, path).map_err(openhands_error_as_capture)?;
    Ok(Some(EventFileCoordinates {
        group_key: conversation_id,
        relative_file_key,
    }))
}

fn openhands_error_as_capture(error: OpenHandsSourceBackedErrorV2) -> CaptureError {
    match error {
        OpenHandsSourceBackedErrorV2::Capture(error) => error,
        OpenHandsSourceBackedErrorV2::MissingConversationCoordinate { path } => {
            CaptureError::InvalidProviderTranscriptPath {
                path,
                reason: "OpenHands V1 event path has no conversation coordinate",
            }
        }
        OpenHandsSourceBackedErrorV2::InvalidRelativeEventKey { path } => {
            CaptureError::InvalidProviderTranscriptPath {
                path,
                reason: "OpenHands V1 event path has no bounded relative UTF-8 key",
            }
        }
        _ => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn validate_plan_for_group(
    group: EventFileGroup<'_>,
    plan: &OpenHandsEventFileSourcePlan,
) -> OpenHandsSourceBackedResultV2<()> {
    let expected_source = source_key(group.group_key())?;
    if group.group_key() != plan.conversation_id
        || !plan.source.exact_descriptor_eq(&expected_source)
        || plan.session_id != session_identity(&plan.source, &plan.conversation_id)?
    {
        return Err(OpenHandsSourceBackedErrorV2::InvalidRelativeEventKey {
            path: group
                .leaves()
                .first()
                .map(|leaf| leaf.display_path().to_path_buf())
                .unwrap_or_default(),
        });
    }
    let current = SourceObservation::new(
        plan.source.clone(),
        OPENHANDS_SOURCE_REVISION_KIND,
        group.observation_digest().to_vec(),
    )?;
    if current != plan.opening {
        return Err(EventFileInventoryError::SourceChanged {
            path: group
                .leaves()
                .first()
                .map(|leaf| leaf.display_path().to_path_buf())
                .unwrap_or_default(),
            detail: "OpenHands group observation changed before replacement staging".to_owned(),
        }
        .into());
    }
    Ok(())
}

pub(super) fn project_group(
    group: EventFileGroup<'_>,
    plan: &OpenHandsEventFileSourcePlan,
    mut emit: impl FnMut(LexicalDocument) -> OpenHandsSourceBackedResultV2<()>,
) -> OpenHandsSourceBackedResultV2<CertifiedSource> {
    validate_plan_for_group(group, plan)?;
    let mut event_ids = BTreeSet::new();
    let mut content_digest = Sha256::new();
    content_digest.update(OPENHANDS_CONVERSATION_CONTENT_DOMAIN);
    content_digest.update((group.leaves().len() as u64).to_be_bytes());
    let mut counts = ScannedSourceCounts::default();

    for (sequence, leaf) in group.leaves().iter().enumerate() {
        let provider_bytes = group.read_leaf(leaf)?;
        let record_digest: [u8; 32] = Sha256::digest(&provider_bytes).into();
        let legacy_observation = OpenHandsFileObservation::from_metadata(leaf.metadata())?;
        let leaf_revision = leaf_revision_digest(
            &leaf.coordinates().relative_file_key,
            &legacy_observation,
            record_digest,
        )?;
        digest_leaf(
            &mut content_digest,
            &leaf.coordinates().relative_file_key,
            &record_digest,
            legacy_observation.length,
        );
        counts.complete_records = checked_add(counts.complete_records, 1)?;
        counts.certified_bytes = checked_add(counts.certified_bytes, legacy_observation.length)?;

        match decode_openhands_event(leaf.display_path(), &provider_bytes) {
            Ok(decoded) => {
                if !event_ids.insert(decoded.event_id().to_owned()) {
                    return Err(OpenHandsSourceBackedErrorV2::DuplicateEventId {
                        conversation_id: plan.conversation_id.clone(),
                        event_id: decoded.event_id().to_owned(),
                    });
                }
                if let Some(body) = lexical_body(&decoded) {
                    let event_id =
                        event_identity(&plan.source, plan.session_id, decoded.event_id())?;
                    let locator = source_locator(
                        &plan.source,
                        &leaf.coordinates().relative_file_key,
                        decoded.event_id(),
                        leaf_revision,
                        record_digest,
                    )?;
                    emit(LexicalDocument {
                        event_id,
                        session_id: plan.session_id,
                        parent_session_id: None,
                        root_session_id: plan.session_id,
                        source: plan.source.clone(),
                        locator,
                        provider_session_id: Some(plan.conversation_id.clone()),
                        branch: None,
                        source_path: Some(openhands_checked_path_text(leaf.display_path())?),
                        agent_type: ctx_history_core::AgentType::Primary.as_str().to_owned(),
                        is_primary: true,
                        event_sequence: u64::try_from(sequence)
                            .map_err(|_| OpenHandsSourceBackedErrorV2::CountOverflow)?,
                        occurred_at_unix_ms: Some(decoded.timestamp().timestamp_millis()),
                        event_type: decoded.event_type().as_str().to_owned(),
                        role: Some(decoded.role().as_str().to_owned()),
                        body,
                        workspace: None,
                        cwd: None,
                        touched_files: touched_files(&decoded),
                    })?;
                    counts.retained_records = checked_add(counts.retained_records, 1)?;
                    counts.indexed_documents = checked_add(counts.indexed_documents, 1)?;
                } else {
                    counts.ignored_records = checked_add(counts.ignored_records, 1)?;
                }
            }
            Err(_) => {
                counts.rejected_records = checked_add(counts.rejected_records, 1)?;
            }
        }
    }
    group.revalidate()?;
    let certificate = CertifiedSource::certify(
        plan.opening.clone(),
        plan.opening.clone(),
        OPENHANDS_PARSER_REVISION,
        content_digest.finalize().into(),
        counts,
    )?;
    Ok(certificate)
}

fn lexical_body(decoded: &OpenHandsDecodedEvent) -> Option<String> {
    let event_type = decoded.event_type();
    let text = if matches!(
        event_type,
        ctx_history_core::EventType::ToolOutput | ctx_history_core::EventType::CommandOutput
    ) {
        let outcome = openhands_output_outcome(decoded);
        if !matches!(outcome, OutputOutcome::Failure | OutputOutcome::Timeout) {
            return None;
        }
        if decoded.text().trim().is_empty() {
            if outcome == OutputOutcome::Timeout {
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

fn openhands_output_outcome(decoded: &OpenHandsDecodedEvent) -> OutputOutcome {
    if openhands_value_indicates_timeout(decoded.value()) {
        return OutputOutcome::Timeout;
    }
    match provider_result_outcome_evidence(decoded.event_type(), decoded.value()).as_str() {
        Some("success") => OutputOutcome::Success,
        Some("failure") => OutputOutcome::Failure,
        _ => OutputOutcome::Unknown,
    }
}

fn openhands_value_indicates_timeout(value: &serde_json::Value) -> bool {
    const MAX_NODES: usize = 4_096;

    fn visit(value: &serde_json::Value, remaining: &mut usize) -> bool {
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        match value {
            serde_json::Value::Array(values) => values.iter().any(|value| visit(value, remaining)),
            serde_json::Value::Object(values) => values.iter().any(|(key, value)| {
                let normalized = key
                    .chars()
                    .filter(|character| character.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>();
                let direct = matches!(normalized.as_str(), "timeout" | "timedout" | "istimeout")
                    && (value.as_bool().unwrap_or(false)
                        || value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        }));
                direct || visit(value, remaining)
            }),
            serde_json::Value::String(value) => matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "timeout" | "timed_out" | "timedout"
            ),
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
                false
            }
        }
    }

    let mut remaining = MAX_NODES;
    visit(value, &mut remaining)
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

pub(super) fn source_key(conversation_id: &str) -> OpenHandsSourceBackedResultV2<SourceKey> {
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
) -> OpenHandsSourceBackedResultV2<StableEntityId> {
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
) -> OpenHandsSourceBackedResultV2<StableEntityId> {
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
) -> OpenHandsSourceBackedResultV2<(StableEntityId, StableEntityId)> {
    let session_id = session_identity(source, conversation_id)?;
    let event_id = event_identity(source, session_id, event_id)?;
    Ok((session_id, event_id))
}

pub(super) fn source_locator(
    source: &SourceKey,
    relative_file_key: &str,
    event_id: &str,
    leaf_revision: [u8; 32],
    record_digest: [u8; 32],
) -> OpenHandsSourceBackedResultV2<SourceRecordLocator> {
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

pub(super) fn leaf_revision_digest(
    relative_file_key: &str,
    observation: &OpenHandsFileObservation,
    content_digest: [u8; 32],
) -> OpenHandsSourceBackedResultV2<[u8; 32]> {
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

fn conversation_coordinate(path: &Path) -> OpenHandsSourceBackedResultV2<(String, PathBuf)> {
    let components = path.components().collect::<Vec<_>>();
    let Some(v1_index) = components
        .iter()
        .position(|component| component.as_os_str() == "v1_conversations")
    else {
        return Err(
            OpenHandsSourceBackedErrorV2::MissingConversationCoordinate {
                path: path.to_path_buf(),
            },
        );
    };
    let Some(conversation) = components.get(v1_index.saturating_add(1)) else {
        return Err(
            OpenHandsSourceBackedErrorV2::MissingConversationCoordinate {
                path: path.to_path_buf(),
            },
        );
    };
    let conversation_id = conversation
        .as_os_str()
        .to_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(
            || OpenHandsSourceBackedErrorV2::MissingConversationCoordinate {
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
) -> OpenHandsSourceBackedResultV2<String> {
    let relative = path.strip_prefix(conversation_root).map_err(|_| {
        OpenHandsSourceBackedErrorV2::InvalidRelativeEventKey {
            path: path.to_path_buf(),
        }
    })?;
    relative_path_key(relative, path)
}

fn relative_path_key(relative: &Path, original: &Path) -> OpenHandsSourceBackedResultV2<String> {
    let parts = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_owned),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| OpenHandsSourceBackedErrorV2::InvalidRelativeEventKey {
            path: original.to_path_buf(),
        })?;
    if parts.is_empty() {
        return Err(OpenHandsSourceBackedErrorV2::InvalidRelativeEventKey {
            path: original.to_path_buf(),
        });
    }
    Ok(parts.join("/"))
}

fn checked_add(left: u64, right: u64) -> OpenHandsSourceBackedResultV2<u64> {
    left.checked_add(right)
        .ok_or(OpenHandsSourceBackedErrorV2::CountOverflow)
}
