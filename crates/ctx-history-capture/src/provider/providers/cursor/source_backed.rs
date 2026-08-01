//! Thin Cursor adapter for the shared certified-append JSONL family.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, BufReader, Read},
    path::Path,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, EventIdentityInput,
    EventType, NativeItemKey, NativeSessionKey, RepositoryAbstention, RepositoryAbstentionReason,
    RepositoryEvidenceKind, RepositoryFileObservationKind, SessionIdentityInput, SourceAnchor,
    SourceKey, StableEntityId, TypedKey, MAX_CORE_CONTENT_BYTES,
};
use ctx_history_index::BaseEventIdentityLookup;
use serde::{Deserialize, Serialize};
use sha2::Digest;
#[cfg(test)]
use std::cell::Cell;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use super::{
    discover_cursor_transcripts,
    layout::CursorTranscriptPath,
    parser::project_cursor_jsonl_record,
    projection::{CursorEventBody, CursorNativeEvent},
};
use crate::{
    common::io::OpenedProviderSourceFile,
    provider::source_backed::family::jsonl::{
        JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
        JsonlFamilyProjector, JsonlRecordRef,
    },
    CaptureError, Result, CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "cursor.session";
const NATIVE_SESSION_NAMESPACE: &str = "cursor.session";
const NATIVE_EVENT_LOGICAL_KIND: &str = "cursor.logical-event-v3";
const LOGICAL_SESSION_KIND: &str = "cursor-session";
const LOGICAL_EVENT_KIND: &str = "cursor-event";
const SOURCE_SCHEMA_VARIANT: &str = "cursor-agent-transcript-jsonl-v1";
const PARSER_REVISION: &str = "cursor-shared-jsonl-v5-complete-logical-occurrence";
const EVENT_SEQUENCE_PARTS: u64 = u16::MAX as u64 + 1;
const MAX_CURSOR_TOOL_CONTEXTS: usize = 256;

#[cfg(test)]
static CURSOR_PROJECTED_RECORDS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    static CURSOR_SIGNATURE_RECORDS: Cell<u64> = const { Cell::new(0) };
    static CURSOR_BASE_IDENTITY_PROBES: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
fn reset_cursor_projected_records() {
    CURSOR_PROJECTED_RECORDS.store(0, Ordering::SeqCst);
}

#[cfg(test)]
fn cursor_projected_records() -> u64 {
    CURSOR_PROJECTED_RECORDS.load(Ordering::SeqCst)
}

#[cfg(test)]
fn reset_cursor_signature_records() {
    CURSOR_SIGNATURE_RECORDS.set(0);
}

#[cfg(test)]
fn cursor_signature_records() -> u64 {
    CURSOR_SIGNATURE_RECORDS.get()
}

#[cfg(test)]
fn reset_cursor_base_identity_probes() {
    CURSOR_BASE_IDENTITY_PROBES.set(0);
}

#[cfg(test)]
fn cursor_base_identity_probes() -> u64 {
    CURSOR_BASE_IDENTITY_PROBES.get()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorBinding {
    native_session_id: String,
    logical_transcript_sha256: Option<[u8; 32]>,
    selected_route_sha256: [u8; 32],
    alias_route_sha256: Vec<[u8; 32]>,
}

#[derive(Debug, Clone, Copy)]
struct CursorJsonlAdapter;

pub(crate) fn cursor_jsonl_adapter() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(CursorJsonlAdapter)
}

impl JsonlFamilyAdapter for CursorJsonlAdapter {
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
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        match fs::symlink_metadata(root) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
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
        for (native_session_id, mut routes) in native_sessions {
            routes.sort_by(|left, right| left.path().cmp(right.path()));
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
            let source = source_key(&native_session_id)?;
            let selected = routes.remove(0);
            let binding = CursorBinding {
                native_session_id,
                logical_transcript_sha256,
                selected_route_sha256: cursor_route_sha256(selected.path()),
                alias_route_sha256: routes
                    .iter()
                    .map(|route| cursor_route_sha256(route.path()))
                    .collect(),
            };
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                selected.path().to_path_buf(),
                Arc::clone(&authority),
                selected.authority_relative_path().to_path_buf(),
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        self.projector_with_provider_checkpoint(leaf, source_file, imported_at, None, None)
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<BaseEventIdentityLookup>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        validate_cursor_provider_checkpoint(checkpoint)?;
        let binding = decode_binding(leaf)?;
        validate_binding(leaf, &binding, source_file.as_ref())?;
        let session_id = session_id(leaf.source(), &binding.native_session_id)?;
        Ok(Box::new(CursorProjector {
            source: leaf.source().clone(),
            native_session_id: binding.native_session_id,
            session_id,
            repository_attributor: crate::repository_attribution::RepositoryAttributor::default(),
            tool_contexts: BTreeMap::new(),
            linkage_capacity_exceeded: false,
            event_identities: CursorEventIdentityState::new(base_event_lookup),
        }))
    }
}

fn validate_cursor_provider_checkpoint(checkpoint: Option<&TypedKey>) -> Result<()> {
    if checkpoint.is_some() {
        return Err(CaptureError::InvalidPayload(
            "Cursor received unexpected provider checkpoint state".to_owned(),
        ));
    }
    Ok(())
}

struct CursorProjector {
    source: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
    repository_attributor: crate::repository_attribution::RepositoryAttributor,
    tool_contexts: BTreeMap<String, CursorToolContextState>,
    linkage_capacity_exceeded: bool,
    event_identities: CursorEventIdentityState,
}

#[derive(Default)]
struct CursorEventIdentityState {
    base_lookup: Option<BaseEventIdentityLookup>,
    next_occurrences: BTreeMap<CursorLogicalEventIdentity, u64>,
}

impl CursorEventIdentityState {
    fn new(base_lookup: Option<BaseEventIdentityLookup>) -> Self {
        Self {
            base_lookup,
            next_occurrences: BTreeMap::new(),
        }
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

#[derive(Debug, Clone)]
struct CursorToolContext {
    command: Option<String>,
    declared_workdir: Option<String>,
    input_paths: Vec<String>,
}

#[derive(Debug, Clone)]
enum CursorToolContextState {
    Exact(CursorToolContext),
    Ambiguous,
}

impl JsonlFamilyProjector for CursorProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        #[cfg(test)]
        CURSOR_PROJECTED_RECORDS.fetch_add(1, Ordering::SeqCst);
        let evidence = record.evidence();
        let Some(events) = project_cursor_jsonl_record(
            record.bytes(),
            evidence.physical_ordinal(),
            evidence.physical_ordinal(),
            evidence.byte_start(),
            evidence.byte_end_exclusive(),
        )?
        else {
            return Ok(());
        };
        for event in events {
            let duplicate_occurrence = next_event_occurrence(
                &event,
                &self.source,
                self.session_id,
                &mut self.event_identities,
            )?;
            let attribution = self.attribution_for_event(&event);
            if let Some(document) = core_record(
                &self.source,
                self.session_id,
                &self.native_session_id,
                event,
                duplicate_occurrence,
                attribution,
            )? {
                emit(document)?;
            }
        }
        Ok(())
    }
}

impl CursorProjector {
    fn attribution_for_event(
        &mut self,
        event: &CursorNativeEvent,
    ) -> ctx_history_core::CoreRecordAnnotation {
        let activity_at_unix_ms = event
            .occurred_at
            .map(|occurred_at| occurred_at.timestamp_millis());
        let structured_content = match &event.body {
            CursorEventBody::ToolCall { native_content, .. }
            | CursorEventBody::ToolOutput { native_content, .. } => Some(native_content.clone()),
            CursorEventBody::Text { .. } | CursorEventBody::None => None,
        };
        let mut input = crate::repository_attribution::AttributionInput {
            activity_at_unix_ms,
            structured_content,
            ..crate::repository_attribution::AttributionInput::default()
        };
        let mut adapter_abstentions = Vec::new();
        match &event.body {
            CursorEventBody::ToolCall {
                call_id,
                command,
                declared_workdir,
                input_paths,
                ambiguous_native_fields,
                ..
            } => {
                let context = CursorToolContext {
                    command: command.clone(),
                    declared_workdir: declared_workdir.clone(),
                    input_paths: input_paths.clone(),
                };
                apply_cursor_context(&mut input, &context);
                if *ambiguous_native_fields {
                    adapter_abstentions.push((
                        RepositoryEvidenceKind::ProviderNativeResult,
                        RepositoryAbstentionReason::Ambiguous,
                        "cursor_tool_native_fields_are_ambiguous",
                    ));
                }
                if let Some(call_id) = call_id.as_ref() {
                    if self.tool_contexts.contains_key(call_id) {
                        self.tool_contexts
                            .insert(call_id.clone(), CursorToolContextState::Ambiguous);
                    } else if self.tool_contexts.len() < MAX_CURSOR_TOOL_CONTEXTS {
                        let state = if *ambiguous_native_fields {
                            CursorToolContextState::Ambiguous
                        } else {
                            CursorToolContextState::Exact(context)
                        };
                        self.tool_contexts.insert(call_id.clone(), state);
                    } else {
                        self.linkage_capacity_exceeded = true;
                    }
                } else {
                    adapter_abstentions.push((
                        RepositoryEvidenceKind::ProviderNativeResult,
                        RepositoryAbstentionReason::ProviderOutputUnjoined,
                        "cursor_tool_call_has_no_exact_result_link_id",
                    ));
                }
            }
            CursorEventBody::ToolOutput {
                call_id,
                ambiguous_linkage,
                ..
            } => {
                let context = if *ambiguous_linkage {
                    None
                } else {
                    call_id
                        .as_ref()
                        .and_then(|call_id| self.tool_contexts.remove(call_id))
                        .and_then(|state| match state {
                            CursorToolContextState::Exact(context) => Some(context),
                            CursorToolContextState::Ambiguous => None,
                        })
                };
                if let Some(context) = context {
                    apply_cursor_context(&mut input, &context);
                    input
                        .outcome_abstentions
                        .extend(cursor_outcome_abstentions(&context));
                } else {
                    let (reason, detail) = if self.linkage_capacity_exceeded {
                        (
                            RepositoryAbstentionReason::LinkageCapacityExceeded,
                            "cursor_tool_result_linkage_capacity_exceeded",
                        )
                    } else {
                        (
                            RepositoryAbstentionReason::ProviderOutputUnjoined,
                            "cursor_tool_result_has_no_exact_unique_call_link",
                        )
                    };
                    adapter_abstentions.push((
                        RepositoryEvidenceKind::ProviderNativeResult,
                        reason,
                        detail,
                    ));
                }
            }
            CursorEventBody::None | CursorEventBody::Text { .. } => {}
        }
        let mut annotation = self.repository_attributor.attribute(input);
        append_adapter_abstentions(&mut annotation, adapter_abstentions);
        annotation
    }
}

fn append_adapter_abstentions(
    annotation: &mut ctx_history_core::CoreRecordAnnotation,
    abstentions: Vec<(
        RepositoryEvidenceKind,
        RepositoryAbstentionReason,
        &'static str,
    )>,
) {
    for (evidence_kind, reason, detail) in abstentions {
        let abstention = RepositoryAbstention {
            evidence_kind,
            reason,
            detail: Some(detail.to_owned()),
            association_policy_revision: crate::repository_attribution::ASSOCIATION_POLICY_REVISION,
        };
        if !annotation.repository_abstentions.contains(&abstention) {
            annotation.repository_abstentions.push(abstention);
        }
    }
}

fn apply_cursor_context(
    input: &mut crate::repository_attribution::AttributionInput,
    context: &CursorToolContext,
) {
    input.command = context.command.clone();
    input.declared_tool_workdir = context.declared_workdir.clone();
    input
        .file_observations
        .extend(context.input_paths.iter().cloned().map(|path| {
            crate::repository_attribution::UnscopedFileObservation {
                path,
                prior_path: None,
                kind: RepositoryFileObservationKind::Unknown,
            }
        }));
}

fn cursor_outcome_abstentions(
    context: &CursorToolContext,
) -> Vec<(RepositoryAbstentionReason, &'static str)> {
    let Some(command) = context.command.as_deref() else {
        return Vec::new();
    };
    let base = context
        .declared_workdir
        .as_deref()
        .and_then(|path| crate::repository_attribution::lexical_absolute(path, None));
    let plan = match base.as_deref() {
        Some(base) => crate::repository_attribution::bounded_outcome_plan(command, base),
        None => {
            let provisional =
                crate::repository_attribution::bounded_outcome_plan(command, Path::new("/"));
            if matches!(
                provisional,
                crate::repository_attribution::BoundedOutcomePlanDisposition::Planned(_)
            ) {
                return vec![(
                    RepositoryAbstentionReason::OutcomeRepositoryUnbound,
                    "cursor_outcome_command_has_no_bounded_base",
                )];
            }
            provisional
        }
    };
    match plan {
        crate::repository_attribution::BoundedOutcomePlanDisposition::Unrecognized => Vec::new(),
        crate::repository_attribution::BoundedOutcomePlanDisposition::Abstained {
            reason,
            detail,
            ..
        } => vec![(reason, detail)],
        crate::repository_attribution::BoundedOutcomePlanDisposition::Planned(plan) => {
            let rewrite = matches!(
                plan.operation,
                crate::repository_attribution::BoundedOutcomeOperation::Commit {
                    rewrites_history: true,
                    ..
                }
            );
            if rewrite {
                vec![(
                    RepositoryAbstentionReason::HistoryRewriteUnlinked,
                    "cursor_result_has_no_exact_structured_replacement_lineage",
                )]
            } else {
                vec![(
                    RepositoryAbstentionReason::OutcomeResultInadmissible,
                    "cursor_result_has_no_exact_structured_repository_outcome",
                )]
            }
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
        #[cfg(test)]
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
    while read_cursor_line(&mut reader, &mut line)? {
        if !line.is_empty() {
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
    }
    source.revalidate_leaf()
}

fn read_cursor_line(reader: &mut BufReader<std::fs::File>, line: &mut Vec<u8>) -> io::Result<bool> {
    line.clear();
    let limit = MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2);
    let read = {
        let mut bounded = (&mut *reader).take(u64::try_from(limit).unwrap_or(u64::MAX));
        bounded.read_until(b'\n', line)?
    };
    if read == 0 {
        return Ok(false);
    }
    if !line.ends_with(b"\n") {
        discard_through_newline(reader)?;
        line.clear();
        return Ok(true);
    }
    line.pop();
    if line.ends_with(b"\r") {
        line.pop();
    }
    if line.len() > MAX_PROVIDER_JSONL_LINE_BYTES {
        line.clear();
    }
    Ok(true)
}

fn discard_through_newline(reader: &mut BufReader<std::fs::File>) -> io::Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position.saturating_add(1));
        let reached_newline = available.get(consumed.saturating_sub(1)) == Some(&b'\n');
        reader.consume(consumed);
        if reached_newline {
            return Ok(());
        }
    }
}

fn core_record(
    source: &SourceKey,
    session_id: StableEntityId,
    native_session_id: &str,
    event: CursorNativeEvent,
    duplicate_occurrence: u64,
    annotation: ctx_history_core::CoreRecordAnnotation,
) -> Result<Option<CoreRecord>> {
    let text = match &event.body {
        CursorEventBody::Text { text } if event.event_type == EventType::Message => text.clone(),
        CursorEventBody::ToolCall { native_content, .. }
        | CursorEventBody::ToolOutput { native_content, .. } => {
            serde_json::to_string(native_content)?
        }
        CursorEventBody::None | CursorEventBody::Text { .. } => return Ok(None),
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
    let normalized_body_bytes = text.len();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        event_sequence,
        event.event_type.as_str(),
        AgentType::Primary.as_str(),
        true,
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
    record.content.structured_content = annotation.structured_content.and_then(|structured| {
        serde_json::to_vec(&structured)
            .ok()
            .and_then(|encoded| {
                normalized_body_bytes
                    .checked_add(encoded.len())
                    .filter(|bytes| *bytes <= MAX_CORE_CONTENT_BYTES)
            })
            .map(|_| structured)
    });
    record.metadata = annotation.metadata;
    record.repository_candidate_evidence = annotation.repository_candidate_evidence;
    record.repository_bindings = annotation.repository_bindings;
    record.repository_abstentions = annotation.repository_abstentions;
    record.repository_file_observations = annotation.repository_file_observations;
    record.repository_vcs_observations = annotation.repository_vcs_observations;
    record.validate_contract().map_err(contract)?;
    Ok(Some(record))
}

fn source_key(native_session_id: &str) -> Result<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)?;
    SourceKey::derive(
        CaptureProvider::Cursor.as_str(),
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )
    .map_err(contract)
}

fn session_id(source: &SourceKey, native_session_id: &str) -> Result<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })
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

fn next_event_occurrence(
    event: &CursorNativeEvent,
    source: &SourceKey,
    session_id: StableEntityId,
    state: &mut CursorEventIdentityState,
) -> Result<u64> {
    let logical_identity = CursorLogicalEventIdentity::from_event(event);
    let occurrence = match state.next_occurrences.get(&logical_identity).copied() {
        Some(occurrence) => occurrence,
        None => {
            first_unused_base_occurrence(state.base_lookup.as_ref(), source, session_id, event)?
        }
    };
    let next = occurrence
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "Cursor duplicate event occurrence overflowed",
        ))?;
    state.next_occurrences.insert(logical_identity, next);
    Ok(occurrence)
}

fn first_unused_base_occurrence(
    base_lookup: Option<&BaseEventIdentityLookup>,
    source: &SourceKey,
    session_id: StableEntityId,
    event: &CursorNativeEvent,
) -> Result<u64> {
    let Some(base_lookup) = base_lookup else {
        return Ok(0);
    };
    if !base_occurrence_exists(base_lookup, source, session_id, event, 0)? {
        return Ok(0);
    }

    let mut present = 0_u64;
    let mut missing = 1_u64;
    while base_occurrence_exists(base_lookup, source, session_id, event, missing)? {
        present = missing;
        missing = match missing.checked_mul(2) {
            Some(next) => next,
            None if missing != u64::MAX => u64::MAX,
            None => {
                return Err(CaptureError::SystemInvariant(
                    "Cursor duplicate event occurrence overflowed",
                ));
            }
        };
    }
    while present.saturating_add(1) < missing {
        let candidate = present + (missing - present) / 2;
        if base_occurrence_exists(base_lookup, source, session_id, event, candidate)? {
            present = candidate;
        } else {
            missing = candidate;
        }
    }
    Ok(missing)
}

fn base_occurrence_exists(
    base_lookup: &BaseEventIdentityLookup,
    source: &SourceKey,
    session_id: StableEntityId,
    event: &CursorNativeEvent,
    duplicate_occurrence: u64,
) -> Result<bool> {
    #[cfg(test)]
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

fn validate_binding(
    leaf: &JsonlFamilyLeaf,
    binding: &CursorBinding,
    _source_file: &OpenedProviderSourceFile,
) -> Result<()> {
    if !source_key(&binding.native_session_id)?.exact_descriptor_eq(leaf.source())
        || cursor_route_sha256(leaf.source_path()) != binding.selected_route_sha256
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> Result<CursorBinding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(contract("Cursor family binding is malformed"));
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(format!("Cursor source-backed contract is invalid: {error}"))
}

#[cfg(test)]
mod tests;
