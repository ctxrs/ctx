//! Thin Cursor adapter for the shared certified-append JSONL family.

use std::{collections::BTreeMap, fs, io, path::Path, sync::Arc};

use chrono::{DateTime, Utc};
use ctx_history_capture_runtime::BaseEventLookup;
use ctx_history_core::{
    derive_event_id, derive_native_session_id, ActivityInvocation, ActivityJsonCapture,
    ActivityResult, ActivityTextCapture, AgentScope, CaptureProvider, CoreActivity, CoreRecord,
    CoreRecordAnnotation, EventIdentityInput, EventType, NativeItemKey, SourceAnchorScope,
    SourceKey, StableEntityId, TypedKey, CORE_ACTIVITY_REVISION,
};
use serde::{Deserialize, Serialize};
#[cfg(any(test, feature = "test-support"))]
use std::{
    cell::Cell,
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

#[cfg(test)]
use super::parser::project_cursor_jsonl_record;
use super::{
    discover_cursor_transcripts,
    parser::{
        project_cursor_jsonl_record_with_rejection, CursorJsonlRecordOutcome, CursorRejectionKind,
    },
    projection::{CursorEventBody, CursorNativeEvent},
};
use crate::CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT;
use ctx_history_jsonl::{
    fit_jsonl_activity, selected_content_fits, JsonlActivityObservedBytes, JsonlFamilyAdapter,
    JsonlFamilyRejectedLeaf, JsonlOversizedRecordPolicy, JsonlRecordRejections,
    SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDrafts,
};
use ctx_history_provider_runtime::{
    source_io::OpenedProviderSourceFile, CaptureError, JsonlFamilyAppendMode,
    JsonlFamilyProjectionMode, JsonlFamilyProjector, JsonlFamilyTerminalProof,
    JsonlFileObservation, JsonlOrderedAppendOccurrenceState, JsonlRecordRef,
    ProviderBaseEventLookup, ProviderJsonlInventory, ProviderJsonlLeaf, ProviderJsonlReader,
    ProviderJsonlRuntime, ProviderJsonlWorkerContext, ProviderRuntimeBinding, Result,
};
use ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;

type JsonlFamilyLeaf = ProviderJsonlLeaf;
type JsonlReader = ProviderJsonlReader;
type JsonlFamilyWorkerContext<B> = ProviderJsonlWorkerContext<B>;

const SOURCE_ANCHOR_NAMESPACE: &str = "cursor.session";
const DIVERGENT_COPY_ANCHOR_NAMESPACE: &str = "cursor.divergent-transcript-copy.v1";
const NATIVE_SESSION_NAMESPACE: &str = "cursor.session";
const NATIVE_EVENT_LOGICAL_KIND: &str = "cursor.logical-event-v3";
const LOGICAL_SESSION_KIND: &str = "cursor-session";
const LOGICAL_EVENT_KIND: &str = "cursor-event";
const SOURCE_SCHEMA_VARIANT: &str = "cursor-agent-transcript-jsonl-v1";
const PARSER_REVISION: &str = "cursor-shared-jsonl-core-activity-v2-top-level-role";
const EVENT_SEQUENCE_PARTS: u64 = u16::MAX as u64 + 1;

mod binding;
mod duplicates;

use binding::*;
use duplicates::*;

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
        let mut rejected_leaves = Vec::new();
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
            let duplicate_selection = (routes.len() > 1)
                .then(|| select_cursor_transcript(&routes))
                .transpose()?;
            for proof in &route_proofs {
                proof.revalidate_dependency()?;
            }
            let source = source_key_scoped(&native_session_id, self.source_anchor_scope)?;
            let selected_index = duplicate_selection
                .as_ref()
                .map_or(0, |selection| selection.selected_index);
            let logical_transcript_sha256 = duplicate_selection
                .as_ref()
                .map(|selection| selection.selected_signature);
            let alias_route_sha256 = routes
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != selected_index)
                .map(|(_, route)| cursor_route_sha256(route.path()))
                .collect::<Vec<_>>();
            if let Some(selection) = duplicate_selection.as_ref() {
                for index in selection.divergent_indices.iter().copied() {
                    let route = &routes[index];
                    let route_sha256 = cursor_route_sha256(route.path());
                    let proof = CursorDivergentAliasProof {
                        schema_version: 1,
                        native_session_id: native_session_id.clone(),
                        selected_signature: selection.selected_signature,
                        rejected_route_sha256: route_sha256,
                    };
                    rejected_leaves.push(
                        JsonlFamilyRejectedLeaf::bind_observed(
                            route.path().to_path_buf(),
                            route.authority_relative_path().to_path_buf(),
                            selection.route_observations[index].clone(),
                            TypedKey::bytes(serde_json::to_vec(&proof)?).map_err(contract)?,
                            0,
                        )
                        .with_logical_source_failure(
                            divergent_copy_source_key(route_sha256, self.source_anchor_scope)?,
                            "Cursor transcript copy diverges from another copy of the same session; ctx retained the deterministic copy with the most valid history and did not merge this alternative",
                        ),
                    );
                }
            }
            let selected = routes.remove(selected_index);
            exact_dependencies.extend(route_proofs.into_iter().enumerate().filter_map(
                |(index, proof)| {
                    (index != selected_index
                        && duplicate_selection
                            .as_ref()
                            .is_none_or(|selection| !selection.divergent_indices.contains(&index)))
                    .then_some(proof)
                },
            ));
            let binding = CursorBinding {
                native_session_id,
                logical_transcript_sha256,
                selected_route_sha256: cursor_route_sha256(selected.path()),
                alias_route_sha256,
            };
            let leaf = ProviderJsonlLeaf::observe(
                source,
                selected.path().to_path_buf(),
                Arc::clone(&authority),
                selected.authority_relative_path().to_path_buf(),
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?,
            )?;
            leaves.push(authenticate_selected_cursor_leaf(
                leaf,
                duplicate_selection
                    .as_ref()
                    .map(|selection| &selection.selected_observation),
            )?);
        }
        Ok(ProviderJsonlInventory::present_with_rejected(
            self.provider(),
            root,
            authority,
            leaves,
            rejected_leaves,
        )?
        .with_exact_dependencies(exact_dependencies))
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

fn authenticate_selected_cursor_leaf(
    leaf: ProviderJsonlLeaf,
    expected: Option<&JsonlFileObservation>,
) -> Result<ProviderJsonlLeaf> {
    if expected.is_some_and(|expected| leaf.observation() != expected) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(leaf)
}

fn divergent_copy_source_key(
    route_sha256: [u8; 32],
    scope: SourceAnchorScope,
) -> Result<SourceKey> {
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::Cursor.as_str(),
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        DIVERGENT_COPY_ANCHOR_NAMESPACE,
        TypedKey::bytes(route_sha256.to_vec()).map_err(contract)?,
        scope,
    )
    .map_err(contract)
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
