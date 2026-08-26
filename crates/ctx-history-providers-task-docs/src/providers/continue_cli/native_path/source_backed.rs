//! Thin Continue adapter for the shared replacement-document lifecycle.

use std::path::PathBuf;

use ctx_history_core::{
    derive_event_id, derive_session_id, ActivityInvocation, AgentScope, CaptureProvider,
    CoreActivity, CoreRecord, CoreRecordError, EventIdentityInput, EventType, LiteralFactKind,
    NativeItemKey, NativeSessionKey, ProjectionContractError, ProviderDeclaredFact,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchorScope, SourceKey, SourceObservation,
    StableEntityId, TypedKey, CORE_ACTIVITY_REVISION,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    normalize::{
        ContinueCallRelationship, ContinuePreparedPage, ContinuePreparedSource,
        CONTINUE_NATIVE_MAX_PAGE_BYTES, CONTINUE_NATIVE_MAX_PAGE_ROWS,
    },
    parse::{parse_continue_source, ContinueParseOutcome, ContinueSourcePageStream},
    source::{discover_continue_root, ContinueDocumentLeaf, ContinueTreeAuthority},
    ContinueEventKind, ContinueEventRole, ContinueEventRow, ContinueGenerationAuthority,
    ContinueNativePathError,
};
use crate::{
    route_error, CaptureLifecycleSink, ChangedDocumentSink, CompleteDocumentTree,
    DocumentLeafExecutionPolicy, DocumentLeafFingerprint, DocumentRecordSpool,
    DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResult, CONTINUE_CLI_SOURCE_FORMAT,
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
    "continue-nativepath-source-backed-v1-neutral-activity";

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

/// Provider state for one canonical replacement-document route.
pub struct ContinueSourceBackedReader<L, S, C> {
    root: PathBuf,
    source_anchor_scope: SourceAnchorScope,
    _lifecycle: crate::ProviderLifecycleMarker<L, S, C>,
}

impl<L, S, C> ContinueSourceBackedReader<L, S, C> {
    pub fn new(root: PathBuf) -> Self {
        Self::new_scoped(root, SourceAnchorScope::Unqualified)
    }

    pub fn new_scoped(root: PathBuf, source_anchor_scope: SourceAnchorScope) -> Self {
        Self {
            root,
            source_anchor_scope,
            _lifecycle: std::marker::PhantomData,
        }
    }
}

impl<L, S, C> ReplacementDocumentTree for ContinueSourceBackedReader<L, S, C>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
    C: Send + Sync + 'static,
{
    type Lifecycle = L;
    type Spool = S;
    type RouteControl = C;
    type Leaf = ContinueDocumentLeaf;
    type TreeAuthority = ContinueTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        CONTINUE_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        owns_continue_source(source)
    }

    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        DocumentLeafExecutionPolicy::Serial
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let discovery = discover_continue_root(&self.root).map_err(route_error)?;
        let (leaves, authority) = discovery.into_parts();
        let tree_fingerprint = scope_continue_document_fingerprint(
            authority.tree_fingerprint(),
            self.source_anchor_scope,
            b"ctx.continue-document-root-scoped-tree-v1\0",
        );
        let leaves = leaves
            .into_iter()
            .map(|leaf| {
                ObservedDocumentLeaf::new(
                    DocumentLeafFingerprint::new(scope_continue_document_fingerprint(
                        authority.leaf_fingerprint(&leaf),
                        self.source_anchor_scope,
                        b"ctx.continue-document-root-scoped-leaf-v1\0",
                    )),
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
        sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let snapshot = authority.open_source(leaf).map_err(route_error)?;
        scan_continue_document(snapshot, authority, self.source_anchor_scope, sink)
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        tree.authority
            .revalidate_fingerprint()
            .map_err(route_error)?
            .map(|fingerprint| {
                scope_continue_document_fingerprint(
                    fingerprint,
                    self.source_anchor_scope,
                    b"ctx.continue-document-root-scoped-tree-v1\0",
                )
            })
            .ok_or_else(|| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Continue document inventory changed before terminal certification",
                )
            })
    }
}

fn scope_continue_document_fingerprint(
    fingerprint: [u8; 32],
    source_anchor_scope: SourceAnchorScope,
    domain: &[u8],
) -> [u8; 32] {
    let SourceAnchorScope::Lineage(root_lineage) = source_anchor_scope else {
        return fingerprint;
    };
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(root_lineage);
    digest.update(fingerprint);
    digest.finalize().into()
}

fn scan_continue_document<L, S>(
    snapshot: super::source::ContinueSourceSnapshot,
    authority: &ContinueTreeAuthority,
    source_anchor_scope: SourceAnchorScope,
    sink: &mut ChangedDocumentSink<'_, '_, L, S>,
) -> SourceBackedRouteResult<DocumentSourceTerminal>
where
    L: CaptureLifecycleSink,
    S: DocumentRecordSpool,
{
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
    let mut sink_failure = None;
    let projected =
        project_changed_stream(&mut stream, source_anchor_scope, sink, &mut sink_failure);
    if let Some(error) = sink_failure {
        return Err(error);
    }
    projected.map_err(route_error)
}

fn project_changed_stream<L, S>(
    stream: &mut ContinueSourcePageStream,
    source_anchor_scope: SourceAnchorScope,
    sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    sink_failure: &mut Option<SourceBackedRouteError>,
) -> ContinueSourceBackedResult<DocumentSourceTerminal>
where
    L: CaptureLifecycleSink,
    S: DocumentRecordSpool,
{
    let mut active = None;
    let mut terminal = None;
    while let Some(page) = stream.next_page().map_err(|failure| {
        ContinueSourceBackedError::ProjectionFailure(failure.message.into_string())
    })? {
        if terminal.is_some() {
            return Err(ContinueSourceBackedError::CountMismatch);
        }
        terminal =
            project_changed_page(&mut active, page, source_anchor_scope, sink, sink_failure)?;
    }
    if active.is_some() {
        return Err(ContinueSourceBackedError::UnterminatedSource);
    }
    terminal.ok_or(ContinueSourceBackedError::UnterminatedSource)
}

fn project_changed_page<L, S>(
    active: &mut Option<ActiveSource>,
    mut page: ContinuePreparedPage,
    source_anchor_scope: SourceAnchorScope,
    sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    sink_failure: &mut Option<SourceBackedRouteError>,
) -> ContinueSourceBackedResult<Option<DocumentSourceTerminal>>
where
    L: CaptureLifecycleSink,
    S: DocumentRecordSpool,
{
    if let Some(prepared) = page.source.take() {
        if active.is_some() {
            return Err(ContinueSourceBackedError::OverlappingSource);
        }
        let started = start_source(*prepared, source_anchor_scope)?;
        sink.begin_source(started.source.clone()).map_err(|error| {
            let detail = error.to_string();
            *sink_failure = Some(error);
            ContinueSourceBackedError::ProjectionFailure(detail)
        })?;
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
        sink.emit_core_record(document).map_err(|error| {
            let detail = error.to_string();
            *sink_failure = Some(error);
            ContinueSourceBackedError::ProjectionFailure(detail)
        })?;
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

fn start_source(
    prepared: ContinuePreparedSource,
    source_anchor_scope: SourceAnchorScope,
) -> ContinueSourceBackedResult<ActiveSource> {
    let native_session_id = prepared.session.identity.0.as_str();
    let source = continue_source_key_scoped(native_session_id, source_anchor_scope)?;
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
    project_bound_event(
        &active.source,
        active.session_id,
        active.source_revision_digest,
        &active.prepared.session,
        event,
    )
}

fn project_bound_event(
    source: &SourceKey,
    session_id: StableEntityId,
    source_revision_digest: [u8; 32],
    session: &super::normalize::ContinueSessionRow,
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
            TypedKey::bytes(source_revision_digest.to_vec())?,
        )?
    };
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: CONTINUE_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8(&session.identity.0)?,
        TypedKey::U64(event.identity.history_ordinal),
    ])?;
    let body = continue_lexical_body(&event);
    if body.is_empty() {
        return Err(ContinueSourceBackedError::CountMismatch);
    }
    let occurred_at_unix_ms = event
        .occurred_at
        .or(session.started_at)
        .map(|timestamp| timestamp.timestamp_millis());
    let workspace = session.workspace_directory.clone();
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
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        event.identity.history_ordinal,
        event_type,
        CONTINUE_SOURCE_BACKED_PARSER_REVISION,
        body,
    )?;
    record.agent_scope = Some(AgentScope::Primary);
    record.provider_session_id = Some(session.identity.0.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = occurred_at_unix_ms;
    record.role = Some(role.to_owned());
    record.content.structured_content = Some(serde_json::json!({
        "calls": &event.calls,
        "native_item_id": &event.native_item_id,
    }));
    let mut facts = Vec::new();
    if let Some(workspace) = workspace {
        facts.push(ProviderDeclaredFact {
            kind: LiteralFactKind::SessionCwd,
            value: workspace,
        });
    }
    let (provider_call_id, invocation) = if event.calls.len() == 1 {
        let call = &event.calls[0];
        let call_id = exact_continue_call_id(call);
        let invocation = call_id
            .zip(call.tool_name.as_ref())
            .map(|(_, tool)| ActivityInvocation {
                protocol: None,
                server: None,
                tool: tool.clone(),
                arguments: call.arguments.clone(),
                started_at_unix_ms: occurred_at_unix_ms,
            });
        (call_id.map(TypedKey::utf8).transpose()?, invocation)
    } else {
        (None, None)
    };
    if invocation.is_some() || !facts.is_empty() {
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id,
            invocation,
            result: None,
            facts,
        });
    }
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(record)
}

fn exact_continue_call_id(call: &ContinueCallRelationship) -> Option<&str> {
    match (call.call_id.as_deref(), call.nested_call_id.as_deref()) {
        (Some(left), Some(right)) if left != right => None,
        (Some(value), _) | (_, Some(value)) => Some(value),
        (None, None) => None,
    }
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

#[cfg(test)]
fn continue_source_key(native_session_id: &str) -> ContinueSourceBackedResult<SourceKey> {
    continue_source_key_scoped(native_session_id, SourceAnchorScope::Unqualified)
}

fn continue_source_key_scoped(
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> ContinueSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive_provider_native_scoped(
        CaptureProvider::Continue.as_str(),
        CONTINUE_CLI_SOURCE_FORMAT,
        CONTINUE_SOURCE_SCHEMA_VARIANT,
        1,
        CONTINUE_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
        source_anchor_scope,
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
mod identity_tests {
    use super::*;

    #[test]
    fn root_scope_distinguishes_native_sessions_and_unqualified_is_unchanged() {
        let native_session_id = "same-native-session";
        let legacy = continue_source_key(native_session_id).unwrap();
        let unqualified =
            continue_source_key_scoped(native_session_id, SourceAnchorScope::Unqualified).unwrap();
        let first =
            continue_source_key_scoped(native_session_id, SourceAnchorScope::Lineage([1; 32]))
                .unwrap();
        let second =
            continue_source_key_scoped(native_session_id, SourceAnchorScope::Lineage([2; 32]))
                .unwrap();

        assert!(legacy.exact_descriptor_eq(&unqualified));
        assert_ne!(first.identity(), second.identity());
        assert_ne!(
            continue_session_id(&first, native_session_id).unwrap(),
            continue_session_id(&second, native_session_id).unwrap()
        );
    }

    #[test]
    fn root_scope_partitions_document_replay_without_changing_unqualified_fingerprints() {
        let fingerprint = [7; 32];
        let domain = b"ctx.continue-document-root-scoped-test-v1\0";

        assert_eq!(
            scope_continue_document_fingerprint(
                fingerprint,
                SourceAnchorScope::Unqualified,
                domain,
            ),
            fingerprint
        );
        assert_ne!(
            scope_continue_document_fingerprint(
                fingerprint,
                SourceAnchorScope::Lineage([1; 32]),
                domain,
            ),
            scope_continue_document_fingerprint(
                fingerprint,
                SourceAnchorScope::Lineage([2; 32]),
                domain,
            )
        );
    }
}

#[cfg(test)]
#[path = "source_backed_tests.rs"]
mod tests;
