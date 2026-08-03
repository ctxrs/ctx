use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, StableEntityId, TypedKey,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::provider::source_backed::family::document::{
    ChangedDocumentSink, CompleteDocumentTree, DocumentLeafExecutionPolicy,
    DocumentLeafFingerprint, DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
};
use crate::{
    provider::{
        normalization::provider_result_outcome_evidence,
        source_backed::{
            SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDraft,
            SourceBackedRecordRejectionDrafts, SourceBackedRouteError, SourceBackedRouteErrorKind,
            SourceBackedRouteResult,
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
        openhands_json_path_is_event, OPENHANDS_MAX_PATH_BYTES,
    },
};

mod detection;

use detection::detects_current_cli_format;

const OPENHANDS_SOURCE_ANCHOR_NAMESPACE: &str = "openhands.v1-conversation";
const OPENHANDS_NATIVE_SESSION_NAMESPACE: &str = "openhands.v1-conversation";
const OPENHANDS_NATIVE_EVENT_NAMESPACE: &str = "openhands.v1-event";
const OPENHANDS_LOGICAL_SESSION_KIND: &str = "openhands-conversation";
const OPENHANDS_LOGICAL_EVENT_KIND: &str = "openhands-event";
const OPENHANDS_SOURCE_SCHEMA_VARIANT: &str = "openhands-v1-conversation-tree-v1";
const OPENHANDS_SOURCE_REVISION_KIND: &str = "openhands-v1-conversation-leaves-v2";
const OPENHANDS_PARSER_REVISION: &str = "openhands-source-backed-v4";
const OPENHANDS_CONVERSATION_CONTENT_DOMAIN: &[u8] = b"ctx.openhands.conversation-content.v1\0";
const OPENHANDS_DOCUMENT_LEAF_DOMAIN: &[u8] = b"ctx.openhands.document-leaf.v1\0";
const OPENHANDS_DISCOVERY_MAX_DEPTH: usize = 16;
const OPENHANDS_DISCOVERY_MAX_ENTRIES: usize = 16_384;

#[derive(Debug, Error)]
pub(crate) enum OpenHandsSourceBackedErrorV2 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    EventFiles(#[from] EventFileInventoryError),
    #[error(transparent)]
    Projection(#[from] ctx_history_core::ProjectionContractError),
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
    #[error("OpenHands event record was rejected: {0}")]
    RecordRejected(String),
}

pub(crate) type OpenHandsSourceBackedResultV2<T> = Result<T, OpenHandsSourceBackedErrorV2>;

#[derive(Debug, Clone)]
pub(crate) struct OpenHandsEventFileAdapterV2 {
    selected: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenHandsEventFileSourcePlan {
    group_ordinal: usize,
    pub source: SourceKey,
    pub conversation_id: String,
    pub session_id: StableEntityId,
    pub opening: SourceObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpenHandsEventFileLeafJob {
    group_ordinal: usize,
    leaf_ordinal: usize,
}

impl OpenHandsEventFileLeafJob {
    pub(crate) fn group_ordinal(&self) -> usize {
        self.group_ordinal
    }

    pub(crate) fn leaf_ordinal(&self) -> usize {
        self.leaf_ordinal
    }
}

pub(crate) struct OpenHandsEventFileLeafProjection {
    job: OpenHandsEventFileLeafJob,
    relative_file_key: String,
    native_event_id: Option<String>,
    record_digest: [u8; 32],
    certified_bytes: u64,
    record: Option<CoreRecord>,
    rejection: Option<SourceBackedRecordRejectionDraft>,
}

impl OpenHandsEventFileSourcePlan {
    pub(crate) fn group_ordinal(&self) -> usize {
        self.group_ordinal
    }
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
            Err(EventFileInventoryError::DuplicateGroupInstance { group_key }) => {
                return Err(OpenHandsSourceBackedErrorV2::DuplicateConversationId(
                    group_key,
                ));
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
        let conversation_id = group.group_key().to_owned();
        let source = source_key(&conversation_id)?;
        let session_id = session_identity(&source, &conversation_id)?;
        let opening = SourceObservation::new(
            source.clone(),
            OPENHANDS_SOURCE_REVISION_KIND,
            group.observation_digest().to_vec(),
        )?;
        Ok(OpenHandsEventFileSourcePlan {
            group_ordinal: group.ordinal(),
            source,
            conversation_id,
            session_id,
            opening,
        })
    }
}

impl ReplacementDocumentTree for OpenHandsEventFileAdapterV2 {
    type Leaf = OpenHandsEventFileSourcePlan;
    type TreeAuthority = std::sync::Arc<EventFileInventory>;

    fn parser_revision(&self) -> &'static str {
        OPENHANDS_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        openhands_owns_source(source)
    }

    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        DocumentLeafExecutionPolicy::Independent
    }

    fn independent_leaf_source(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<SourceKey> {
        Ok(leaf.source.clone())
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let inventory = std::sync::Arc::new(self.open_inventory().map_err(openhands_route_error)?);
        let plans = inventory
            .groups()
            .map(|group| {
                let plan = self.bind_group(group).map_err(openhands_route_error)?;
                let mut digest = Sha256::new();
                digest.update(OPENHANDS_DOCUMENT_LEAF_DOMAIN);
                digest.update(plan.source.exact_descriptor_digest());
                digest.update(group.observation_digest());
                Ok(ObservedDocumentLeaf::new(
                    DocumentLeafFingerprint::new(digest.finalize().into()),
                    plan,
                ))
            })
            .collect::<SourceBackedRouteResult<Vec<_>>>()?;
        Ok(CompleteDocumentTree::new(
            inventory.observation_digest(),
            plans,
            inventory,
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let group = authority.group_at(leaf.group_ordinal()).ok_or_else(|| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::SourceChanged,
                "OpenHands group ordinal disappeared after complete discovery",
            )
        })?;
        sink.begin_source(leaf.source.clone())?;
        let (certificate, rejections) = project_group_with_emit(group, leaf, |record| {
            if let Some(record) = record {
                sink.emit_core_record(record)?;
            }
            Ok::<_, SourceBackedRouteError>(())
        })
        .map_err(|error| match error {
            ProjectGroupError::Provider(error) => openhands_route_error(error),
            ProjectGroupError::Emit(error) => error,
        })?;
        let all_records_rejected = certificate.counts().retained_records == 0
            && certificate.counts().ignored_records == 0
            && certificate.counts().rejected_records != 0;
        let rejection_detail = rejections
            .first()
            .map(|rejection| {
                format!(
                    "OpenHands conversation has no usable records after rejecting {}: {}",
                    rejection.source_selector, rejection.detail
                )
            })
            .unwrap_or_else(|| {
                "OpenHands conversation has no usable records after record rejection".to_owned()
            });
        sink.record_rejections(rejections);
        if all_records_rejected {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::InvalidSource,
                rejection_detail,
            ));
        }
        Ok(DocumentSourceTerminal {
            source: leaf.source.clone(),
            opening: certificate.observation().clone(),
            closing: certificate.observation().clone(),
            parser_revision: OPENHANDS_PARSER_REVISION,
            content_digest: *certificate.content_digest(),
            counts: certificate.counts(),
        })
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        tree.authority
            .revalidate_all()
            .map_err(OpenHandsSourceBackedErrorV2::from)
            .map_err(openhands_route_error)?;
        Ok(tree.authority.observation_digest())
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
        group_instance_key: openhands_checked_path_text(&conversation_root)?,
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
    if group.ordinal() != plan.group_ordinal
        || group.group_key() != plan.conversation_id
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
    if !plan.opening.source().exact_descriptor_eq(&plan.source)
        || plan.opening.revision_kind() != OPENHANDS_SOURCE_REVISION_KIND
        || plan.opening.revision() != group.observation_digest()
    {
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

#[cfg(test)]
pub(super) fn project_group(
    group: EventFileGroup<'_>,
    plan: &OpenHandsEventFileSourcePlan,
    mut emit: impl FnMut(CoreRecord) -> OpenHandsSourceBackedResultV2<()>,
) -> OpenHandsSourceBackedResultV2<CertifiedSource> {
    match project_group_with_emit(group, plan, |document| match document {
        Some(document) => emit(document),
        None => Ok(()),
    }) {
        Ok((certificate, _)) => Ok(certificate),
        Err(ProjectGroupError::Provider(error) | ProjectGroupError::Emit(error)) => Err(error),
    }
}

enum ProjectGroupError<E> {
    Provider(OpenHandsSourceBackedErrorV2),
    Emit(E),
}

fn project_group_with_emit<E>(
    group: EventFileGroup<'_>,
    plan: &OpenHandsEventFileSourcePlan,
    mut checkpoint: impl FnMut(Option<CoreRecord>) -> Result<(), E>,
) -> Result<(CertifiedSource, SourceBackedRecordRejectionDrafts), ProjectGroupError<E>> {
    let jobs = projection_jobs(group, plan).map_err(ProjectGroupError::Provider)?;
    let mut event_ids = BTreeSet::new();
    let mut content_digest = Sha256::new();
    content_digest.update(OPENHANDS_CONVERSATION_CONTENT_DOMAIN);
    content_digest.update((group.leaves().len() as u64).to_be_bytes());
    let mut counts = ScannedSourceCounts::default();
    let mut record_rejections = SourceBackedRecordRejectionDrafts::default();

    for job in jobs {
        checkpoint(None).map_err(ProjectGroupError::Emit)?;
        let projected = project_leaf_job(group, plan, job).map_err(ProjectGroupError::Provider)?;
        let expected_leaf_ordinal = usize::try_from(counts.complete_records).map_err(|_| {
            ProjectGroupError::Provider(OpenHandsSourceBackedErrorV2::CountOverflow)
        })?;
        if projected.job.group_ordinal() != group.ordinal()
            || projected.job.leaf_ordinal() != expected_leaf_ordinal
        {
            return Err(ProjectGroupError::Provider(
                EventFileInventoryError::SourceChanged {
                    path: group
                        .leaf_at(expected_leaf_ordinal)
                        .map(|leaf| leaf.display_path().to_path_buf())
                        .unwrap_or_default(),
                    detail: "OpenHands projection results are not in deterministic leaf order"
                        .to_owned(),
                }
                .into(),
            ));
        }
        digest_leaf(
            &mut content_digest,
            &projected.relative_file_key,
            &projected.record_digest,
            projected.certified_bytes,
        );
        counts.complete_records =
            checked_add(counts.complete_records, 1).map_err(ProjectGroupError::Provider)?;
        counts.certified_bytes = checked_add(counts.certified_bytes, projected.certified_bytes)
            .map_err(ProjectGroupError::Provider)?;
        if let Some(native_event_id) = projected.native_event_id.as_ref() {
            if !event_ids.insert(native_event_id.clone()) {
                return Err(ProjectGroupError::Provider(
                    OpenHandsSourceBackedErrorV2::DuplicateEventId {
                        conversation_id: plan.conversation_id.clone(),
                        event_id: native_event_id.clone(),
                    },
                ));
            }
        }
        if let Some(rejection) = projected.rejection {
            record_rejections.record(rejection);
            counts.rejected_records =
                checked_add(counts.rejected_records, 1).map_err(ProjectGroupError::Provider)?;
        } else if let Some(record) = projected.record {
            checkpoint(Some(record)).map_err(ProjectGroupError::Emit)?;
            counts.retained_records =
                checked_add(counts.retained_records, 1).map_err(ProjectGroupError::Provider)?;
            counts.indexed_documents =
                checked_add(counts.indexed_documents, 1).map_err(ProjectGroupError::Provider)?;
        } else {
            counts.ignored_records =
                checked_add(counts.ignored_records, 1).map_err(ProjectGroupError::Provider)?;
        }
    }
    let certificate = CertifiedSource::certify(
        plan.opening.clone(),
        plan.opening.clone(),
        OPENHANDS_PARSER_REVISION,
        content_digest.finalize().into(),
        counts,
    )
    .map_err(OpenHandsSourceBackedErrorV2::from)
    .map_err(ProjectGroupError::Provider)?;
    Ok((certificate, record_rejections))
}

pub(crate) fn projection_jobs(
    group: EventFileGroup<'_>,
    plan: &OpenHandsEventFileSourcePlan,
) -> OpenHandsSourceBackedResultV2<Vec<OpenHandsEventFileLeafJob>> {
    validate_plan_for_group(group, plan)?;
    Ok((0..group.leaves().len())
        .map(|leaf_ordinal| OpenHandsEventFileLeafJob {
            group_ordinal: group.ordinal(),
            leaf_ordinal,
        })
        .collect())
}

pub(crate) fn project_leaf_job(
    group: EventFileGroup<'_>,
    plan: &OpenHandsEventFileSourcePlan,
    job: OpenHandsEventFileLeafJob,
) -> OpenHandsSourceBackedResultV2<OpenHandsEventFileLeafProjection> {
    let leaf = group
        .leaf_at(job.leaf_ordinal)
        .ok_or_else(|| EventFileInventoryError::MissingGroup(plan.conversation_id.clone()))?;
    if group.ordinal() != job.group_ordinal
        || plan.group_ordinal != job.group_ordinal
        || leaf.group_ordinal() != job.group_ordinal
        || leaf.leaf_ordinal() != job.leaf_ordinal
    {
        return Err(EventFileInventoryError::SourceChanged {
            path: leaf.display_path().to_path_buf(),
            detail: "OpenHands projection job ordinal no longer matches its inventory".to_owned(),
        }
        .into());
    }
    let provider_bytes = group.read_leaf_at(job.leaf_ordinal)?;
    let record_digest: [u8; 32] = Sha256::digest(&provider_bytes).into();
    let relative_file_key = leaf.coordinates().relative_file_key.clone();
    let decoded = match decode_openhands_event(leaf.display_path(), &provider_bytes) {
        Ok(decoded) => decoded,
        Err(error) => {
            return Ok(OpenHandsEventFileLeafProjection {
                job,
                relative_file_key,
                native_event_id: None,
                record_digest,
                certified_bytes: leaf.metadata().len(),
                record: None,
                rejection: Some(SourceBackedRecordRejectionDraft {
                    source: plan.source.clone(),
                    provider: CaptureProvider::OpenHands,
                    source_selector: leaf.display_path().display().to_string(),
                    line_number: 1,
                    payload_type: None,
                    class: SourceBackedRecordRejectionClass::MalformedRecord,
                    detail: error.to_string(),
                }),
            });
        }
    };
    let native_event_id = Some(decoded.event_id().to_owned());
    let record = lexical_body(&decoded)
        .map(|body| {
            let mut record = CoreRecord::new_selected(
                event_identity(&plan.source, plan.session_id, decoded.event_id())?,
                plan.session_id,
                plan.session_id,
                plan.source.clone(),
                u64::try_from(job.leaf_ordinal)
                    .map_err(|_| OpenHandsSourceBackedErrorV2::CountOverflow)?,
                decoded.event_type().as_str(),
                ctx_history_core::AgentType::Primary.as_str(),
                true,
                OPENHANDS_PARSER_REVISION,
                body,
            )
            .map_err(core_contract)?;
            record.provider_session_id = Some(plan.conversation_id.clone());
            record.native_event_id = Some(TypedKey::utf8(decoded.event_id())?);
            record.occurred_at_unix_ms = Some(decoded.timestamp().timestamp_millis());
            record.role = Some(decoded.role().as_str().to_owned());
            if matches!(
                decoded.event_type(),
                ctx_history_core::EventType::ToolOutput
                    | ctx_history_core::EventType::CommandOutput
                    | ctx_history_core::EventType::FileTouched
            ) {
                record.content.structured_content = Some(openhands_output_linkage(&decoded)?);
            }
            record.validate_contract().map_err(core_contract)?;
            Ok::<CoreRecord, OpenHandsSourceBackedErrorV2>(record)
        })
        .transpose();
    let (record, rejection) = match record {
        Ok(record) => (record, None),
        Err(error) => (
            None,
            Some(SourceBackedRecordRejectionDraft {
                source: plan.source.clone(),
                provider: CaptureProvider::OpenHands,
                source_selector: leaf.display_path().display().to_string(),
                line_number: 1,
                payload_type: Some(decoded.event_type().as_str().to_owned()),
                class: SourceBackedRecordRejectionClass::UnsupportedRecord,
                detail: error.to_string(),
            }),
        ),
    };
    Ok(OpenHandsEventFileLeafProjection {
        job,
        relative_file_key,
        native_event_id,
        record_digest,
        certified_bytes: leaf.metadata().len(),
        record,
        rejection,
    })
}

fn lexical_body(decoded: &OpenHandsDecodedEvent) -> Option<String> {
    let text = decoded.text().to_owned();
    (!text.trim().is_empty()).then_some(text)
}

fn openhands_output_linkage(
    decoded: &OpenHandsDecodedEvent,
) -> OpenHandsSourceBackedResultV2<serde_json::Value> {
    let value = decoded.value();
    let observation = value
        .get("observation")
        .and_then(serde_json::Value::as_object);
    let unique_text = |fields: &[&str]| -> OpenHandsSourceBackedResultV2<Option<String>> {
        let mut selected = None;
        for field in fields {
            let candidate = value
                .get(*field)
                .or_else(|| observation.and_then(|object| object.get(*field)))
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty());
            let Some(candidate) = candidate else {
                continue;
            };
            if candidate.len() > 16 * 1024 {
                return Err(OpenHandsSourceBackedErrorV2::RecordRejected(format!(
                    "OpenHands {field} exceeds the bounded linkage limit"
                )));
            }
            if selected
                .as_deref()
                .is_some_and(|existing| existing != candidate)
            {
                return Err(OpenHandsSourceBackedErrorV2::RecordRejected(format!(
                    "OpenHands result exposes conflicting {field} linkage"
                )));
            }
            selected = Some(candidate.to_owned());
        }
        Ok(selected)
    };
    let outcome = match openhands_output_outcome(decoded) {
        OutputOutcome::Success => "success",
        OutputOutcome::Failure => "failure",
        OutputOutcome::Timeout => "timeout",
        OutputOutcome::Unknown => "unknown",
    };
    let observation_kind = observation
        .and_then(|object| object.get("kind"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    if observation_kind
        .as_ref()
        .is_some_and(|value| value.len() > 16 * 1024)
    {
        return Err(OpenHandsSourceBackedErrorV2::RecordRejected(
            "OpenHands observation kind exceeds the bounded linkage limit".to_owned(),
        ));
    }
    let tool_name = unique_text(&["tool_name", "name"])?.or(observation_kind);
    Ok(serde_json::json!({
        "provider_native_tool_result": {
            "call_id": unique_text(&["tool_call_id", "call_id"] )?,
            "action_id": unique_text(&["action_id"] )?,
            "tool_name": tool_name,
            "outcome": outcome,
        }
    }))
}

fn openhands_output_outcome(decoded: &OpenHandsDecodedEvent) -> OutputOutcome {
    if openhands_value_indicates_timeout(decoded.value()) {
        return OutputOutcome::Timeout;
    }
    // File-editor observations are result records even though their structured
    // path evidence gives them the more specific FileTouched event type.
    let result_event_type = if decoded.event_type() == ctx_history_core::EventType::FileTouched {
        ctx_history_core::EventType::ToolOutput
    } else {
        decoded.event_type()
    };
    match provider_result_outcome_evidence(result_event_type, decoded.value()).as_str() {
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

fn core_contract(error: impl std::fmt::Display) -> OpenHandsSourceBackedErrorV2 {
    OpenHandsSourceBackedErrorV2::Capture(CaptureError::InvalidPayload(error.to_string()))
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
