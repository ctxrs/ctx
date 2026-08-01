//! Thin Continue adapter for the shared replacement-document lifecycle.

use std::path::PathBuf;

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, CoreRecordError,
    EventIdentityInput, EventType, NativeItemKey, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation,
    StableEntityId, TypedKey,
};
use serde::Serialize;
use thiserror::Error;

use super::{
    normalize::{
        ContinuePreparedPage, ContinuePreparedSource, CONTINUE_NATIVE_MAX_PAGE_BYTES,
        CONTINUE_NATIVE_MAX_PAGE_ROWS,
    },
    parse::{parse_continue_source, ContinueParseOutcome, ContinueSourcePageStream},
    source::{discover_continue_root, ContinueDocumentLeaf, ContinueTreeAuthority},
    ContinueEventKind, ContinueEventRole, ContinueEventRow, ContinueGenerationAuthority,
    ContinueNativePathError,
};
use crate::{
    provider::source_backed::{
        document_leaf_execution_policy,
        family::document::{
            register_replacement_document_tree_route, ChangedDocumentSink, CompleteDocumentTree,
            DocumentLeafExecutionPolicy, DocumentLeafFingerprint, DocumentSourceTerminal,
            ObservedDocumentLeaf, ReplacementDocumentTree,
        },
        route_error, SourceBackedCoordinatorResult, SourceBackedProviderRegistry,
        SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
        SourceBackedRouteSelection,
    },
    ProviderSource, CONTINUE_CLI_SOURCE_FORMAT,
};

const CONTINUE_SOURCE_ANCHOR_NAMESPACE: &str = "continue.session";
const CONTINUE_NATIVE_SESSION_NAMESPACE: &str = "continue.session";
const CONTINUE_NATIVE_EVENT_NAMESPACE: &str = "continue.history-item";
const CONTINUE_NATIVE_EVENT_POSITION_KIND: &str = "continue.history-ordinal";
const CONTINUE_LOGICAL_SESSION_KIND: &str = "continue-session";
const CONTINUE_LOGICAL_EVENT_KIND: &str = "continue-event";
const CONTINUE_SOURCE_SCHEMA_VARIANT: &str = "continue-nativepath-document-v0";
const CONTINUE_SOURCE_REVISION_KIND: &str = "continue-whole-document-observation-v0";
pub(crate) const CONTINUE_SOURCE_BACKED_PARSER_REVISION: &str =
    "continue-nativepath-source-backed-v0";
const MAX_CONTINUE_LEXICAL_METADATA_CHARS: usize = 8 * 1024;

#[derive(Debug, Error)]
pub(crate) enum ContinueSourceBackedError {
    #[error(transparent)]
    Native(#[from] ContinueNativePathError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Continue source-backed page stream changed its active session")]
    SessionChanged,
    #[error("Continue source-backed page stream is missing source/session authority")]
    MissingSourceAuthority,
    #[error("Continue source-backed page stream started a source before completing the prior one")]
    OverlappingSource,
    #[error("Continue source-backed page stream ended before terminal source authority")]
    UnterminatedSource,
    #[error("Continue source-backed terminal counts do not reconcile")]
    CountMismatch,
    #[error("Continue source-backed count or ordinal overflowed")]
    CountOverflow,
    #[error("Continue source revision evidence is malformed")]
    InvalidRevisionEvidence,
    #[error("Continue direct Core projection failed: {0}")]
    ProjectionFailure(String),
}

pub(crate) type ContinueSourceBackedResult<T> = Result<T, ContinueSourceBackedError>;

/// Registration result retained at the shared registry import seam.
pub(crate) type ContinueSourceBackedOutcome = SourceBackedCoordinatorResult<()>;

/// Provider state for one canonical replacement-document route.
pub(crate) struct ContinueSourceBackedReader {
    root: PathBuf,
}

impl ContinueSourceBackedReader {
    fn explicit(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn register(
        registry: &mut SourceBackedProviderRegistry,
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
    ) -> ContinueSourceBackedOutcome {
        let adapter = Self::explicit(source.path.clone());
        register_replacement_document_tree_route(registry, source, selection, adapter)
    }
}

impl ReplacementDocumentTree for ContinueSourceBackedReader {
    type Leaf = ContinueDocumentLeaf;
    type TreeAuthority = ContinueTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        CONTINUE_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        owns_continue_source(source)
    }

    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        document_leaf_execution_policy(CaptureProvider::Continue)
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let discovery = discover_continue_root(&self.root).map_err(route_error)?;
        let (leaves, authority) = discovery.into_parts();
        let tree_fingerprint = authority.tree_fingerprint();
        let leaves = leaves
            .into_iter()
            .map(|leaf| {
                ObservedDocumentLeaf::new(
                    DocumentLeafFingerprint::new(authority.leaf_fingerprint(&leaf)),
                    leaf,
                )
            })
            .collect();
        Ok(CompleteDocumentTree::new(
            tree_fingerprint,
            leaves,
            authority,
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let snapshot = authority.open_source(leaf).map_err(route_error)?;
        scan_continue_document(snapshot, authority, sink)
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        tree.authority
            .revalidate_fingerprint()
            .map_err(route_error)?
            .ok_or_else(|| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Continue document inventory changed before terminal certification",
                )
            })
    }
}

fn scan_continue_document(
    snapshot: super::source::ContinueSourceSnapshot,
    authority: &ContinueTreeAuthority,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> SourceBackedRouteResult<DocumentSourceTerminal> {
    let mut stream = match parse_continue_source(snapshot, authority.index()) {
        Ok(ContinueParseOutcome::Complete(stream)) => stream,
        Ok(ContinueParseOutcome::Incomplete) => {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                "Continue selected source was incomplete",
            ));
        }
        Err(failure) => {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                failure.message,
            ));
        }
    };
    project_changed_stream(&mut stream, sink).map_err(route_error)
}

fn project_changed_stream(
    stream: &mut ContinueSourcePageStream,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> ContinueSourceBackedResult<DocumentSourceTerminal> {
    let mut active = None;
    let mut terminal = None;
    while let Some(page) = stream.next_page().map_err(|failure| {
        ContinueSourceBackedError::ProjectionFailure(failure.message.into_string())
    })? {
        if terminal.is_some() {
            return Err(ContinueSourceBackedError::CountMismatch);
        }
        terminal = project_changed_page(&mut active, page, sink)?;
    }
    if active.is_some() {
        return Err(ContinueSourceBackedError::UnterminatedSource);
    }
    terminal.ok_or(ContinueSourceBackedError::UnterminatedSource)
}

fn project_changed_page(
    active: &mut Option<ActiveSource>,
    mut page: ContinuePreparedPage,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> ContinueSourceBackedResult<Option<DocumentSourceTerminal>> {
    if let Some(prepared) = page.source.take() {
        if active.is_some() {
            return Err(ContinueSourceBackedError::OverlappingSource);
        }
        let started = start_source(*prepared)?;
        sink.begin_source(started.source.clone())
            .map_err(|error| ContinueSourceBackedError::ProjectionFailure(error.to_string()))?;
        *active = Some(started);
    }
    let current = active
        .as_mut()
        .ok_or(ContinueSourceBackedError::MissingSourceAuthority)?;
    if current.prepared.session.identity != page.session_identity {
        return Err(ContinueSourceBackedError::SessionChanged);
    }
    for event in page.events {
        if event.identity.history_ordinal < current.next_history_ordinal {
            return Err(ContinueSourceBackedError::CountMismatch);
        }
        current.next_history_ordinal = event
            .identity
            .history_ordinal
            .checked_add(1)
            .ok_or(ContinueSourceBackedError::CountOverflow)?;
        let document = project_event(current, event)?;
        sink.emit_core_record(document)
            .map_err(|error| ContinueSourceBackedError::ProjectionFailure(error.to_string()))?;
        current.emitted_documents = current
            .emitted_documents
            .checked_add(1)
            .ok_or(ContinueSourceBackedError::CountOverflow)?;
    }
    if page.estimated_bytes > CONTINUE_NATIVE_MAX_PAGE_BYTES
        || page.row_count > CONTINUE_NATIVE_MAX_PAGE_ROWS
    {
        return Err(ContinueSourceBackedError::CountMismatch);
    }
    if !page.terminal {
        if page.authority.is_some() || page.output_exclusion.is_some() {
            return Err(ContinueSourceBackedError::CountMismatch);
        }
        return Ok(None);
    }
    let generation = page
        .authority
        .take()
        .ok_or(ContinueSourceBackedError::CountMismatch)?;
    page.output_exclusion
        .take()
        .ok_or(ContinueSourceBackedError::CountMismatch)?;
    let active = active
        .take()
        .ok_or(ContinueSourceBackedError::MissingSourceAuthority)?;
    finish_source(active, generation).map(Some)
}

struct ActiveSource {
    source: SourceKey,
    session_id: StableEntityId,
    prepared: ContinuePreparedSource,
    source_revision_digest: [u8; 32],
    next_history_ordinal: u64,
    emitted_documents: u64,
}

fn start_source(prepared: ContinuePreparedSource) -> ContinueSourceBackedResult<ActiveSource> {
    let native_session_id = prepared.session.identity.0.as_str();
    let source = continue_source_key(native_session_id)?;
    let session_id = continue_session_id(&source, native_session_id)?;
    let source_revision_digest = decode_hex_digest(prepared.observation.session_revision())
        .ok_or(ContinueSourceBackedError::InvalidRevisionEvidence)?;
    Ok(ActiveSource {
        source,
        session_id,
        prepared,
        source_revision_digest,
        next_history_ordinal: 0,
        emitted_documents: 0,
    })
}

fn project_event(
    active: &ActiveSource,
    event: ContinueEventRow,
) -> ContinueSourceBackedResult<CoreRecord> {
    let native_item_id = event.native_item_id.as_deref().filter(|value| {
        !value.trim().is_empty() && value.len() <= 384 && !value.chars().any(char::is_control)
    });
    let native_item_key = if let Some(native_item_id) = native_item_id {
        NativeItemKey::native_id(
            CONTINUE_NATIVE_EVENT_NAMESPACE,
            TypedKey::utf8(native_item_id)?,
        )?
    } else {
        NativeItemKey::revision_scoped_position(
            CONTINUE_NATIVE_EVENT_POSITION_KIND,
            TypedKey::U64(event.identity.history_ordinal),
            TypedKey::bytes(active.source_revision_digest.to_vec())?,
        )?
    };
    let event_id = derive_event_id(EventIdentityInput {
        source: &active.source,
        session_id: active.session_id,
        logical_item_kind: CONTINUE_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8(&active.prepared.session.identity.0)?,
        TypedKey::U64(event.identity.history_ordinal),
    ])?;
    let body = continue_lexical_body(&event);
    if body.is_empty() {
        return Err(ContinueSourceBackedError::CountMismatch);
    }
    let occurred_at_unix_ms = event
        .occurred_at
        .or(active.prepared.session.started_at)
        .map(|timestamp| timestamp.timestamp_millis());
    let workspace = active
        .prepared
        .session
        .workspace_directory
        .as_deref()
        .map(|value| bounded_chars(value, MAX_CONTINUE_LEXICAL_METADATA_CHARS));
    let event_type = match event.kind {
        ContinueEventKind::Message => EventType::Message.as_str(),
        ContinueEventKind::ToolCall => EventType::ToolCall.as_str(),
    };
    let role = match event.role {
        ContinueEventRole::User => "user",
        ContinueEventRole::Assistant => "assistant",
        ContinueEventRole::System => "system",
        ContinueEventRole::Tool => "tool",
        ContinueEventRole::Unknown => "unknown",
    };
    let native_file_touches = (!event.file_touches.is_empty())
        .then(|| serde_json::to_value(&event.file_touches))
        .transpose()?;
    let mut record = CoreRecord::new_selected(
        event_id,
        active.session_id,
        active.session_id,
        active.source.clone(),
        event.identity.history_ordinal,
        event_type,
        AgentType::Primary.as_str(),
        true,
        CONTINUE_SOURCE_BACKED_PARSER_REVISION,
        body,
    )?;
    record.provider_session_id = Some(active.prepared.session.identity.0.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = occurred_at_unix_ms;
    record.role = Some(role.to_owned());
    record.workspace = workspace.clone();
    record.cwd = workspace;
    if let Some(native_file_touches) = native_file_touches {
        record.metadata.insert(
            "provider_native_file_touches".to_owned(),
            native_file_touches,
        );
    }
    record.validate_contract()?;
    Ok(record)
}

fn finish_source(
    active: ActiveSource,
    authority: ContinueGenerationAuthority,
) -> ContinueSourceBackedResult<DocumentSourceTerminal> {
    if authority.retained_events
        != usize::try_from(active.emitted_documents)
            .map_err(|_| ContinueSourceBackedError::CountOverflow)?
    {
        return Err(ContinueSourceBackedError::CountMismatch);
    }
    let observed = authority
        .observed_history_items
        .ok_or(ContinueSourceBackedError::CountMismatch)?;
    if authority
        .retained_events
        .checked_add(authority.rejected_items)
        != Some(observed)
    {
        return Err(ContinueSourceBackedError::CountMismatch);
    }
    let counts = ScannedSourceCounts {
        complete_records: u64::try_from(observed)
            .map_err(|_| ContinueSourceBackedError::CountOverflow)?,
        retained_records: u64::try_from(authority.retained_events)
            .map_err(|_| ContinueSourceBackedError::CountOverflow)?,
        rejected_records: u64::try_from(authority.rejected_items)
            .map_err(|_| ContinueSourceBackedError::CountOverflow)?,
        ignored_records: 0,
        indexed_documents: active.emitted_documents,
        certified_bytes: active.prepared.observation.raw_bytes(),
    };
    let observation = source_observation(
        &active.source,
        active.source_revision_digest,
        active.prepared.index_dependency.dependency_revision(),
    )?;
    Ok(DocumentSourceTerminal {
        source: active.source,
        opening: observation.clone(),
        closing: observation,
        parser_revision: CONTINUE_SOURCE_BACKED_PARSER_REVISION,
        content_digest: active.source_revision_digest,
        counts,
    })
}

#[derive(Serialize)]
struct ContinueRevisionEvidence<'a> {
    session_revision_sha256: [u8; 32],
    index_dependency_revision: &'a str,
}

fn source_observation(
    source: &SourceKey,
    session_revision_sha256: [u8; 32],
    index_dependency_revision: &str,
) -> ContinueSourceBackedResult<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        CONTINUE_SOURCE_REVISION_KIND,
        serde_json::to_vec(&ContinueRevisionEvidence {
            session_revision_sha256,
            index_dependency_revision,
        })?,
    )?)
}

fn owns_continue_source(source: &SourceKey) -> bool {
    source.provider() == CaptureProvider::Continue.as_str()
        && source.source_format() == CONTINUE_CLI_SOURCE_FORMAT
        && source.schema_variant() == CONTINUE_SOURCE_SCHEMA_VARIANT
        && source.provider_identity_version() == 1
}

fn continue_source_key(native_session_id: &str) -> ContinueSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        CONTINUE_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Continue.as_str(),
        CONTINUE_CLI_SOURCE_FORMAT,
        CONTINUE_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn continue_session_id(
    source: &SourceKey,
    native_session_id: &str,
) -> ContinueSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        CONTINUE_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: CONTINUE_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn continue_lexical_body(event: &ContinueEventRow) -> String {
    if !event.search_text.trim().is_empty() {
        return event.search_text.clone();
    }
    match event.kind {
        ContinueEventKind::Message => "Continue message".to_owned(),
        ContinueEventKind::ToolCall => "Continue tool call".to_owned(),
    }
}

fn bounded_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn decode_hex_digest(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut output = [0_u8; 32];
    for (index, slot) in output.iter_mut().enumerate() {
        let offset = index.checked_mul(2)?;
        *slot = u8::from_str_radix(value.get(offset..offset + 2)?, 16).ok()?;
    }
    Some(output)
}

#[cfg(test)]
#[path = "source_backed_tests.rs"]
mod tests;
