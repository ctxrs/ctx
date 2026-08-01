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
const NATIVE_EVENT_LOGICAL_KIND: &str = "cursor.logical-event-v2";
const LOGICAL_SESSION_KIND: &str = "cursor-session";
const LOGICAL_EVENT_KIND: &str = "cursor-event";
const SOURCE_SCHEMA_VARIANT: &str = "cursor-agent-transcript-jsonl-v1";
const PARSER_REVISION: &str = "cursor-shared-jsonl-v4-complete-logical-identity";
const EVENT_SEQUENCE_PARTS: u64 = u16::MAX as u64 + 1;
const MAX_CURSOR_TOOL_CONTEXTS: usize = 256;

#[cfg(test)]
static CURSOR_PROJECTED_RECORDS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    static CURSOR_SIGNATURE_RECORDS: Cell<u64> = const { Cell::new(0) };
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
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
        }))
    }
}

struct CursorProjector {
    source: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
    repository_attributor: crate::repository_attribution::RepositoryAttributor,
    tool_contexts: BTreeMap<String, CursorToolContextState>,
    linkage_capacity_exceeded: bool,
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
            let attribution = self.attribution_for_event(&event);
            if let Some(document) = core_record(
                &self.source,
                self.session_id,
                &self.native_session_id,
                event,
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
        digest.update(event.provider_event_hash.as_bytes());
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
    let native_event_key = event_identity_key(&event)?;
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

fn event_identity_key(event: &CursorNativeEvent) -> Result<TypedKey> {
    // Cursor exposes no stable native ID for these blocks. Logical content and
    // fields therefore form the key. An exact duplicate deliberately collides
    // and makes publication fail closed instead of smuggling position into ID.
    TypedKey::composite(vec![
        TypedKey::utf8(NATIVE_EVENT_LOGICAL_KIND).map_err(contract)?,
        TypedKey::utf8(event.event_type.as_str()).map_err(contract)?,
        TypedKey::utf8(event.role.as_str()).map_err(contract)?,
        event.occurred_at.map_or(TypedKey::Null, |occurred_at| {
            TypedKey::I64(occurred_at.timestamp_millis())
        }),
        TypedKey::utf8(&event.provider_event_hash).map_err(contract)?,
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
mod repository_tests {
    use std::{fs, path::PathBuf, process::Command};

    use tempfile::TempDir;

    use super::*;

    fn repository(temp: &TempDir) -> PathBuf {
        let path = temp.path().join("repo");
        fs::create_dir(&path).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(&path)
            .status()
            .unwrap()
            .success());
        fs::create_dir(path.join("src")).unwrap();
        fs::write(path.join("src/lib.rs"), "pub fn native() {}\n").unwrap();
        path
    }

    fn event(value: &str, ordinal: u64) -> CursorNativeEvent {
        project_cursor_jsonl_record(value.as_bytes(), ordinal, ordinal, 0, value.len() as u64)
            .unwrap()
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    }

    fn projector() -> CursorProjector {
        let native_session_id = "cursor-native-contract-test".to_owned();
        let source = source_key(&native_session_id).unwrap();
        let session_id = session_id(&source, &native_session_id).unwrap();
        CursorProjector {
            source,
            native_session_id,
            session_id,
            repository_attributor: crate::repository_attribution::RepositoryAttributor::default(),
            tool_contexts: BTreeMap::new(),
            linkage_capacity_exceeded: false,
        }
    }

    fn has_reason(
        annotation: &ctx_history_core::CoreRecordAnnotation,
        reason: RepositoryAbstentionReason,
    ) -> bool {
        annotation
            .repository_abstentions
            .iter()
            .any(|abstention| abstention.reason == reason)
    }

    #[test]
    fn cursor_exact_native_tool_fields_and_result_id_bind_without_fabricating_outcomes() {
        let temp = TempDir::new().unwrap();
        let repo = repository(&temp);
        let call = serde_json::json!({
            "timestamp": "2026-07-31T12:00:00Z",
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "call-1",
                "name": "run_shell_command",
                "input": {
                    "command": "git commit -m bounded",
                    "workdir": repo,
                    "path": "src/lib.rs"
                }
            }]}
        })
        .to_string();
        let result = r#"{"timestamp":"2026-07-31T12:00:01Z","role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"untrusted prose oid deadbeef"}]}}"#;
        let mut projector = projector();

        let call_annotation = projector.attribution_for_event(&event(&call, 1));
        assert_eq!(call_annotation.repository_bindings.len(), 1);
        assert_eq!(
            call_annotation
                .repository_candidate_evidence
                .declared_tool_workdir
                .as_deref(),
            Some(repo.to_string_lossy().as_ref())
        );
        assert_eq!(
            call_annotation.repository_file_observations[0].relative_path,
            "src/lib.rs"
        );

        let result_annotation = projector.attribution_for_event(&event(result, 2));
        assert_eq!(result_annotation.repository_bindings.len(), 1);
        assert!(result_annotation.repository_vcs_observations.is_empty());
        assert!(has_reason(
            &result_annotation,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        ));
        assert!(!has_reason(
            &result_annotation,
            RepositoryAbstentionReason::ProviderOutputUnjoined
        ));
    }

    #[test]
    fn cursor_dynamic_ambiguous_and_rewrite_evidence_abstains_with_typed_reasons() {
        let temp = TempDir::new().unwrap();
        let repo = repository(&temp);
        let mut projector = projector();
        let dynamic = serde_json::json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "dynamic",
                "name": "run_shell_command",
                "input": {"command": "cd $REPO && git status", "path": "$REPO/src/lib.rs"}
            }]}
        })
        .to_string();
        let dynamic_annotation = projector.attribution_for_event(&event(&dynamic, 1));
        assert!(dynamic_annotation.repository_bindings.is_empty());
        assert!(has_reason(
            &dynamic_annotation,
            RepositoryAbstentionReason::DynamicPath
        ));

        let rewrite = serde_json::json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "rewrite",
                "name": "run_shell_command",
                "input": {"command": "git commit --amend --no-edit", "workdir": repo}
            }]}
        })
        .to_string();
        projector.attribution_for_event(&event(&rewrite, 2));
        let rewrite_result = r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"rewrite","content":"success without structured replacement lineage"}]}}"#;
        let rewrite_annotation = projector.attribution_for_event(&event(rewrite_result, 3));
        assert!(rewrite_annotation.repository_vcs_observations.is_empty());
        assert!(has_reason(
            &rewrite_annotation,
            RepositoryAbstentionReason::HistoryRewriteUnlinked
        ));

        let pr = serde_json::json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "pr",
                "name": "run_shell_command",
                "input": {
                    "command": "gh pr create --title bounded --body bounded",
                    "workdir": repo
                }
            }]}
        })
        .to_string();
        projector.attribution_for_event(&event(&pr, 4));
        let pr_result = r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"pr","content":"URL prose is not structured outcome authority"}]}}"#;
        let pr_annotation = projector.attribution_for_event(&event(pr_result, 5));
        assert!(pr_annotation.repository_vcs_observations.is_empty());
        assert!(has_reason(
            &pr_annotation,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        ));

        let ambiguous_result = r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"rewrite","tool_use_id":"other","content":"ignored"}]}}"#;
        let ambiguous_annotation = projector.attribution_for_event(&event(ambiguous_result, 6));
        assert!(has_reason(
            &ambiguous_annotation,
            RepositoryAbstentionReason::ProviderOutputUnjoined
        ));
    }

    #[test]
    fn cursor_synthetic_native_contract_does_not_establish_real_history_parity() {
        let mut projector = projector();
        let relative_only = r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"write_file","input":{"path":"src/unproven.rs"}}]}}"#;
        let annotation = projector.attribution_for_event(&event(relative_only, 1));
        assert!(annotation.repository_bindings.is_empty());
        assert!(annotation.repository_vcs_observations.is_empty());
        assert!(has_reason(
            &annotation,
            RepositoryAbstentionReason::UnscopedFileActivity
        ));
    }
}

#[cfg(test)]
mod fidelity_identity_tests {
    use std::{
        collections::BTreeMap,
        fs::{self, OpenOptions},
        io::Write,
        path::PathBuf,
    };

    use ctx_history_core::{CoreRecord, StableEntityId};
    use ctx_history_index::WriterOptions;
    use serde_json::{json, Value};
    use tempfile::TempDir;

    use super::*;
    use crate::{
        provider::source_backed::{
            refresh_source_backed_generation, register_landed_source_backed_route,
            SourceBackedProviderRegistry, SourceBackedRouteSelection,
        },
        ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
        ProviderSourceStatus,
    };

    fn transcript_path(root: &Path, project: &str, session: &str) -> PathBuf {
        root.join("projects")
            .join(project)
            .join("agent-transcripts")
            .join(session)
            .join(format!("{session}.jsonl"))
    }

    fn write_transcript(path: &Path, rows: &[Value]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut encoded = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut encoded, row).unwrap();
            encoded.push(b'\n');
        }
        fs::write(path, encoded).unwrap();
    }

    fn append_transcript(path: &Path, row: &Value) {
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        serde_json::to_writer(&mut file, row).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_all().unwrap();
    }

    fn registry(root: &Path) -> SourceBackedProviderRegistry {
        let mut registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route(
            &mut registry,
            ProviderSource {
                provider: CaptureProvider::Cursor,
                path: root.to_path_buf(),
                exists: true,
                source_format: CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
            },
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();
        registry
    }

    fn writer_options() -> WriterOptions {
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        }
    }

    fn event(row: &Value, ordinal: u64) -> CursorNativeEvent {
        let encoded = serde_json::to_vec(row).unwrap();
        project_cursor_jsonl_record(&encoded, ordinal, ordinal, 0, encoded.len() as u64)
            .unwrap()
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    }

    fn projector() -> CursorProjector {
        let native_session_id = "cursor-fidelity-test".to_owned();
        let source = source_key(&native_session_id).unwrap();
        let session_id = session_id(&source, &native_session_id).unwrap();
        CursorProjector {
            source,
            native_session_id,
            session_id,
            repository_attributor: crate::repository_attribution::RepositoryAttributor::default(),
            tool_contexts: BTreeMap::new(),
            linkage_capacity_exceeded: false,
        }
    }

    fn projected_core(row: &Value) -> CoreRecord {
        let mut projector = projector();
        let event = event(row, 0);
        let annotation = projector.attribution_for_event(&event);
        core_record(
            &projector.source,
            projector.session_id,
            &projector.native_session_id,
            event,
            annotation,
        )
        .unwrap()
        .unwrap()
    }

    fn message(role: &str, timestamp: &str, text: &str) -> Value {
        json!({
            "timestamp": timestamp,
            "role": role,
            "message": {
                "role": role,
                "content": [{"type": "text", "text": text}]
            }
        })
    }

    fn event_ids(rows: &[Value]) -> Vec<StableEntityId> {
        let native_session_id = "cursor-identity-test";
        let source = source_key(native_session_id).unwrap();
        let session_id = session_id(&source, native_session_id).unwrap();
        let mut ids = Vec::new();
        for (ordinal, row) in rows.iter().enumerate() {
            let encoded = serde_json::to_vec(row).unwrap();
            let events = project_cursor_jsonl_record(
                &encoded,
                ordinal as u64,
                ordinal as u64,
                0,
                encoded.len() as u64,
            )
            .unwrap()
            .unwrap();
            for event in events {
                let key = event_identity_key(&event).unwrap();
                ids.push(event_id(&source, session_id, &key).unwrap());
            }
        }
        ids
    }

    #[test]
    fn cursor_write_file_core_content_preserves_complete_input() {
        let row = json!({
            "timestamp": "2026-07-31T12:00:00Z",
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "write-1",
                "name": "write_file",
                "input": {
                    "path": "src/main.rs",
                    "contents": "fn main() { println!(\"complete\"); }\n",
                    "overwrite": true
                }
            }]}
        });
        let expected = row.pointer("/message/content/0").unwrap().clone();
        let core = projected_core(&row);

        assert_eq!(core.content.structured_content, Some(expected.clone()));
        assert_eq!(
            serde_json::from_str::<Value>(core.content.normalized_body.as_deref().unwrap())
                .unwrap(),
            expected
        );
        assert!(core
            .content
            .normalized_body
            .as_deref()
            .unwrap()
            .contains("println!"));
    }

    #[test]
    fn cursor_shell_result_core_content_preserves_complete_stdout() {
        let stdout = "first line\nsecond line\nexit marker";
        let row = json!({
            "timestamp": "2026-07-31T12:00:01Z",
            "role": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "shell-1",
                "content": stdout,
                "is_error": false
            }]}
        });
        let expected = row.pointer("/message/content/0").unwrap().clone();
        let core = projected_core(&row);

        assert_eq!(core.content.structured_content, Some(expected.clone()));
        assert_eq!(
            serde_json::from_str::<Value>(core.content.normalized_body.as_deref().unwrap())
                .unwrap(),
            expected
        );
        let normalized: Value =
            serde_json::from_str(core.content.normalized_body.as_deref().unwrap()).unwrap();
        assert_eq!(
            normalized.get("content").and_then(Value::as_str),
            Some(stdout)
        );
    }

    #[test]
    fn cursor_provider_redaction_is_retained_without_invented_result_content() {
        let row = json!({
            "timestamp": "2026-07-31T12:00:01Z",
            "role": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "redacted-1",
                "content": null,
                "redacted": true
            }]}
        });
        let expected = row.pointer("/message/content/0").unwrap().clone();
        let core = projected_core(&row);

        assert_eq!(core.content.structured_content, Some(expected.clone()));
        assert_eq!(
            serde_json::from_str::<Value>(core.content.normalized_body.as_deref().unwrap())
                .unwrap(),
            expected
        );
        assert!(!core
            .content
            .normalized_body
            .as_deref()
            .unwrap()
            .contains("Cursor tool result"));
    }

    #[test]
    fn cursor_logical_event_ids_survive_insert_before_and_collide_fail_closed_for_duplicates() {
        let first = message("user", "2026-07-31T12:00:00Z", "first");
        let second = message("assistant", "2026-07-31T12:00:01Z", "second");
        let inserted = message("user", "2026-07-31T11:59:59Z", "inserted");

        let original = event_ids(&[first.clone(), second.clone()]);
        let with_insert = event_ids(&[inserted, first.clone(), second]);
        assert_eq!(original, with_insert[1..]);

        let duplicates = event_ids(&[first.clone(), first]);
        assert_eq!(duplicates[0], duplicates[1]);
    }

    #[test]
    fn cursor_append_projects_only_suffix_and_exact_duplicate_fails_closed() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("cursor-data");
        let transcript = transcript_path(&root, "project", "native-session");
        let first = message("user", "2026-07-31T12:00:00Z", "first");
        let second = message("assistant", "2026-07-31T12:00:01Z", "second");
        write_transcript(&transcript, &[first]);
        let registry = registry(&root);
        let index = temp.path().join("index");

        reset_cursor_projected_records();
        reset_cursor_signature_records();
        let cold = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert_eq!(cold.commit.indexed_documents, 1);
        assert_eq!(cursor_projected_records(), 1);
        assert_eq!(
            cursor_signature_records(),
            0,
            "a singleton native session must not be pre-parsed for route comparison"
        );

        append_transcript(&transcript, &second);
        reset_cursor_projected_records();
        reset_cursor_signature_records();
        let appended =
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert_eq!(appended.commit.indexed_documents, 2);
        assert_eq!(
            cursor_projected_records(),
            1,
            "Cursor append work must remain bounded to the validated suffix"
        );
        assert_eq!(
            cursor_signature_records(),
            0,
            "singleton append discovery must not rescan transcript content"
        );

        append_transcript(&transcript, &second);
        assert!(
            refresh_source_backed_generation(&index, &registry, writer_options()).is_err(),
            "an indistinguishable logical duplicate must fail closed instead of using position as identity"
        );
    }

    #[test]
    fn cursor_equivalent_duplicate_routes_cover_move_overlap_deterministically() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("cursor-data");
        let session = "native-session";
        let first = transcript_path(&root, "project-a", session);
        let second = transcript_path(&root, "project-b", session);
        let rows = [message("user", "2026-07-31T12:00:00Z", "same")];
        write_transcript(&first, &rows);

        reset_cursor_signature_records();
        let initial = CursorJsonlAdapter.discover(&root).unwrap();
        assert_eq!(initial.leaves().len(), 1);
        let initial_source = initial.leaves()[0].source().clone();
        assert_eq!(initial.leaves()[0].source_path(), first);
        assert_eq!(cursor_signature_records(), 0);

        write_transcript(&second, &rows);
        reset_cursor_signature_records();
        let overlap = CursorJsonlAdapter.discover(&root).unwrap();
        assert_eq!(overlap.leaves().len(), 1);
        assert_eq!(overlap.leaves()[0].source_path(), first);
        let overlap_binding = decode_binding(&overlap.leaves()[0]).unwrap();
        assert_eq!(overlap_binding.alias_route_sha256.len(), 1);
        assert_eq!(cursor_signature_records(), 2);

        fs::remove_file(&first).unwrap();
        reset_cursor_signature_records();
        let moved = CursorJsonlAdapter.discover(&root).unwrap();
        assert_eq!(moved.leaves().len(), 1);
        assert_eq!(moved.leaves()[0].source_path(), second);
        assert_eq!(cursor_signature_records(), 0);
        assert!(moved.leaves()[0]
            .source()
            .exact_descriptor_eq(&initial_source));
        assert!(decode_binding(&moved.leaves()[0])
            .unwrap()
            .alias_route_sha256
            .is_empty());
    }

    #[test]
    fn cursor_conflicting_duplicate_transcripts_are_rejected() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("cursor-data");
        let session = "native-session";
        write_transcript(
            &transcript_path(&root, "project-a", session),
            &[message("user", "2026-07-31T12:00:00Z", "first")],
        );
        write_transcript(
            &transcript_path(&root, "project-b", session),
            &[message("user", "2026-07-31T12:00:00Z", "conflict")],
        );

        let error = CursorJsonlAdapter.discover(&root).unwrap_err();
        assert!(error.to_string().contains("conflicting transcript copies"));
    }
}
