use std::path::{Path, PathBuf};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    EventIdentityInput, EventType, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator, SourceResolverContractError,
    StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    lifecycle::{
        prepare_continue_discovery_with_profile, ContinuePreparationStream, ContinueSourceOutcome,
    },
    normalize::{
        ContinueNativeProfile, ContinuePreparedPage, ContinuePreparedSource,
        ContinueSourceCompleteness, CONTINUE_NATIVE_MAX_PAGE_BYTES, CONTINUE_NATIVE_MAX_PAGE_ROWS,
    },
    parse::{
        locate_continue_exact_history_item, ContinueExactHistoryLookup, ContinueIncompleteSource,
        ContinueOutputExclusionStats, ContinueSourceFailure,
    },
    source::{ContinueDiscovery, ContinueRootAuthority},
    ContinueEventKind, ContinueEventRole, ContinueEventRow, ContinueGenerationAuthority,
    ContinueIndexObservation, ContinueNativePathError, ContinueSessionRow,
    ContinueSourceObservation,
};
use crate::{
    provider::providers::continue_cli::continue_history_item_text, CONTINUE_CLI_SOURCE_FORMAT,
    PROVIDER_MAX_TEXT_CHARS,
};

const CONTINUE_SOURCE_ANCHOR_NAMESPACE: &str = "continue.session";
const CONTINUE_NATIVE_SESSION_NAMESPACE: &str = "continue.session";
const CONTINUE_NATIVE_EVENT_NAMESPACE: &str = "continue.history-item";
const CONTINUE_NATIVE_EVENT_POSITION_KIND: &str = "continue.history-ordinal";
const CONTINUE_LOGICAL_SESSION_KIND: &str = "continue-session";
const CONTINUE_LOGICAL_EVENT_KIND: &str = "continue-event";
const CONTINUE_SOURCE_SCHEMA_VARIANT: &str = "continue-nativepath-document-v0";
const CONTINUE_SOURCE_REVISION_KIND: &str = "continue-whole-document-observation-v0";
const CONTINUE_SOURCE_BACKED_PARSER_REVISION: &str = "continue-nativepath-source-backed-v0";
const MAX_CONTINUE_LEXICAL_METADATA_CHARS: usize = 8 * 1024;

#[derive(Debug, Error)]
pub(crate) enum ContinueSourceBackedError {
    #[error(transparent)]
    Native(#[from] ContinueNativePathError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
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
    #[error("Continue Core-only source-backed parsing emitted transient Pro output")]
    UnexpectedOutput,
    #[error("Continue source-backed terminal counts do not reconcile")]
    CountMismatch,
    #[error("Continue source-backed count or ordinal overflowed")]
    CountOverflow,
    #[error("Continue source revision evidence is malformed")]
    InvalidRevisionEvidence,
    #[error("locator is not a Continue whole-document history item")]
    InvalidLocator,
    #[error("Continue locator source revision was not found below the selected sessions root")]
    LocatorSourceRevisionNotFound,
    #[error("Continue locator source revision is ambiguous below the selected sessions root")]
    AmbiguousLocatorSource,
    #[error("Continue locator history item is missing")]
    LocatorRecordMissing,
    #[error("Continue locator record digest no longer matches provider bytes")]
    LocatorDigestMismatch,
    #[error("Continue exact resolver rejected its certified source: {0}")]
    ExactResolver(String),
}

pub(crate) type ContinueSourceBackedResult<T> = Result<T, ContinueSourceBackedError>;

/// One independently bounded provider page for the shared source-backed
/// coordinator. The terminal page carries its final source certificate.
#[derive(Debug)]
pub(crate) struct ContinueSourceBackedPage {
    pub(crate) page_identity: [u8; 32],
    pub(crate) page_ordinal: u64,
    pub(crate) expected_history_ordinal: u64,
    pub(crate) next_history_ordinal: u64,
    pub(crate) logical_units: usize,
    pub(crate) estimated_bytes: usize,
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) terminal: Option<ContinueSourceBackedLeaf>,
}

/// Terminal provider-owned evidence. Publication, replacement classification,
/// inventory deletion, and retry policy remain shared responsibilities.
#[derive(Debug)]
pub(crate) struct ContinueSourceBackedLeaf {
    pub(crate) source: SourceKey,
    pub(crate) session: ContinueSessionRow,
    pub(crate) session_id: StableEntityId,
    pub(crate) certificate: CertifiedSource,
    pub(crate) source_observation: ContinueSourceObservation,
    pub(crate) index_dependency: ContinueIndexObservation,
    pub(crate) output_exclusion: ContinueOutputExclusionStats,
}

#[derive(Debug)]
pub(crate) enum ContinueSourceBackedOutcome {
    Page(ContinueSourceBackedPage),
    Incomplete(Box<ContinueIncompleteSource>),
    Failed(ContinueSourceFailure),
}

struct ActiveSource {
    source: SourceKey,
    session_id: StableEntityId,
    prepared: ContinuePreparedSource,
    source_revision_digest: [u8; 32],
    next_history_ordinal: u64,
    emitted_documents: u64,
}

/// Bounded adapter over the already-selected Continue global sessions root.
///
/// Callers must pass discovery for the selected `.../sessions` root. This
/// reader never searches editor webview/global-storage trees or chooses a
/// second Continue root.
pub(crate) struct ContinueSourceBackedReader<'a> {
    native: ContinuePreparationStream<'a>,
    active: Option<ActiveSource>,
}

impl<'a> ContinueSourceBackedReader<'a> {
    pub(crate) fn new(discovery: &'a ContinueDiscovery) -> ContinueSourceBackedResult<Self> {
        Ok(Self {
            native: prepare_continue_discovery_with_profile(
                discovery,
                ContinueNativeProfile::CoreOnly,
            )?,
            active: None,
        })
    }

    pub(crate) fn root_authority(&self) -> &ContinueRootAuthority {
        self.native.root_authority()
    }

    pub(crate) fn next_outcome(
        &mut self,
    ) -> ContinueSourceBackedResult<Option<ContinueSourceBackedOutcome>> {
        let Some(outcome) = self.native.next() else {
            if self.active.is_some() {
                return Err(ContinueSourceBackedError::UnterminatedSource);
            }
            return Ok(None);
        };
        match outcome? {
            ContinueSourceOutcome::Page(page) => self
                .project_page(*page)
                .map(ContinueSourceBackedOutcome::Page)
                .map(Some),
            ContinueSourceOutcome::Incomplete(source) => {
                if self.active.is_some() {
                    return Err(ContinueSourceBackedError::UnterminatedSource);
                }
                Ok(Some(ContinueSourceBackedOutcome::Incomplete(source)))
            }
            ContinueSourceOutcome::Failed(failure) => {
                if self.active.is_some() {
                    return Err(ContinueSourceBackedError::UnterminatedSource);
                }
                Ok(Some(ContinueSourceBackedOutcome::Failed(failure)))
            }
        }
    }

    fn project_page(
        &mut self,
        mut page: ContinuePreparedPage,
    ) -> ContinueSourceBackedResult<ContinueSourceBackedPage> {
        if page.transient_output.is_some() {
            return Err(ContinueSourceBackedError::UnexpectedOutput);
        }
        if let Some(prepared) = page.source.take() {
            if self.active.is_some() {
                return Err(ContinueSourceBackedError::OverlappingSource);
            }
            self.active = Some(start_source(*prepared)?);
        }
        let active = self
            .active
            .as_mut()
            .ok_or(ContinueSourceBackedError::MissingSourceAuthority)?;
        if active.prepared.session.identity != page.session_identity {
            return Err(ContinueSourceBackedError::SessionChanged);
        }

        let expected_history_ordinal = active.next_history_ordinal;
        let mut documents = Vec::with_capacity(page.events.len());
        for event in page.events {
            if event.identity.history_ordinal < active.next_history_ordinal {
                return Err(ContinueSourceBackedError::CountMismatch);
            }
            active.next_history_ordinal = event
                .identity
                .history_ordinal
                .checked_add(1)
                .ok_or(ContinueSourceBackedError::CountOverflow)?;
            documents.push(project_event(active, event)?);
        }
        active.emitted_documents = active
            .emitted_documents
            .checked_add(
                u64::try_from(documents.len())
                    .map_err(|_| ContinueSourceBackedError::CountOverflow)?,
            )
            .ok_or(ContinueSourceBackedError::CountOverflow)?;

        let page_identity = page_identity(
            &active.source,
            page.page_ordinal,
            expected_history_ordinal,
            active.next_history_ordinal,
            &documents,
        );
        let next_history_ordinal = active.next_history_ordinal;
        let terminal = if page.terminal {
            let authority = page
                .authority
                .take()
                .ok_or(ContinueSourceBackedError::CountMismatch)?;
            let output_exclusion = page
                .output_exclusion
                .take()
                .ok_or(ContinueSourceBackedError::CountMismatch)?;
            let active = self
                .active
                .take()
                .ok_or(ContinueSourceBackedError::MissingSourceAuthority)?;
            Some(finish_source(active, authority, output_exclusion)?)
        } else {
            if page.authority.is_some() || page.output_exclusion.is_some() {
                return Err(ContinueSourceBackedError::CountMismatch);
            }
            None
        };
        if documents.len() > CONTINUE_NATIVE_MAX_PAGE_ROWS
            || page.estimated_bytes > CONTINUE_NATIVE_MAX_PAGE_BYTES
        {
            return Err(ContinueSourceBackedError::CountMismatch);
        }
        Ok(ContinueSourceBackedPage {
            page_identity,
            page_ordinal: page.page_ordinal,
            expected_history_ordinal,
            next_history_ordinal,
            logical_units: page.row_count,
            estimated_bytes: page.estimated_bytes,
            documents,
            terminal,
        })
    }
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
) -> ContinueSourceBackedResult<LexicalDocument> {
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
    let locator = SourceRecordLocator::new(
        active.source.clone(),
        NativeRecordCoordinate::Document {
            object_key: TypedKey::utf8(&active.prepared.session.identity.0)?,
            json_pointer: Some(format!("/history/{}", event.identity.history_ordinal)),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(active.source_revision_digest),
        event.source_record_digest,
    )?;
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
    Ok(LexicalDocument {
        event_id,
        session_id: active.session_id,
        parent_session_id: None,
        root_session_id: active.session_id,
        source: active.source.clone(),
        locator,
        provider_session_id: Some(active.prepared.session.identity.0.clone()),
        branch: None,
        source_path: Some(
            active
                .prepared
                .observation
                .canonical_path()
                .display()
                .to_string(),
        ),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: event.identity.history_ordinal,
        occurred_at_unix_ms,
        event_type: match event.kind {
            ContinueEventKind::Message => EventType::Message.as_str().to_owned(),
            ContinueEventKind::ToolCall => EventType::ToolCall.as_str().to_owned(),
        },
        role: Some(
            match event.role {
                ContinueEventRole::User => "user",
                ContinueEventRole::Assistant => "assistant",
                ContinueEventRole::System => "system",
                ContinueEventRole::Tool => "tool",
                ContinueEventRole::Unknown => "unknown",
            }
            .to_owned(),
        ),
        body,
        workspace: workspace.clone(),
        cwd: workspace,
        touched_files: event
            .file_touches
            .iter()
            .map(|touch| touch.path.clone())
            .collect(),
    })
}

fn finish_source(
    active: ActiveSource,
    authority: ContinueGenerationAuthority,
    output_exclusion: ContinueOutputExclusionStats,
) -> ContinueSourceBackedResult<ContinueSourceBackedLeaf> {
    if authority.completeness != ContinueSourceCompleteness::Complete
        || authority.retained_events
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
    let complete_records =
        u64::try_from(observed).map_err(|_| ContinueSourceBackedError::CountOverflow)?;
    let retained_records = u64::try_from(authority.retained_events)
        .map_err(|_| ContinueSourceBackedError::CountOverflow)?;
    let rejected_records = u64::try_from(authority.rejected_items)
        .map_err(|_| ContinueSourceBackedError::CountOverflow)?;
    let counts = ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records: 0,
        indexed_documents: active.emitted_documents,
        certified_bytes: active.prepared.observation.raw_bytes(),
    };
    let observation = source_observation(
        &active.source,
        active.source_revision_digest,
        active.prepared.index_dependency.dependency_revision(),
    )?;
    let certificate = CertifiedSource::certify(
        observation.clone(),
        observation,
        CONTINUE_SOURCE_BACKED_PARSER_REVISION,
        active.source_revision_digest,
        counts,
    )?;
    Ok(ContinueSourceBackedLeaf {
        source: active.source,
        session: active.prepared.session,
        session_id: active.session_id,
        certificate,
        source_observation: active.prepared.observation,
        index_dependency: active.prepared.index_dependency,
        output_exclusion,
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
        return bounded_chars(&event.search_text, PROVIDER_MAX_TEXT_CHARS);
    }
    match event.kind {
        ContinueEventKind::Message => "Continue message".to_owned(),
        ContinueEventKind::ToolCall => "Continue tool call".to_owned(),
    }
}

fn bounded_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn page_identity(
    source: &SourceKey,
    page_ordinal: u64,
    expected_history_ordinal: u64,
    next_history_ordinal: u64,
    documents: &[LexicalDocument],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.continue.source-backed-page\0");
    digest.update(source.identity().digest());
    digest.update(page_ordinal.to_be_bytes());
    digest.update(expected_history_ordinal.to_be_bytes());
    digest.update(next_history_ordinal.to_be_bytes());
    digest.update((documents.len() as u64).to_be_bytes());
    for document in documents {
        digest.update(document.event_id.digest());
    }
    digest.finalize().into()
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinueHydratedSourceRecord {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: Option<String>,
}

/// Resolves one exact whole-document locator below the same selected sessions
/// root used for discovery. Relocation inside that root is allowed; stale
/// source revisions and duplicate exact revisions fail closed.
pub(crate) fn hydrate_continue_source_backed_record(
    selected_sessions_root: impl AsRef<Path>,
    locator: &SourceRecordLocator,
) -> ContinueSourceBackedResult<ContinueHydratedSourceRecord> {
    locator.validate_contract()?;
    let (native_session_id, history_ordinal, certified_revision) =
        validate_continue_locator(locator)?;
    let discovery = super::discover_continue_root(selected_sessions_root.as_ref())?;
    let mut paths = discovery.paths()?;
    let mut resolved: Option<(PathBuf, Vec<u8>)> = None;
    for path in &mut paths {
        let path = path?;
        let snapshot = discovery.open_source(&path)?;
        let observed_revision = decode_hex_digest(snapshot.observation().session_revision())
            .ok_or(ContinueSourceBackedError::InvalidRevisionEvidence)?;
        if observed_revision != certified_revision {
            continue;
        }
        let provider_bytes = match locate_continue_exact_history_item(
            snapshot.bytes(),
            &native_session_id,
            history_ordinal,
        )
        .map_err(ContinueSourceBackedError::ExactResolver)?
        {
            ContinueExactHistoryLookup::DifferentSession => {
                return Err(ContinueSourceBackedError::InvalidLocator)
            }
            ContinueExactHistoryLookup::MissingItem => {
                return Err(ContinueSourceBackedError::LocatorRecordMissing)
            }
            ContinueExactHistoryLookup::Item(bytes) => bytes.to_vec(),
        };
        if resolved.replace((path, provider_bytes)).is_some() {
            return Err(ContinueSourceBackedError::AmbiguousLocatorSource);
        }
    }
    let (_path, provider_bytes) =
        resolved.ok_or(ContinueSourceBackedError::LocatorSourceRevisionNotFound)?;
    if !discovery.root_authority().revalidate()?.authoritative {
        return Err(ContinueNativePathError::SourceChanged {
            path: discovery.index().observation().path().to_path_buf(),
        }
        .into());
    }
    let actual_digest: [u8; 32] = Sha256::digest(&provider_bytes).into();
    if &actual_digest != locator.record_digest() {
        return Err(ContinueSourceBackedError::LocatorDigestMismatch);
    }
    let value: Value = serde_json::from_slice(&provider_bytes)?;
    let decoded_display_text = continue_history_item_text(&value)
        .ok_or(ContinueSourceBackedError::LocatorRecordMissing)?;
    Ok(ContinueHydratedSourceRecord {
        provider_bytes: decoded_display_text.as_bytes().to_vec(),
        decoded_display_text: Some(decoded_display_text),
    })
}

fn validate_continue_locator(
    locator: &SourceRecordLocator,
) -> ContinueSourceBackedResult<(String, u64, [u8; 32])> {
    if locator.source().provider() != CaptureProvider::Continue.as_str()
        || locator.source().source_format() != CONTINUE_CLI_SOURCE_FORMAT
        || locator.source().schema_variant() != CONTINUE_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(ContinueSourceBackedError::InvalidLocator);
    }
    let SourceAnchor::ProviderNative { namespace, key } = locator.source().anchor() else {
        return Err(ContinueSourceBackedError::InvalidLocator);
    };
    let TypedKey::Utf8(native_session_id) = key else {
        return Err(ContinueSourceBackedError::InvalidLocator);
    };
    if namespace != CONTINUE_SOURCE_ANCHOR_NAMESPACE {
        return Err(ContinueSourceBackedError::InvalidLocator);
    }
    let NativeRecordCoordinate::Document {
        object_key,
        json_pointer,
    } = locator.coordinate()
    else {
        return Err(ContinueSourceBackedError::InvalidLocator);
    };
    if object_key != &TypedKey::Utf8(native_session_id.clone()) {
        return Err(ContinueSourceBackedError::InvalidLocator);
    }
    let pointer = json_pointer
        .as_deref()
        .and_then(|pointer| pointer.strip_prefix("/history/"))
        .ok_or(ContinueSourceBackedError::InvalidLocator)?;
    let history_ordinal = pointer
        .parse::<u64>()
        .map_err(|_| ContinueSourceBackedError::InvalidLocator)?;
    if pointer != history_ordinal.to_string() {
        return Err(ContinueSourceBackedError::InvalidLocator);
    }
    let certified_revision = locator
        .certified_source_revision_digest()
        .copied()
        .ok_or(ContinueSourceBackedError::InvalidLocator)?;
    Ok((
        native_session_id.clone(),
        history_ordinal,
        certified_revision,
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::test_support_paths::tempdir;

    struct Scan {
        pages: Vec<ContinueSourceBackedPage>,
    }

    impl Scan {
        fn documents(&self) -> Vec<&LexicalDocument> {
            self.pages
                .iter()
                .flat_map(|page| page.documents.iter())
                .collect()
        }

        fn leaf(&self) -> &ContinueSourceBackedLeaf {
            self.pages
                .iter()
                .find_map(|page| page.terminal.as_ref())
                .expect("scan must contain one terminal leaf")
        }
    }

    fn message(id: Option<&str>, text: &str) -> Value {
        let mut item = json!({
            "timestamp": "2026-07-28T12:00:00Z",
            "message": {
                "role": "assistant",
                "content": text,
            }
        });
        if let Some(id) = id {
            item.as_object_mut()
                .expect("message fixture object")
                .insert("id".to_owned(), Value::String(id.to_owned()));
        }
        item
    }

    fn tool_call(id: &str, secret_output: &str) -> Value {
        json!({
            "id": id,
            "timestamp": "2026-07-28T12:00:01Z",
            "message": {
                "role": "assistant",
                "content": "",
            },
            "toolCallStates": [{
                "toolCallId": format!("call-{id}"),
                "toolCall": {
                    "id": format!("call-{id}"),
                    "type": "function",
                    "function": {
                        "name": "shell",
                        "arguments": "{\"command\":\"secret command\"}",
                    }
                },
                "status": "done",
                "output": secret_output,
            }]
        })
    }

    fn session(session_id: &str, history: Vec<Value>) -> Value {
        json!({
            "sessionId": session_id,
            "title": format!("Session {session_id}"),
            "createdAt": "2026-07-28T12:00:00Z",
            "workspaceDirectory": "/workspace/continue",
            "history": history,
        })
    }

    fn write_session(root: &Path, name: &str, value: &Value) -> PathBuf {
        fs::create_dir_all(root).unwrap();
        let path = root.join(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }

    fn scan(root: &Path) -> Scan {
        let discovery = super::super::discover_continue_root(root).unwrap();
        let mut reader = ContinueSourceBackedReader::new(&discovery).unwrap();
        let mut pages = Vec::new();
        while let Some(outcome) = reader.next_outcome().unwrap() {
            match outcome {
                ContinueSourceBackedOutcome::Page(page) => pages.push(page),
                ContinueSourceBackedOutcome::Incomplete(_) => {
                    panic!("complete fixture became incomplete")
                }
                ContinueSourceBackedOutcome::Failed(failure) => {
                    panic!("complete fixture failed: {}", failure.message)
                }
            }
        }
        Scan { pages }
    }

    #[test]
    fn continue_source_backed_cold_scan_is_bounded_stable_and_excludes_webviews_and_outputs() {
        const SECRET: &str = "CONTINUE-SUCCESSFUL-OUTPUT-SECRET";

        let temp = tempdir().unwrap();
        let global = temp.path().join("continue-global");
        let sessions = global.join("sessions");
        write_session(
            &sessions,
            "primary.json",
            &session(
                "continue-primary",
                vec![
                    message(Some("message-one"), "cold searchable sentinel"),
                    tool_call("tool-two", SECRET),
                ],
            ),
        );
        fs::write(
            sessions.join("sessions.json"),
            serde_json::to_vec(&json!([{
                "sessionId": "continue-primary",
                "title": "Indexed title",
            }]))
            .unwrap(),
        )
        .unwrap();
        let editor_webview = global.join("editor-webview/globalStorage/continue/sessions");
        write_session(
            &editor_webview,
            "editor.json",
            &session(
                "continue-editor-only",
                vec![message(Some("editor-message"), "must stay excluded")],
            ),
        );

        let first = scan(&sessions);
        let second = scan(&sessions);
        let first_documents = first.documents();
        let second_documents = second.documents();
        assert_eq!(first_documents.len(), 2);
        assert_eq!(
            first_documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>(),
            second_documents
                .iter()
                .map(|document| document.event_id)
                .collect::<Vec<_>>()
        );
        assert!(first.pages.iter().all(|page| {
            page.documents.len() <= CONTINUE_NATIVE_MAX_PAGE_ROWS
                && page.estimated_bytes <= CONTINUE_NATIVE_MAX_PAGE_BYTES
        }));
        assert!(first_documents
            .iter()
            .all(|document| !document.body.contains(SECRET)));
        assert!(first_documents
            .iter()
            .all(|document| !document.body.contains("must stay excluded")));
        assert!(first_documents.iter().all(|document| {
            document.parent_session_id.is_none()
                && document.root_session_id == document.session_id
                && document.provider_session_id.as_deref() == Some("continue-primary")
                && document.branch.is_none()
                && document
                    .source_path
                    .as_deref()
                    .is_some_and(|path| path.ends_with("primary.json"))
                && document.agent_type == AgentType::Primary.as_str()
                && document.is_primary
        }));
        assert_eq!(first.leaf().certificate.counts().complete_records, 2);
        assert_eq!(first.leaf().certificate.counts().indexed_documents, 2);
        assert_eq!(first.leaf().output_exclusion.native_results_observed, 1);
        assert_eq!(first.leaf().output_exclusion.result_string_allocations, 0);
        assert_eq!(first.leaf().output_exclusion.result_hashes_created, 0);

        let NativeRecordCoordinate::Document {
            object_key,
            json_pointer,
        } = first_documents[0].locator.coordinate()
        else {
            panic!("Continue source-backed locator must be a document locator");
        };
        assert_eq!(object_key, &TypedKey::Utf8("continue-primary".to_owned()));
        assert_eq!(json_pointer.as_deref(), Some("/history/0"));
    }

    #[test]
    fn continue_source_backed_exact_resolver_returns_verified_display_content() {
        const SECRET: &str = "CONTINUE-EXACT-OUTPUT-SECRET";

        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let history_item = tool_call("tool-exact", SECRET);
        write_session(
            &sessions,
            "exact.json",
            &session("continue-exact", vec![history_item.clone()]),
        );
        let scanned = scan(&sessions);
        let document = scanned.documents()[0];
        let hydrated = hydrate_continue_source_backed_record(&sessions, &document.locator).unwrap();
        assert!(!hydrated
            .decoded_display_text
            .as_deref()
            .unwrap_or_default()
            .contains(SECRET));
        assert!(hydrated
            .decoded_display_text
            .as_deref()
            .unwrap_or_default()
            .contains("status: done"));
        assert_eq!(
            hydrated.provider_bytes,
            hydrated.decoded_display_text.unwrap().as_bytes()
        );
        assert!(!String::from_utf8_lossy(&hydrated.provider_bytes).contains(SECRET));
    }

    #[test]
    fn continue_source_backed_indexes_the_full_selected_message_body() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let text = format!(
            "{}continue-tail-term{}",
            "x".repeat(3_000),
            "y".repeat(PROVIDER_MAX_TEXT_CHARS)
        );
        write_session(
            &sessions,
            "full-body.json",
            &session(
                "continue-full-body",
                vec![message(Some("full-body-message"), &text)],
            ),
        );

        let scanned = scan(&sessions);
        let document = scanned.documents()[0];
        assert_eq!(document.body.chars().count(), PROVIDER_MAX_TEXT_CHARS);
        assert!(document.body.contains("continue-tail-term"));
        let hydrated = hydrate_continue_source_backed_record(&sessions, &document.locator).unwrap();
        assert_eq!(hydrated.provider_bytes, text.as_bytes());
        assert_eq!(
            hydrated.decoded_display_text.as_deref(),
            Some(text.as_str())
        );
    }

    #[test]
    fn continue_source_backed_replacement_keeps_native_ids_and_rejects_stale_locators() {
        let temp = tempdir().unwrap();
        let sessions = temp.path().join("sessions");
        let path = write_session(
            &sessions,
            "replace.json",
            &session(
                "continue-replacement",
                vec![
                    message(Some("stable-native-id"), "before replacement"),
                    message(None, "before positional replacement"),
                ],
            ),
        );
        let before = scan(&sessions);
        let before_documents = before.documents();
        let stable_event_id = before_documents[0].event_id;
        let positional_event_id = before_documents[1].event_id;
        let stale_locator = before_documents[0].locator.clone();
        let source_id = before.leaf().source.identity();
        let session_id = before.leaf().session_id;

        fs::write(
            &path,
            serde_json::to_vec(&session(
                "continue-replacement",
                vec![
                    message(Some("inserted-native-id"), "inserted replacement"),
                    message(Some("stable-native-id"), "after replacement"),
                    message(None, "after positional replacement"),
                ],
            ))
            .unwrap(),
        )
        .unwrap();

        assert!(matches!(
            hydrate_continue_source_backed_record(&sessions, &stale_locator),
            Err(ContinueSourceBackedError::LocatorSourceRevisionNotFound)
        ));
        let after = scan(&sessions);
        let after_documents = after.documents();
        assert_eq!(after.leaf().source.identity(), source_id);
        assert_eq!(after.leaf().session_id, session_id);
        assert_eq!(after_documents[1].event_id, stable_event_id);
        assert_ne!(after_documents[2].event_id, positional_event_id);
        assert_eq!(after_documents[1].event_sequence, 1);
        let hydrated =
            hydrate_continue_source_backed_record(&sessions, &after_documents[1].locator).unwrap();
        assert_eq!(
            hydrated.decoded_display_text.as_deref(),
            Some("after replacement")
        );
    }
}
