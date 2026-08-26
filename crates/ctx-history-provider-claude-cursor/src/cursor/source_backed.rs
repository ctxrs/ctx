//! Thin Cursor adapter for the shared certified-append JSONL family.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufReader},
    path::Path,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_capture_runtime::BaseEventLookup;
use ctx_history_core::{
    derive_event_id, derive_native_session_id, ActivityInvocation, ActivityJsonCapture,
    ActivityResult, ActivityTextCapture, AgentScope, CaptureProvider, CoreActivity, CoreRecord,
    CoreRecordAnnotation, EventIdentityInput, EventType, NativeItemKey, SourceAnchorScope,
    SourceKey, StableEntityId, TypedKey, CORE_ACTIVITY_REVISION,
};
use serde::{Deserialize, Serialize};
use sha2::Digest;
#[cfg(any(test, feature = "test-support"))]
use std::{
    cell::Cell,
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use super::{
    discover_cursor_transcripts,
    layout::CursorTranscriptPath,
    parser::{
        project_cursor_jsonl_record, project_cursor_jsonl_record_with_rejection,
        CursorJsonlRecordOutcome, CursorRejectionKind,
    },
    projection::{CursorEventBody, CursorNativeEvent},
};
use crate::CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT;
use ctx_history_jsonl::{
    fit_jsonl_activity, selected_content_fits, JsonlActivityObservedBytes, JsonlFamilyAdapter,
    JsonlOversizedRecordPolicy, JsonlRecordRejections, SourceBackedRecordRejectionClass,
    SourceBackedRecordRejectionDrafts,
};
use ctx_history_provider_runtime::{
    read_bounded_record_unhashed, source_io::OpenedProviderSourceFile, CaptureError,
    JsonlFamilyAppendMode, JsonlFamilyProjectionMode, JsonlFamilyProjector,
    JsonlFamilyTerminalProof, JsonlOrderedAppendOccurrenceState, JsonlRecordFraming,
    JsonlRecordRef, ProviderBaseEventLookup, ProviderJsonlInventory, ProviderJsonlLeaf,
    ProviderJsonlReader, ProviderJsonlRuntime, ProviderJsonlWorkerContext, ProviderRuntimeBinding,
    Result,
};
use ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;

type JsonlFamilyLeaf = ProviderJsonlLeaf;
type JsonlReader = ProviderJsonlReader;
type JsonlFamilyWorkerContext<B> = ProviderJsonlWorkerContext<B>;

const SOURCE_ANCHOR_NAMESPACE: &str = "cursor.session";
const NATIVE_SESSION_NAMESPACE: &str = "cursor.session";
const NATIVE_EVENT_LOGICAL_KIND: &str = "cursor.logical-event-v3";
const LOGICAL_SESSION_KIND: &str = "cursor-session";
const LOGICAL_EVENT_KIND: &str = "cursor-event";
const SOURCE_SCHEMA_VARIANT: &str = "cursor-agent-transcript-jsonl-v1";
const PARSER_REVISION: &str = "cursor-shared-jsonl-core-activity-v2-top-level-role";
const EVENT_SEQUENCE_PARTS: u64 = u16::MAX as u64 + 1;

mod binding;

use binding::*;

#[cfg(any(test, feature = "test-support"))]
static CURSOR_PROJECTED_RECORDS: LazyLock<Mutex<HashMap<StableEntityId, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static CURSOR_SIGNATURE_RECORDS: Cell<u64> = const { Cell::new(0) };
    static CURSOR_BASE_IDENTITY_PROBES: Cell<u64> = const { Cell::new(0) };
}

#[cfg(feature = "test-support")]
pub(crate) fn reset_cursor_projected_records(source: &SourceKey) {
    CURSOR_PROJECTED_RECORDS
        .lock()
        .expect("Cursor projection counters must remain available")
        .insert(source.identity(), 0);
}

#[cfg(feature = "test-support")]
pub(crate) fn take_cursor_projected_records(source: &SourceKey) -> u64 {
    CURSOR_PROJECTED_RECORDS
        .lock()
        .expect("Cursor projection counters must remain available")
        .remove(&source.identity())
        .unwrap_or(0)
}

#[cfg(any(test, feature = "test-support"))]
fn observe_cursor_projected_record(source: &SourceKey) {
    let mut counters = CURSOR_PROJECTED_RECORDS
        .lock()
        .expect("Cursor projection counters must remain available");
    if let Some(counter) = counters.get_mut(&source.identity()) {
        *counter = counter
            .checked_add(1)
            .expect("Cursor projection test counter overflowed");
    }
}

#[cfg(feature = "test-support")]
pub(crate) fn reset_cursor_signature_records() {
    CURSOR_SIGNATURE_RECORDS.set(0);
}

#[cfg(feature = "test-support")]
pub(crate) fn cursor_signature_records() -> u64 {
    CURSOR_SIGNATURE_RECORDS.get()
}

#[cfg(feature = "test-support")]
pub(crate) fn reset_cursor_base_identity_probes() {
    CURSOR_BASE_IDENTITY_PROBES.set(0);
}

#[cfg(feature = "test-support")]
pub(crate) fn cursor_base_identity_probes() -> u64 {
    CURSOR_BASE_IDENTITY_PROBES.get()
}

#[derive(Debug)]
struct CursorJsonlAdapter<B> {
    source_anchor_scope: SourceAnchorScope,
    binding: std::marker::PhantomData<fn() -> B>,
}

pub(crate) fn cursor_jsonl_adapter<B>(
) -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    cursor_jsonl_adapter_with_source_root_lineage(None)
}

pub(crate) fn cursor_jsonl_adapter_with_source_root_lineage<B>(
    source_root_lineage: Option<[u8; 32]>,
) -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    Arc::new(CursorJsonlAdapter {
        source_anchor_scope: source_root_lineage
            .map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
        binding: std::marker::PhantomData,
    })
}

impl<B> JsonlFamilyAdapter for CursorJsonlAdapter<B>
where
    B: ProviderRuntimeBinding,
{
    type Runtime = ProviderJsonlRuntime<B>;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Cursor
    }

    fn source_format(&self) -> &'static str {
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::ProjectorPreflight(true)
    }

    fn oversized_record_policy(&self) -> JsonlOversizedRecordPolicy {
        JsonlOversizedRecordPolicy::RejectRecord
    }

    fn discover(&self, root: &Path) -> Result<ProviderJsonlInventory> {
        match fs::symlink_metadata(root) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return ProviderJsonlInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        }
        let inventory = discover_cursor_transcripts(root);
        if !inventory.completed {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "Cursor transcript inventory could not be completed",
            });
        }
        let authority = Arc::new(
            inventory
                .authority()
                .ok_or(CaptureError::InvalidProviderTranscriptPath {
                    path: root.to_path_buf(),
                    reason: "Cursor discovery has no retained source authority",
                })?
                .clone(),
        );
        let mut native_sessions = BTreeMap::<String, Vec<_>>::new();
        for transcript in inventory.transcripts {
            if transcript.authority().named_path() != authority.named_path()
                || transcript.authority().authority_fingerprint()
                    != authority.authority_fingerprint()
            {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            native_sessions
                .entry(transcript.native_session_id().to_owned())
                .or_default()
                .push(transcript);
        }
        let mut leaves = Vec::with_capacity(native_sessions.len());
        let mut exact_dependencies = Vec::new();
        for (native_session_id, mut routes) in native_sessions {
            routes.sort_by(|left, right| left.path().cmp(right.path()));
            let route_proofs = routes
                .iter()
                .map(|route| {
                    JsonlFamilyTerminalProof::exact_path(
                        route.path().to_path_buf(),
                        Arc::clone(&authority),
                        route.authority_relative_path().to_path_buf(),
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let logical_transcript_sha256 = if routes.len() > 1 {
                let signature = cursor_transcript_signature(&routes[0])?;
                for route in routes.iter().skip(1) {
                    if cursor_transcript_signature(route)? != signature {
                        return Err(CaptureError::InvalidPayload(format!(
                            "Cursor native session ID {native_session_id:?} has conflicting transcript copies"
                        )));
                    }
                }
                Some(signature)
            } else {
                None
            };
            for proof in &route_proofs {
                proof.revalidate_dependency()?;
            }
            let source = source_key_scoped(&native_session_id, self.source_anchor_scope)?;
            let selected = routes.remove(0);
            exact_dependencies.extend(route_proofs.into_iter().skip(1));
            let binding = CursorBinding {
                native_session_id,
                logical_transcript_sha256,
                selected_route_sha256: cursor_route_sha256(selected.path()),
                alias_route_sha256: routes
                    .iter()
                    .map(|route| cursor_route_sha256(route.path()))
                    .collect(),
            };
            leaves.push(ProviderJsonlLeaf::observe(
                source,
                selected.path().to_path_buf(),
                Arc::clone(&authority),
                selected.authority_relative_path().to_path_buf(),
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?,
            )?);
        }
        Ok(
            ProviderJsonlInventory::present(self.provider(), root, authority, leaves)?
                .with_exact_dependencies(exact_dependencies),
        )
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &ProviderJsonlLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
        _checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<ProviderBaseEventLookup<B>>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = ProviderJsonlRuntime<B>>>> {
        let binding = decode_binding(leaf)?;
        validate_binding(
            leaf,
            &binding,
            source_file.as_ref(),
            self.source_anchor_scope,
        )?;
        let session_id = session_id(leaf.source(), &binding.native_session_id)?;
        Ok(Box::new(CursorProjector {
            source: leaf.source().clone(),
            native_session_id: binding.native_session_id,
            session_id,
            event_identities: match (mode, base_event_lookup) {
                (JsonlFamilyProjectionMode::CertifiedAppend, Some(base_lookup)) => {
                    JsonlOrderedAppendOccurrenceState::for_append(base_lookup)
                }
                _ => JsonlOrderedAppendOccurrenceState::default(),
            },
            rejections: JsonlRecordRejections::new(
                leaf.source().clone(),
                CaptureProvider::Cursor,
                leaf.source_path().display().to_string(),
            ),
        }))
    }
}

struct CursorProjector<B: ProviderRuntimeBinding> {
    source: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
    event_identities:
        JsonlOrderedAppendOccurrenceState<CursorLogicalEventIdentity, ProviderBaseEventLookup<B>>,
    rejections: JsonlRecordRejections,
}

impl<B: ProviderRuntimeBinding> CursorProjector<B> {
    fn reject(&mut self, record: JsonlRecordRef<'_>, kind: CursorRejectionKind, detail: String) {
        let class = match kind {
            CursorRejectionKind::MalformedJson => SourceBackedRecordRejectionClass::MalformedRecord,
            CursorRejectionKind::UnsupportedShape => {
                SourceBackedRecordRejectionClass::UnsupportedRecord
            }
        };
        self.rejections.record(record, class, detail);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CursorLogicalEventIdentity {
    event_type: &'static str,
    role: &'static str,
    occurred_at_unix_ms: Option<i64>,
    content_sha256: [u8; 32],
}

impl CursorLogicalEventIdentity {
    fn from_event(event: &CursorNativeEvent) -> Self {
        Self {
            event_type: event.event_type.as_str(),
            role: event.role.as_str(),
            occurred_at_unix_ms: event
                .occurred_at
                .map(|occurred_at| occurred_at.timestamp_millis()),
            content_sha256: event.provider_event_hash,
        }
    }
}

impl<B> JsonlFamilyProjector for CursorProjector<B>
where
    B: ProviderRuntimeBinding,
{
    type Runtime = ProviderJsonlRuntime<B>;

    fn preflight(
        &mut self,
        reader: &mut JsonlReader,
        _certified_prefix_end: Option<u64>,
    ) -> Result<bool> {
        crate::consume_neutral_preflight(reader)?;
        Ok(false)
    }

    fn retry_replacement(&mut self) {
        self.event_identities = JsonlOrderedAppendOccurrenceState::default();
    }

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<B>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        #[cfg(any(test, feature = "test-support"))]
        observe_cursor_projected_record(&self.source);
        if record.oversized() {
            self.reject(
                record,
                CursorRejectionKind::MalformedJson,
                format!("Cursor record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"),
            );
            return Ok(());
        }
        let evidence = record.evidence();
        let events = match project_cursor_jsonl_record_with_rejection(
            record.bytes(),
            evidence.physical_ordinal(),
            evidence.physical_ordinal(),
            evidence.byte_start(),
            evidence.byte_end_exclusive(),
        )? {
            CursorJsonlRecordOutcome::Events(events) => events,
            CursorJsonlRecordOutcome::Ignored => return Ok(()),
            CursorJsonlRecordOutcome::Rejected(kind, detail) => {
                self.reject(record, kind, detail);
                return Ok(());
            }
        };
        for event in events {
            let duplicate_occurrence = next_event_occurrence::<B>(
                &event,
                &self.source,
                self.session_id,
                &mut self.event_identities,
            )?;
            let normalized_body = cursor_normalized_body(&event)?;
            let annotation = cursor_annotation(&event)?;
            if let Some(document) = core_record(
                &self.source,
                self.session_id,
                &self.native_session_id,
                event,
                duplicate_occurrence,
                CursorProjectedContent {
                    annotation,
                    normalized_body,
                },
            )? {
                emit(document)?;
            }
        }
        Ok(())
    }

    fn provider_checkpoint(&self) -> Result<Option<TypedKey>> {
        Ok(None)
    }

    fn rejected_records(&self) -> u64 {
        self.rejections.count()
    }

    fn take_record_rejections(&mut self) -> SourceBackedRecordRejectionDrafts {
        self.rejections.take_drafts()
    }
}

fn cursor_annotation(event: &CursorNativeEvent) -> Result<CoreRecordAnnotation> {
    let activity_at_unix_ms = event
        .occurred_at
        .map(|occurred_at| occurred_at.timestamp_millis());
    let structured_content = match &event.body {
        CursorEventBody::ToolCall {
            native_content,
            native_content_unavailable,
            ..
        }
        | CursorEventBody::ToolOutput {
            native_content,
            native_content_unavailable,
            ..
        } => (!native_content_unavailable).then(|| native_content.clone()),
        CursorEventBody::Text { .. } | CursorEventBody::None => None,
    };
    let mut provider_call_id = None;
    let mut invocation = None;
    let mut result = None;
    let mut facts = Vec::new();
    match &event.body {
        CursorEventBody::ToolCall {
            call_id,
            tool_name,
            arguments,
            protocol,
            server,
            explicit_tool,
            arguments_unavailable,
            mcp_identity_unavailable,
            literal_facts,
            ..
        } => {
            facts.extend(literal_facts.iter().cloned());
            if let (Some(call_id), Some(advertised_tool)) = (
                call_id.as_deref().filter(|value| !value.is_empty()),
                tool_name.as_deref().filter(|value| !value.is_empty()),
            ) {
                provider_call_id = Some(TypedKey::utf8(call_id).map_err(contract)?);
                let (protocol, server, tool) = exact_cursor_tool_identity(
                    advertised_tool,
                    protocol.as_deref(),
                    server.as_deref(),
                    explicit_tool.as_deref(),
                    *mcp_identity_unavailable,
                );
                invocation = Some(ActivityInvocation {
                    protocol,
                    server,
                    tool,
                    arguments: json_capture(arguments.as_ref(), *arguments_unavailable),
                    started_at_unix_ms: activity_at_unix_ms,
                });
            }
        }
        CursorEventBody::ToolOutput {
            call_id,
            native_content,
            content_unavailable,
            native_content_unavailable,
            literal_facts,
            ..
        } => {
            facts.extend(literal_facts.iter().cloned());
            if let Some(call_id) = call_id.as_deref().filter(|value| !value.is_empty()) {
                provider_call_id = Some(TypedKey::utf8(call_id).map_err(contract)?);
                result = Some(ActivityResult {
                    status: None,
                    completed_at_unix_ms: activity_at_unix_ms,
                    duration_ns: None,
                    text: cursor_result_text(native_content, *content_unavailable),
                    structured_content: if *native_content_unavailable {
                        ActivityJsonCapture::Unavailable
                    } else {
                        ActivityJsonCapture::Present {
                            value: native_content.clone(),
                        }
                    },
                });
            }
        }
        CursorEventBody::None | CursorEventBody::Text { .. } => {}
    }
    let activity =
        (invocation.is_some() || result.is_some() || !facts.is_empty()).then_some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id,
            invocation,
            result,
            facts,
        });
    Ok(CoreRecordAnnotation {
        activity,
        structured_content,
    })
}

fn exact_cursor_tool_identity(
    native_name: &str,
    protocol: Option<&str>,
    server: Option<&str>,
    explicit_tool: Option<&str>,
    unavailable: bool,
) -> (Option<String>, Option<String>, String) {
    if !unavailable {
        if let (Some("mcp"), Some(server), Some(tool)) = (protocol, server, explicit_tool) {
            return (
                Some("mcp".to_owned()),
                Some(server.to_owned()),
                tool.to_owned(),
            );
        }
    }
    (None, None, native_name.to_owned())
}

fn json_capture(value: Option<&serde_json::Value>, unavailable: bool) -> ActivityJsonCapture {
    if unavailable {
        ActivityJsonCapture::Unavailable
    } else {
        value.cloned().map_or(ActivityJsonCapture::Absent, |value| {
            ActivityJsonCapture::Present { value }
        })
    }
}

fn cursor_result_text(
    native_content: &serde_json::Value,
    unavailable: bool,
) -> ActivityTextCapture {
    if unavailable {
        ActivityTextCapture::Unavailable
    } else {
        match native_content.get("content") {
            Some(serde_json::Value::String(value)) => ActivityTextCapture::Present {
                value: value.clone(),
            },
            Some(_) | None => ActivityTextCapture::Absent,
        }
    }
}

fn cursor_transcript_signature(transcript: &CursorTranscriptPath) -> Result<[u8; 32]> {
    let source = transcript
        .authority()
        .open_file(transcript.authority_relative_path())?;
    let mut digest = sha2::Sha256::new();
    digest.update(b"ctx.cursor.logical-transcript.v1\0");
    let mut event_count = 0_u64;
    visit_cursor_events(&source, |event| {
        #[cfg(any(test, feature = "test-support"))]
        CURSOR_SIGNATURE_RECORDS.set(CURSOR_SIGNATURE_RECORDS.get().saturating_add(1));
        digest.update(event_count.to_be_bytes());
        digest.update(event.provider_event_hash);
        event_count = event_count
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Cursor logical transcript event count overflowed",
            ))?;
        Ok(())
    })?;
    digest.update(event_count.to_be_bytes());
    Ok(digest.finalize().into())
}

fn cursor_route_sha256(path: &Path) -> [u8; 32] {
    let mut digest = sha2::Sha256::new();
    digest.update(b"ctx.cursor.transcript-route.v1\0");
    digest.update(path.as_os_str().as_encoded_bytes());
    digest.finalize().into()
}

fn visit_cursor_events(
    source: &OpenedProviderSourceFile,
    mut visit: impl FnMut(CursorNativeEvent) -> Result<()>,
) -> Result<()> {
    let mut reader = BufReader::new(source.file().try_clone()?);
    let mut line = Vec::new();
    let mut physical_ordinal = 0_u64;
    let mut offset = 0_u64;
    let frozen_len = source.len();
    while offset < frozen_len {
        let record = read_bounded_record_unhashed(
            &mut reader,
            &mut line,
            frozen_len.saturating_sub(offset),
            JsonlRecordFraming::ordinary(),
            || CaptureError::SourceChangedDuringCapture,
        )?
        .ok_or(CaptureError::SourceChangedDuringCapture)?;
        offset = offset
            .checked_add(record.byte_len)
            .ok_or(CaptureError::SystemInvariant(
                "Cursor signature offset overflowed",
            ))?;
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if record.complete
            && !record.oversized
            && line.len() <= MAX_PROVIDER_JSONL_LINE_BYTES
            && !line.is_empty()
        {
            if let Some(events) = project_cursor_jsonl_record(
                &line,
                physical_ordinal,
                physical_ordinal,
                0,
                u64::try_from(line.len()).map_err(|_| {
                    CaptureError::InvalidPayload("Cursor line length exceeds u64".to_owned())
                })?,
            )? {
                for event in events {
                    visit(event)?;
                }
            }
        }
        physical_ordinal = physical_ordinal
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Cursor physical ordinal overflowed",
            ))?;
        if !record.complete {
            break;
        }
    }
    source.revalidate_leaf()
}

struct CursorProjectedContent {
    annotation: ctx_history_core::CoreRecordAnnotation,
    normalized_body: Option<String>,
}

fn core_record(
    source: &SourceKey,
    session_id: StableEntityId,
    native_session_id: &str,
    event: CursorNativeEvent,
    duplicate_occurrence: u64,
    content: CursorProjectedContent,
) -> Result<Option<CoreRecord>> {
    let Some(text) = content.normalized_body else {
        return Ok(None);
    };
    if text.is_empty() {
        return Ok(None);
    }
    let part_ordinal = event.native_order.part_ordinal;
    if part_ordinal > u32::from(u16::MAX) {
        return Err(CaptureError::InvalidPayload(
            "Cursor record exceeds the stable event-sequence part bound".to_owned(),
        ));
    }
    let native_event_key = event_identity_key(&event, duplicate_occurrence)?;
    let event_id = event_id(source, session_id, &native_event_key)?;
    let event_sequence = event
        .native_order
        .semantic_ordinal
        .checked_mul(EVENT_SEQUENCE_PARTS)
        .and_then(|base| base.checked_add(u64::from(part_ordinal)))
        .ok_or(CaptureError::SystemInvariant(
            "Cursor event sequence overflowed",
        ))?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        event_sequence,
        event.event_type.as_str(),
        PARSER_REVISION,
        text,
    )
    .map_err(contract)?;
    record.provider_session_id = Some(native_session_id.to_owned());
    record.native_event_id = Some(native_event_key);
    record.occurred_at_unix_ms = event
        .occurred_at
        .map(|occurred_at| occurred_at.timestamp_millis());
    record.role = Some(event.role.as_str().to_owned());
    record.agent_scope = Some(AgentScope::Primary);
    let mut structured_content = content.annotation.structured_content;
    let mut activity = content.annotation.activity;
    if !selected_content_fits(
        record
            .content
            .normalized_body
            .as_deref()
            .unwrap_or_default(),
        structured_content.as_ref(),
        activity.as_ref(),
        ctx_history_core::MAX_CORE_CONTENT_BYTES,
    ) {
        structured_content = None;
    }
    fit_jsonl_activity(
        record
            .content
            .normalized_body
            .as_deref()
            .unwrap_or_default(),
        structured_content.as_ref(),
        &mut activity,
        JsonlActivityObservedBytes::infer_from_present(),
        ctx_history_core::MAX_CORE_CONTENT_BYTES,
    );
    record.content.structured_content = structured_content;
    record.content.activity = activity;
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()
        .map_err(contract)?;
    record.validate_contract().map_err(contract)?;
    Ok(Some(record))
}

fn cursor_normalized_body(event: &CursorNativeEvent) -> Result<Option<String>> {
    match &event.body {
        CursorEventBody::Text { text } if event.event_type == EventType::Message => {
            Ok(Some(text.clone()))
        }
        CursorEventBody::ToolCall {
            native_content,
            native_content_unavailable,
            ..
        } => {
            if *native_content_unavailable {
                Ok(Some("Cursor tool call".to_owned()))
            } else {
                let text = serde_json::to_string(native_content)?;
                Ok(Some(
                    if text.len() <= ctx_history_core::MAX_CORE_CONTENT_BYTES {
                        text
                    } else {
                        "Cursor tool call".to_owned()
                    },
                ))
            }
        }
        CursorEventBody::ToolOutput {
            native_content,
            content_unavailable,
            ..
        } => {
            if *content_unavailable {
                Ok(Some("Cursor tool result".to_owned()))
            } else if let Some(text) = native_content
                .get("content")
                .and_then(serde_json::Value::as_str)
            {
                Ok(Some(
                    if text.len() <= ctx_history_core::MAX_CORE_CONTENT_BYTES {
                        text.to_owned()
                    } else {
                        "Cursor tool result".to_owned()
                    },
                ))
            } else {
                Ok(Some("Cursor tool result".to_owned()))
            }
        }
        CursorEventBody::None | CursorEventBody::Text { .. } => Ok(None),
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn source_key(native_session_id: &str) -> Result<SourceKey> {
    source_key_scoped(native_session_id, SourceAnchorScope::Unqualified)
}

pub(crate) fn source_key_scoped(
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<SourceKey> {
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::Cursor.as_str(),
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
        source_anchor_scope,
    )
    .map_err(contract)
}

fn session_id(source: &SourceKey, native_session_id: &str) -> Result<StableEntityId> {
    derive_native_session_id(
        source,
        LOGICAL_SESSION_KIND,
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)
}

fn event_id(
    source: &SourceKey,
    session_id: StableEntityId,
    native_event_key: &TypedKey,
) -> Result<StableEntityId> {
    let TypedKey::Composite(parts) = native_event_key else {
        return Err(contract("Cursor logical event key is not composite"));
    };
    let native_item_key =
        NativeItemKey::composite(NATIVE_EVENT_LOGICAL_KIND, parts.clone()).map_err(contract)?;
    derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(contract)
}

fn next_event_occurrence<B: ProviderRuntimeBinding>(
    event: &CursorNativeEvent,
    source: &SourceKey,
    session_id: StableEntityId,
    state: &mut JsonlOrderedAppendOccurrenceState<
        CursorLogicalEventIdentity,
        ProviderBaseEventLookup<B>,
    >,
) -> Result<u64> {
    let logical_identity = CursorLogicalEventIdentity::from_event(event);
    state.next(
        logical_identity,
        || CaptureError::SystemInvariant("Cursor duplicate event occurrence overflowed"),
        |base_lookup, occurrence| {
            base_occurrence_exists::<B>(base_lookup, source, session_id, event, occurrence)
        },
    )
}

fn base_occurrence_exists<B: ProviderRuntimeBinding>(
    base_lookup: &ProviderBaseEventLookup<B>,
    source: &SourceKey,
    session_id: StableEntityId,
    event: &CursorNativeEvent,
    duplicate_occurrence: u64,
) -> Result<bool> {
    #[cfg(any(test, feature = "test-support"))]
    CURSOR_BASE_IDENTITY_PROBES.set(CURSOR_BASE_IDENTITY_PROBES.get().saturating_add(1));
    let native_event_key = event_identity_key(event, duplicate_occurrence)?;
    let candidate = event_id(source, session_id, &native_event_key)?;
    // The pinned lookup also rejects duplicate base identities. Propagate that
    // error so an ambiguous/corrupt base can never select a new occurrence.
    base_lookup
        .contains(candidate.as_uuid())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn event_identity_key(event: &CursorNativeEvent, duplicate_occurrence: u64) -> Result<TypedKey> {
    // Cursor exposes no stable native ID for these blocks. Logical fields and
    // normalized native content form the key; occurrence distinguishes exact
    // repeats without making unrelated physical positions part of identity.
    TypedKey::composite(vec![
        TypedKey::utf8(NATIVE_EVENT_LOGICAL_KIND).map_err(contract)?,
        TypedKey::utf8(event.event_type.as_str()).map_err(contract)?,
        TypedKey::utf8(event.role.as_str()).map_err(contract)?,
        event.occurred_at.map_or(TypedKey::Null, |occurred_at| {
            TypedKey::I64(occurred_at.timestamp_millis())
        }),
        TypedKey::bytes(event.provider_event_hash.to_vec()).map_err(contract)?,
        TypedKey::U64(duplicate_occurrence),
    ])
    .map_err(contract)
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(format!("Cursor source-backed contract is invalid: {error}"))
}

#[cfg(test)]
mod tests;
