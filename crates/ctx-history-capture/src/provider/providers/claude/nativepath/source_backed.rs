//! Claude Code adapter for the shared borrowed JSONL replacement family.

use std::{
    collections::{BTreeSet, HashMap},
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, RepositoryAbstentionReason, SessionIdentityInput,
    SourceAnchor, SourceKey, StableEntityId, TypedKey,
};
use ctx_history_index::BaseEventIdentityLookup;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    record::parse_native_record,
    rows::{
        ClaudeEventKind, ClaudeOutputOutcome, ClaudePhysicalLocator, ClaudeRetainedRow,
        ClaudeSessionMetadata, CLAUDE_MAX_RECORD_ROWS,
    },
    source::{classify_claude_path, claude_projects_root, ClaudeSessionKey, SessionLayout},
};
use crate::repository_attribution::{
    apply_annotation, linked_outcome_evidence, AttributionInput, LinkedOutcomeInput,
    RepositoryAttributor, UnscopedFileObservation,
};
use crate::OutputOutcome;
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        providers::native_jsonl::visit_native_jsonl_files,
        source_backed::family::jsonl::{
            observe_opened_file, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory,
            JsonlFamilyLeaf, JsonlFamilyProjector, JsonlFileObservation, JsonlRecordRef,
        },
    },
    CaptureError, Result, CLAUDE_PROJECTS_SOURCE_FORMAT,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "claude.session-leaf";
const SESSION_KEY_NAMESPACE: &str = "claude.session";
const NATIVE_EVENT_KEY_NAMESPACE: &str = "claude.event";
const FALLBACK_EVENT_ID_VERSION: &str = "claude.fallback-event.v1";
const FALLBACK_EVENT_ID_DOMAIN: &[u8] = b"ctx-claude-fallback-event-id-v1\0";
const LOGICAL_SESSION_KIND: &str = "claude-session";
const LOGICAL_EVENT_KIND: &str = "claude-event";
const SOURCE_SCHEMA_VARIANT: &str = "claude-nativepath-jsonl-v6";
const PARSER_REVISION: &str = "claude-shared-jsonl-v3";
const MAX_PENDING_CALLS: usize = 4096;

mod checkpoint;

use checkpoint::*;

#[derive(Debug, Clone, Copy, Default)]
struct ClaudeJsonlAdapter;

fn claude_source_backed_adapter() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(ClaudeJsonlAdapter)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    project_dir: PathBuf,
    key: ClaudeSessionKey,
    layout: SessionLayout,
}

impl JsonlFamilyAdapter for ClaudeJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Claude
    }

    fn source_format(&self) -> &'static str {
        CLAUDE_PROJECTS_SOURCE_FORMAT
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
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: root.to_path_buf(),
                    reason: "Claude source-backed discovery requires a projects directory",
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        }
        let canonical_root = fs::canonicalize(root)?;
        let projects_root = claude_projects_root(&canonical_root);
        let authority = Arc::new(ProviderSourceRoot::open(&canonical_root)?);
        let mut paths = BTreeSet::new();
        visit_native_jsonl_files(&canonical_root, self.provider(), &mut |path| {
            paths.insert(fs::canonicalize(path)?);
            Ok(())
        })?;

        let mut selected = HashMap::<[u8; 32], JsonlFileObservation>::new();
        let mut leaves = Vec::new();
        for path in paths {
            let Some((project_dir, layout, key)) = classify_claude_path(&projects_root, &path)?
            else {
                continue;
            };
            let binding = Binding {
                project_dir,
                key,
                layout,
            };
            let source = source_key(&binding.key)?;
            let relative_path = relative_to_authority(&authority, &path)?;
            let opened = authority.open_file(&relative_path)?;
            let observation = observe_opened_file(&path, &opened)?;
            let digest = source.exact_descriptor_digest();
            if let Some(previous) = selected.get(&digest) {
                if previous == &observation {
                    continue;
                }
                return Err(CaptureError::InvalidPayload(
                    "Claude inventory repeats a native session identity".to_owned(),
                ));
            }
            selected.insert(digest, observation);
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                path,
                Arc::clone(&authority),
                relative_path,
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
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<BaseEventIdentityLookup>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        let binding = decode_binding(leaf)?;
        let identities = identities(&binding)?;
        let restored = checkpoint
            .map(|checkpoint| decode_projector_checkpoint(checkpoint, &binding))
            .transpose()?;
        let (session, pending_calls, linkage_capacity_exceeded) = restored.map_or_else(
            || {
                (
                    ClaudeSessionMetadata::new(binding.key.clone()),
                    HashMap::new(),
                    false,
                )
            },
            |restored| {
                (
                    restored.session,
                    restored.pending_calls,
                    restored.linkage_capacity_exceeded,
                )
            },
        );
        Ok(Box::new(ClaudeProjector {
            source: leaf.source().clone(),
            source_path: leaf.source_path().to_string_lossy().into_owned(),
            session,
            binding,
            identities,
            attributor: RepositoryAttributor::default(),
            pending_calls,
            linkage_capacity_exceeded,
            fallback_identities: FallbackEventIdentityState::new(base_event_lookup),
        }))
    }
}

struct Identities {
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    agent_type: &'static str,
    is_primary: bool,
}

struct ClaudeProjector {
    source: SourceKey,
    source_path: String,
    binding: Binding,
    identities: Identities,
    session: ClaudeSessionMetadata,
    attributor: RepositoryAttributor,
    pending_calls: HashMap<String, PendingCallState>,
    linkage_capacity_exceeded: bool,
    fallback_identities: FallbackEventIdentityState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingCall {
    command: Option<String>,
    declared_workdir: Option<String>,
    event_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PendingCallState {
    Exact(PendingCall),
    Ambiguous,
}

#[derive(Debug)]
enum PendingCallLookup {
    Exact(PendingCall),
    Ambiguous,
    Missing,
}

#[derive(Debug, Clone, Copy)]
struct FallbackEventIdentity {
    digest: [u8; 32],
    duplicate_occurrence: u64,
}

#[derive(Default)]
struct FallbackEventIdentityState {
    base_lookup: Option<BaseEventIdentityLookup>,
    next_occurrences: HashMap<[u8; 32], u64>,
}

impl FallbackEventIdentityState {
    fn new(base_lookup: Option<BaseEventIdentityLookup>) -> Self {
        Self {
            base_lookup,
            next_occurrences: HashMap::new(),
        }
    }
}

impl ClaudeProjector {
    fn remember_pending_call(&mut self, call_id: &str, state: PendingCallState) {
        if let Some(existing) = self.pending_calls.get_mut(call_id) {
            *existing = PendingCallState::Ambiguous;
            return;
        }
        if self.pending_calls.len() >= MAX_PENDING_CALLS {
            self.linkage_capacity_exceeded = true;
            return;
        }
        self.pending_calls.insert(call_id.to_owned(), state);
        if !projector_checkpoint_fits(self) {
            self.pending_calls.remove(call_id);
            self.linkage_capacity_exceeded = true;
        }
    }
}

impl JsonlFamilyProjector for ClaudeProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let evidence = record.evidence();
        let ordinal = evidence.physical_ordinal();
        let line_number = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Claude line number overflowed",
        ))?;
        let locator = ClaudePhysicalLocator {
            path: PathBuf::from(&self.source_path),
            byte_start: evidence.byte_start(),
            byte_end_exclusive: evidence.byte_end_exclusive(),
            line_number,
            record_sha256: Sha256::digest(record.bytes()).into(),
        };
        let Ok(parsed) = parse_native_record(record.bytes(), ordinal, &locator) else {
            return Ok(());
        };
        if parsed
            .session_id
            .as_deref()
            .filter(|session| !session.trim().is_empty())
            .is_some_and(|session| session != self.binding.key.root_session_id)
            || parsed.rows.len() > CLAUDE_MAX_RECORD_ROWS
        {
            return Ok(());
        }
        self.session.observe(
            parsed.timestamp.as_deref(),
            parsed.cwd.as_deref(),
            parsed.version.as_deref(),
            parsed.git_branch.as_deref(),
        );
        for row in parsed.rows {
            let event_sequence = row_event_sequence(&row)?;
            let structured_content = row_structured_content(&row);
            let mut input = AttributionInput {
                activity_at_unix_ms: row
                    .occurred_at
                    .as_deref()
                    .and_then(|value| value.parse::<DateTime<Utc>>().ok())
                    .map(|value| value.timestamp_millis()),
                session_cwd: parsed.cwd.clone().or_else(|| self.session.cwd.clone()),
                structured_content,
                ..AttributionInput::default()
            };
            if let Some(call) = &row.tool_call {
                input.command = call.command.clone();
                input.declared_tool_workdir = call.declared_workdir.clone();
                input.file_observations = call
                    .file_touches
                    .iter()
                    .map(|touch| UnscopedFileObservation {
                        path: touch.path.clone(),
                        prior_path: touch.previous_path.clone(),
                        kind: touch.kind,
                    })
                    .collect();
                if let Some(call_id) = call.call_id.as_deref().filter(|id| !id.is_empty()) {
                    self.remember_pending_call(
                        call_id,
                        PendingCallState::Exact(PendingCall {
                            command: call.command.clone(),
                            declared_workdir: call.declared_workdir.clone(),
                            event_sequence,
                        }),
                    );
                }
            }
            if let Some(result) = &row.tool_result {
                let (context, _linkage_abstained) = resolve_pending_call(
                    &mut self.pending_calls,
                    result.call_id.as_deref(),
                    self.linkage_capacity_exceeded,
                    &mut input,
                );
                if let (Some(context), Some(result_call_id)) = (context, result.call_id.as_deref())
                {
                    input.command = context.command.clone();
                    input.declared_tool_workdir = context.declared_workdir.clone();
                    if let Some(command) = context.command.as_deref() {
                        let output = result.tool_use_result.as_ref().unwrap_or(&result.content);
                        let structured_oid = result
                            .tool_use_result
                            .as_ref()
                            .and_then(|value| value.pointer("/gitOperation/commit/sha"))
                            .and_then(serde_json::Value::as_str);
                        let output_workdir = result
                            .tool_use_result
                            .as_ref()
                            .and_then(|value| value.get("cwd").or_else(|| value.get("workdir")))
                            .and_then(serde_json::Value::as_str);
                        if let Some(linked) = linked_outcome_evidence(LinkedOutcomeInput {
                            provider: "claude",
                            command,
                            session_cwd: input.session_cwd.as_deref(),
                            declared_workdir: context.declared_workdir.as_deref(),
                            origin_call_id: result_call_id,
                            result_call_id,
                            origin_event_sequence: context.event_sequence,
                            continuation_call_id_sha256: &[],
                            result_record_sha256: row.locator.record_sha256,
                            observed_at_unix_ms: input.activity_at_unix_ms.unwrap_or(0),
                            result_outcome: claude_output_outcome(result.outcome),
                            result_output: output,
                            structured_commit_oid: structured_oid,
                            output_repository_path: output_workdir,
                        }) {
                            input.provider_native_repository_aliases =
                                linked.provider_native_repository_aliases;
                            input.outcome_operation_repository_path =
                                linked.outcome_operation_repository_path;
                            input.outcome_output_repository_path =
                                linked.outcome_output_repository_path;
                            input.outcome_observations = linked.outcomes;
                            input.outcome_abstentions = linked.abstentions;
                        }
                    }
                }
            }
            let fallback_identity = next_fallback_event_identity(
                &row,
                &self.source,
                self.identities.session_id,
                &mut self.fallback_identities,
            )?;
            let mut core = core_record(
                &self.source,
                &self.source_path,
                &self.binding,
                &self.identities,
                &self.session,
                row,
                fallback_identity,
            )?;
            apply_annotation(&mut core, self.attributor.attribute(input));
            core.validate_contract().map_err(contract)?;
            emit(core)?;
        }
        Ok(())
    }

    fn provider_checkpoint(&self) -> Result<Option<TypedKey>> {
        encode_projector_checkpoint(self).map(Some)
    }
}

fn core_record(
    source: &SourceKey,
    _source_path: &str,
    binding: &Binding,
    identities: &Identities,
    session: &ClaudeSessionMetadata,
    row: ClaudeRetainedRow,
    fallback_identity: Option<FallbackEventIdentity>,
) -> Result<CoreRecord> {
    let native_item_key = native_item_key(&row, fallback_identity)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: identities.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(contract)?;
    let native_event_id = native_event_typed_key(&row, fallback_identity)?;
    let event_sequence = row_event_sequence(&row)?;
    let structured_content = row_structured_content(&row);
    let mut record = CoreRecord::new_selected(
        event_id,
        identities.session_id,
        identities.root_session_id,
        source.clone(),
        event_sequence,
        event_kind(row.kind),
        identities.agent_type,
        identities.is_primary,
        PARSER_REVISION,
        lexical_body(&row),
    )
    .map_err(contract)?;
    record.parent_session_id = identities.parent_session_id;
    record.provider_session_id = Some(binding.key.provider_session_id());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = row
        .occurred_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .map(|value| value.timestamp_millis());
    record.role = row.role;
    record.workspace = binding.project_dir.to_str().map(str::to_owned);
    record.branch = session.git_branch.clone();
    record.cwd = session.cwd.clone();
    record.content.structured_content = structured_content;
    record.validate_contract().map_err(contract)?;
    Ok(record)
}

fn row_event_sequence(row: &ClaudeRetainedRow) -> Result<u64> {
    row.identity
        .source_record_ordinal
        .checked_mul(1_u64 << 16)
        .and_then(|value| value.checked_add(row.identity.source_subrecord_index))
        .ok_or(CaptureError::SystemInvariant(
            "Claude event sequence overflowed",
        ))
}

fn row_structured_content(row: &ClaudeRetainedRow) -> Option<serde_json::Value> {
    row.tool_call
        .as_ref()
        .map(|call| {
            serde_json::json!({
                "type": "tool_use",
                "id": call.call_id,
                "name": call.tool_name,
                "input": call.input,
            })
        })
        .or_else(|| {
            row.tool_result.as_ref().map(|result| {
                serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": result.call_id,
                    "content": result.content,
                    "toolUseResult": result.tool_use_result,
                    "outcome": result.outcome,
                    "exit_code": result.exit_code,
                    "duration_ms": result.duration_ms,
                })
            })
        })
}

fn claude_output_outcome(outcome: ClaudeOutputOutcome) -> OutputOutcome {
    match outcome {
        ClaudeOutputOutcome::Success => OutputOutcome::Success,
        ClaudeOutputOutcome::Failure => OutputOutcome::Failure,
        ClaudeOutputOutcome::Timeout => OutputOutcome::Timeout,
        ClaudeOutputOutcome::Unknown => OutputOutcome::Unknown,
    }
}

fn identities(binding: &Binding) -> Result<Identities> {
    let native_session_key = session_typed_key(&binding.key)?;
    let source = source_key(&binding.key)?;
    let session_id = session_identity(&source, &native_session_key)?;
    let root_key = ClaudeSessionKey {
        root_session_id: binding.key.root_session_id.clone(),
        workflow_run_id: None,
        agent_id: None,
    };
    let root_source = source_key(&root_key)?;
    let root_session_id = if binding.layout == SessionLayout::Primary {
        session_id
    } else {
        session_identity(&root_source, &session_typed_key(&root_key)?)?
    };
    let parent_session_id = binding.key.agent_id.as_ref().map(|_| root_session_id);
    let (agent_type, is_primary) = match binding.layout {
        SessionLayout::Primary => ("primary", true),
        SessionLayout::Subagent => ("subagent", false),
        SessionLayout::WorkflowSubagent => ("workflow_subagent", false),
    };
    Ok(Identities {
        session_id,
        parent_session_id,
        root_session_id,
        agent_type,
        is_primary,
    })
}

fn session_typed_key(key: &ClaudeSessionKey) -> Result<TypedKey> {
    TypedKey::composite(vec![
        TypedKey::utf8(&key.root_session_id).map_err(contract)?,
        key.workflow_run_id
            .as_deref()
            .map(TypedKey::utf8)
            .transpose()
            .map_err(contract)?
            .unwrap_or(TypedKey::Null),
        key.agent_id
            .as_deref()
            .map(TypedKey::utf8)
            .transpose()
            .map_err(contract)?
            .unwrap_or(TypedKey::Null),
    ])
    .map_err(contract)
}

fn source_key(key: &ClaudeSessionKey) -> Result<SourceKey> {
    let anchor = SourceAnchor::provider_native(SOURCE_ANCHOR_NAMESPACE, session_typed_key(key)?)
        .map_err(contract)?;
    SourceKey::derive(
        CaptureProvider::Claude.as_str(),
        CLAUDE_PROJECTS_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )
    .map_err(contract)
}

fn session_identity(source: &SourceKey, native_key: &TypedKey) -> Result<StableEntityId> {
    let key =
        NativeSessionKey::native_id(SESSION_KEY_NAMESPACE, native_key.clone()).map_err(contract)?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &key,
    })
    .map_err(contract)
}

fn native_item_key(
    row: &ClaudeRetainedRow,
    fallback_identity: Option<FallbackEventIdentity>,
) -> Result<NativeItemKey> {
    if let Some(native_record_id) = row.native_record_id.as_deref() {
        return NativeItemKey::composite(
            NATIVE_EVENT_KEY_NAMESPACE,
            vec![
                TypedKey::utf8(native_record_id).map_err(contract)?,
                TypedKey::U64(row.identity.source_subrecord_index),
            ],
        )
        .map_err(contract);
    }
    let fallback_identity = fallback_identity.ok_or(CaptureError::SystemInvariant(
        "Claude fallback event identity was not assigned",
    ))?;
    NativeItemKey::composite(
        NATIVE_EVENT_KEY_NAMESPACE,
        fallback_event_key_parts(fallback_identity)?,
    )
    .map_err(contract)
}

fn native_event_typed_key(
    row: &ClaudeRetainedRow,
    fallback_identity: Option<FallbackEventIdentity>,
) -> Result<TypedKey> {
    if let Some(native_record_id) = row.native_record_id.as_deref() {
        return TypedKey::composite(vec![
            TypedKey::utf8(native_record_id).map_err(contract)?,
            TypedKey::U64(row.identity.source_subrecord_index),
        ])
        .map_err(contract);
    }
    let fallback_identity = fallback_identity.ok_or(CaptureError::SystemInvariant(
        "Claude fallback native event key was not assigned",
    ))?;
    TypedKey::composite(fallback_event_key_parts(fallback_identity)?).map_err(contract)
}

#[cfg(test)]
fn remember_pending_call(
    pending_calls: &mut HashMap<String, PendingCallState>,
    linkage_capacity_exceeded: &mut bool,
    call_id: &str,
    state: PendingCallState,
) {
    if let Some(existing) = pending_calls.get_mut(call_id) {
        *existing = PendingCallState::Ambiguous;
    } else if pending_calls.len() < MAX_PENDING_CALLS {
        pending_calls.insert(call_id.to_owned(), state);
    } else {
        *linkage_capacity_exceeded = true;
    }
}

fn take_pending_call(
    pending_calls: &mut HashMap<String, PendingCallState>,
    call_id: Option<&str>,
) -> PendingCallLookup {
    match call_id.and_then(|call_id| pending_calls.remove(call_id)) {
        Some(PendingCallState::Exact(context)) => PendingCallLookup::Exact(context),
        Some(PendingCallState::Ambiguous) => PendingCallLookup::Ambiguous,
        None => PendingCallLookup::Missing,
    }
}

fn resolve_pending_call(
    pending_calls: &mut HashMap<String, PendingCallState>,
    call_id: Option<&str>,
    linkage_capacity_exceeded: bool,
    input: &mut AttributionInput,
) -> (Option<PendingCall>, bool) {
    match take_pending_call(pending_calls, call_id) {
        PendingCallLookup::Exact(context) => (Some(context), false),
        PendingCallLookup::Ambiguous => {
            input.outcome_abstentions.push((
                RepositoryAbstentionReason::ProviderOutputUnjoined,
                "claude_tool_result_call_id_is_ambiguous",
            ));
            (None, true)
        }
        PendingCallLookup::Missing => {
            let (reason, detail) = if linkage_capacity_exceeded {
                (
                    RepositoryAbstentionReason::LinkageCapacityExceeded,
                    "claude_tool_result_linkage_capacity_exceeded",
                )
            } else {
                (
                    RepositoryAbstentionReason::ProviderOutputUnjoined,
                    "claude_tool_result_has_no_exact_unique_call_link",
                )
            };
            input.outcome_abstentions.push((reason, detail));
            (None, true)
        }
    }
}

fn next_fallback_event_identity(
    row: &ClaudeRetainedRow,
    source: &SourceKey,
    session_id: StableEntityId,
    state: &mut FallbackEventIdentityState,
) -> Result<Option<FallbackEventIdentity>> {
    if row.native_record_id.is_some() {
        return Ok(None);
    }
    let digest = fallback_event_digest(row)?;
    let occurrence = match state.next_occurrences.get(&digest).copied() {
        Some(occurrence) => occurrence,
        None => {
            first_unused_base_occurrence(state.base_lookup.as_ref(), source, session_id, digest)?
        }
    };
    let identity = FallbackEventIdentity {
        digest,
        duplicate_occurrence: occurrence,
    };
    state.next_occurrences.insert(
        digest,
        occurrence
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Claude fallback duplicate occurrence overflowed",
            ))?,
    );
    Ok(Some(identity))
}

fn first_unused_base_occurrence(
    base_lookup: Option<&BaseEventIdentityLookup>,
    source: &SourceKey,
    session_id: StableEntityId,
    digest: [u8; 32],
) -> Result<u64> {
    let Some(base_lookup) = base_lookup else {
        return Ok(0);
    };
    if !base_occurrence_exists(base_lookup, source, session_id, digest, 0)? {
        return Ok(0);
    }

    let mut present = 0_u64;
    let mut missing = 1_u64;
    while base_occurrence_exists(base_lookup, source, session_id, digest, missing)? {
        present = missing;
        missing = match missing.checked_mul(2) {
            Some(next) => next,
            None if missing != u64::MAX => u64::MAX,
            None => {
                return Err(CaptureError::SystemInvariant(
                    "Claude fallback duplicate occurrence overflowed",
                ));
            }
        };
    }
    while present.saturating_add(1) < missing {
        let candidate = present + (missing - present) / 2;
        if base_occurrence_exists(base_lookup, source, session_id, digest, candidate)? {
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
    digest: [u8; 32],
    occurrence: u64,
) -> Result<bool> {
    let identity = FallbackEventIdentity {
        digest,
        duplicate_occurrence: occurrence,
    };
    let native_item_key = NativeItemKey::composite(
        NATIVE_EVENT_KEY_NAMESPACE,
        fallback_event_key_parts(identity)?,
    )
    .map_err(contract)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(contract)?;
    base_lookup
        .contains(event_id.as_uuid())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn fallback_event_digest(row: &ClaudeRetainedRow) -> Result<[u8; 32]> {
    let logical = serde_json::to_vec(&(
        row.parent_native_record_id.as_deref(),
        row.kind,
        row.role.as_deref(),
        row.occurred_at.as_deref(),
        row.body.as_deref(),
        row.body_sha256,
        row.body_text_retention.as_ref(),
        row.tool_call.as_ref(),
        row.tool_result.as_ref(),
    ))?;
    let mut hasher = Sha256::new();
    hasher.update(FALLBACK_EVENT_ID_DOMAIN);
    hasher.update((logical.len() as u64).to_be_bytes());
    hasher.update(logical);
    Ok(hasher.finalize().into())
}

fn fallback_event_key_parts(identity: FallbackEventIdentity) -> Result<Vec<TypedKey>> {
    Ok(vec![
        TypedKey::utf8(FALLBACK_EVENT_ID_VERSION).map_err(contract)?,
        TypedKey::bytes(identity.digest.to_vec()).map_err(contract)?,
        TypedKey::U64(identity.duplicate_occurrence),
    ])
}

fn lexical_body(row: &ClaudeRetainedRow) -> String {
    let text = row
        .body
        .clone()
        .or_else(|| {
            row.tool_call.as_ref().and_then(|call| {
                serde_json::to_string(&serde_json::json!({
                    "type": "tool_use",
                    "id": call.call_id,
                    "name": call.tool_name,
                    "input": call.input,
                }))
                .ok()
            })
        })
        .or_else(|| {
            row.tool_result.as_ref().and_then(|output| {
                serde_json::to_string(&serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": output.call_id,
                    "content": output.content,
                    "toolUseResult": output.tool_use_result,
                    "outcome": output.outcome,
                    "exit_code": output.exit_code,
                    "duration_ms": output.duration_ms,
                }))
                .ok()
            })
        })
        .unwrap_or_else(|| event_kind(row.kind).to_owned());
    if text.trim().is_empty() {
        event_kind(row.kind).to_owned()
    } else {
        text
    }
}

fn event_kind(kind: ClaudeEventKind) -> &'static str {
    match kind {
        ClaudeEventKind::Message => "message",
        ClaudeEventKind::Summary => "summary",
        ClaudeEventKind::Notice => "notice",
        ClaudeEventKind::ToolCall => "tool_call",
        ClaudeEventKind::ToolOutput => "tool_output",
    }
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> Result<Binding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(contract("Claude family binding is malformed"));
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn relative_to_authority(authority: &ProviderSourceRoot, path: &Path) -> Result<PathBuf> {
    path.strip_prefix(authority.named_path())
        .map(Path::to_path_buf)
        .map_err(|_| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Claude transcripts must remain below their selected authority",
        })
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(crate) mod registration;
#[cfg(test)]
mod tests;
