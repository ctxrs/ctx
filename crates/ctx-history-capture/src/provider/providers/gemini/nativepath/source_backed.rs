use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, CoreRecordError,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ProjectionContractError,
    SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId, TypedKey,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::dto::{GeminiEventBody, GeminiTranscriptLayout};
use super::parser::{read_gemini_session_header, GeminiBorrowedRecordParser};
use super::{
    discover_gemini_transcripts, GeminiEventIdentity, GeminiFileObservation, GeminiScanError,
    GeminiSession, GeminiTranscriptSource,
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::{
        executable_route,
        family::jsonl::{
            jsonl_family_driver, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory,
            JsonlFamilyLeaf, JsonlFamilyProjector, JsonlRecordRef,
        },
        SourceBackedCoordinatorResult, SourceBackedProviderRegistry, SourceBackedRouteSelection,
        SourceBackedSelectorAuthority,
    },
    CaptureError, GEMINI_CLI_SOURCE_FORMAT,
};

const GEMINI_SOURCE_ANCHOR_NAMESPACE: &str = "gemini.session";
const GEMINI_NATIVE_SESSION_NAMESPACE: &str = "gemini.session";
const GEMINI_NATIVE_EVENT_NAMESPACE: &str = "gemini.event";
const GEMINI_LOGICAL_SESSION_KIND: &str = "gemini-session";
const GEMINI_LOGICAL_EVENT_KIND: &str = "gemini-event";
const GEMINI_SOURCE_SCHEMA_VARIANT: &str = "gemini-nativepath-jsonl-v0";
const GEMINI_SOURCE_BACKED_PARSER_REVISION: &str = "gemini-nativepath-source-backed-v0-p6-p4";
const MAX_GEMINI_LEXICAL_METADATA_CHARS: usize = 8 * 1024;

pub(crate) mod registration {
    use super::*;
    use crate::ProviderSource;

    pub(crate) fn register(
        registry: &mut SourceBackedProviderRegistry,
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
    ) -> SourceBackedCoordinatorResult<()> {
        let driver = jsonl_family_driver(gemini_jsonl_adapter(), source.path.clone());
        registry.register(executable_route(
            source,
            selection,
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )?);
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum GeminiSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Scan(#[from] GeminiScanError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Core(#[from] CoreRecordError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub(crate) type GeminiSourceBackedResult<T> = Result<T, GeminiSourceBackedError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeminiFamilyBinding {
    relative_path: PathBuf,
    layout: GeminiTranscriptLayout,
    observation: GeminiFileObservation,
    ordinary_file_token: [u8; 32],
    authority_relative_path: PathBuf,
    session: GeminiSession,
}

impl GeminiFamilyBinding {
    fn transcript(&self, leaf: &JsonlFamilyLeaf) -> GeminiTranscriptSource {
        GeminiTranscriptSource {
            path: leaf.source_path().to_path_buf(),
            relative_path: self.relative_path.clone(),
            layout: self.layout.clone(),
            observation: self.observation.clone(),
            ordinary_file_token: self.ordinary_file_token,
            authority_relative_path: self.authority_relative_path.clone(),
            authority: leaf.authority().as_ref().clone(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct GeminiJsonlAdapter;

fn gemini_jsonl_adapter() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(GeminiJsonlAdapter)
}

impl JsonlFamilyAdapter for GeminiJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Gemini
    }

    fn source_format(&self) -> &'static str {
        GEMINI_CLI_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        GEMINI_SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        GEMINI_SOURCE_BACKED_PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::Replacement
    }

    fn discover(&self, root: &Path) -> crate::Result<JsonlFamilyInventory> {
        let metadata = match fs::symlink_metadata(root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        };
        let discovery = discover_gemini_transcripts(root)?;
        if !discovery.completed_inventory {
            return Err(CaptureError::InvalidPayload(
                "Gemini discovery did not produce a complete inventory".to_owned(),
            ));
        }
        let authority = shared_authority(root, &metadata, &discovery.transcripts)?;
        let mut leaves = Vec::with_capacity(discovery.transcripts.len());
        for transcript in discovery.transcripts {
            if transcript.authority.named_path() != authority.named_path()
                || transcript.authority.authority_fingerprint() != authority.authority_fingerprint()
            {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let session = read_gemini_session_header(&transcript).map_err(capture_scan_error)?;
            let source = gemini_source_key(&session.native_session_id).map_err(capture_error)?;
            let binding = GeminiFamilyBinding {
                relative_path: transcript.relative_path.clone(),
                layout: transcript.layout.clone(),
                observation: transcript.observation.clone(),
                ordinary_file_token: transcript.ordinary_file_token,
                authority_relative_path: transcript.authority_relative_path.clone(),
                session,
            };
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                transcript.path,
                Arc::clone(&authority),
                transcript.authority_relative_path,
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract_error)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> crate::Result<Box<dyn JsonlFamilyProjector>> {
        let binding = decode_binding(leaf)?;
        if source_file.ordinary_file_token() != binding.ordinary_file_token {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let expected_source =
            gemini_source_key(&binding.session.native_session_id).map_err(capture_error)?;
        if !expected_source.exact_descriptor_eq(leaf.source()) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let session_id = gemini_session_id(leaf.source(), &binding.session.native_session_id)
            .map_err(capture_error)?;
        let parent_session_id = binding
            .session
            .parent_native_session_id
            .as_deref()
            .map(|parent_native_session_id| {
                let parent_source =
                    gemini_source_key(parent_native_session_id).map_err(capture_error)?;
                gemini_session_id(&parent_source, parent_native_session_id).map_err(capture_error)
            })
            .transpose()?;
        let root_session_id = parent_session_id.unwrap_or(session_id);
        let transcript = binding.transcript(leaf);
        Ok(Box::new(GeminiProjector {
            parser: GeminiBorrowedRecordParser::new(transcript, binding.session.clone()),
            source: leaf.source().clone(),
            session: binding.session,
            session_id,
            parent_session_id,
            root_session_id,
            source_file,
            authority: Arc::clone(leaf.authority()),
        }))
    }
}

struct GeminiProjector {
    parser: GeminiBorrowedRecordParser,
    source: SourceKey,
    session: GeminiSession,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    source_file: Arc<OpenedProviderSourceFile>,
    authority: Arc<ProviderSourceRoot>,
}

impl JsonlFamilyProjector for GeminiProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> crate::Result<()>,
    ) -> crate::Result<()> {
        let evidence = record.evidence();
        for event in self
            .parser
            .project(
                record.bytes(),
                evidence.physical_ordinal(),
                evidence.byte_start(),
                evidence.byte_end_exclusive(),
                evidence.record_digest(),
            )
            .map_err(capture_scan_error)?
        {
            emit(
                project_event(
                    &self.source,
                    self.session_id,
                    self.parent_session_id,
                    self.root_session_id,
                    &self.session,
                    event,
                )
                .map_err(capture_error)?,
            )?;
        }
        Ok(())
    }

    fn finish(&mut self) -> crate::Result<()> {
        self.parser.finish().map_err(capture_scan_error)?;
        self.source_file.revalidate_leaf()?;
        self.authority.revalidate()
    }
}

fn shared_authority(
    root: &Path,
    metadata: &fs::Metadata,
    transcripts: &[GeminiTranscriptSource],
) -> crate::Result<Arc<ProviderSourceRoot>> {
    if let Some(transcript) = transcripts.first() {
        return Ok(Arc::new(transcript.authority.clone()));
    }
    let authority_path = if metadata.is_file() {
        root.parent()
            .ok_or(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "Gemini transcript file has no parent authority",
            })?
    } else {
        root
    };
    Ok(Arc::new(ProviderSourceRoot::open(authority_path)?))
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> crate::Result<GeminiFamilyBinding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(CaptureError::InvalidPayload(
            "Gemini family leaf binding is malformed".to_owned(),
        ));
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn project_event(
    source: &SourceKey,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    session: &GeminiSession,
    event: super::GeminiRetainedEvent,
) -> GeminiSourceBackedResult<CoreRecord> {
    let GeminiEventIdentity::NativeRecordId(native_event_id) = &event.identity;
    let native_item_key = NativeItemKey::native_id(
        GEMINI_NATIVE_EVENT_NAMESPACE,
        TypedKey::utf8(native_event_id)?,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: GEMINI_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let native_event_id = TypedKey::utf8(native_event_id)?;
    let event_sequence = event
        .native_order
        .raw_ordinal
        .checked_mul(u64::from(u32::MAX) + 1)
        .and_then(|sequence| sequence.checked_add(u64::from(event.native_order.sub_ordinal)))
        .ok_or_else(|| {
            GeminiSourceBackedError::Capture(CaptureError::SystemInvariant(
                "Gemini event sequence overflowed",
            ))
        })?;
    let body = lexical_body(&event);
    if body.is_empty() {
        return Err(CaptureError::InvalidPayload(
            "Gemini source-backed event has no lexical body".to_owned(),
        )
        .into());
    }
    let structured_content = if matches!(&event.body, GeminiEventBody::Message { .. })
        && event.safe_file_touches.is_empty()
    {
        None
    } else {
        Some(serde_json::json!({
            "details": (!matches!(&event.body, GeminiEventBody::Message { .. }))
                .then(|| serde_json::to_value(&event.body))
                .transpose()?,
            "file_touches": event.safe_file_touches,
        }))
    };
    let is_primary =
        session.parent_native_session_id.is_none() && session.agent_type != AgentType::Subagent;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        root_session_id,
        source.clone(),
        event_sequence,
        event.event_type.as_str(),
        session.agent_type.as_str(),
        is_primary,
        GEMINI_SOURCE_BACKED_PARSER_REVISION,
        body,
    )?;
    record.parent_session_id = parent_session_id;
    record.provider_session_id = Some(session.native_session_id.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = event
        .occurred_at
        .or(session.started_at)
        .map(|timestamp| timestamp.timestamp_millis());
    record.role = Some(event.role.as_str().to_owned());
    record.cwd = session
        .cwd
        .as_deref()
        .map(|cwd| bounded_chars(cwd, MAX_GEMINI_LEXICAL_METADATA_CHARS));
    record.content.structured_content = structured_content;
    record.validate_contract()?;
    Ok(record)
}

fn lexical_body(event: &super::GeminiRetainedEvent) -> String {
    if !event.searchable_text.is_empty() {
        return event.searchable_text.clone();
    }
    match &event.body {
        GeminiEventBody::Message { text, .. } => text.clone(),
        GeminiEventBody::ToolCall { .. } => "Gemini tool call".to_owned(),
        GeminiEventBody::OutputDiagnostic {
            call_id,
            tool_name,
            outcome,
            exit_code,
            duration_ms,
        } => format!(
            "Gemini {} output {}{}{}{}",
            tool_name.as_deref().unwrap_or("tool"),
            outcome,
            call_id
                .as_deref()
                .map(|call| format!(", call {call}"))
                .unwrap_or_default(),
            exit_code
                .map(|code| format!(", exit code {code}"))
                .unwrap_or_default(),
            duration_ms
                .map(|duration| format!(", duration {duration} ms"))
                .unwrap_or_default(),
        ),
        GeminiEventBody::StateNotice { summary } => summary
            .as_deref()
            .filter(|summary| !summary.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "Gemini state update".to_owned()),
        GeminiEventBody::RewindNotice {
            target_native_record_id,
        } => format!("Gemini rewind to {target_native_record_id}"),
    }
}

fn bounded_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn gemini_source_key(native_session_id: &str) -> GeminiSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        GEMINI_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Gemini.as_str(),
        GEMINI_CLI_SOURCE_FORMAT,
        GEMINI_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn gemini_session_id(
    source: &SourceKey,
    native_session_id: &str,
) -> GeminiSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        GEMINI_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: GEMINI_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

#[cfg(test)]
pub(super) fn project_gemini_test_event(
    source: &GeminiTranscriptSource,
    event: super::GeminiRetainedEvent,
) -> GeminiSourceBackedResult<CoreRecord> {
    let session = read_gemini_session_header(source)?;
    let source_key = gemini_source_key(&session.native_session_id)?;
    let session_id = gemini_session_id(&source_key, &session.native_session_id)?;
    let parent_session_id = session
        .parent_native_session_id
        .as_deref()
        .map(|parent_native_session_id| {
            let parent_source = gemini_source_key(parent_native_session_id)?;
            gemini_session_id(&parent_source, parent_native_session_id)
        })
        .transpose()?;
    project_event(
        &source_key,
        session_id,
        parent_session_id,
        parent_session_id.unwrap_or(session_id),
        &session,
        event,
    )
}

fn capture_scan_error(error: GeminiScanError) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn capture_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn contract_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
