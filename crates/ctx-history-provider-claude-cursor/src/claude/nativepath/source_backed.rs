//! Claude Code adapter for the shared borrowed JSONL replacement family.

use std::{
    collections::{BTreeSet, HashMap},
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_capture_runtime::BaseEventLookup;
use ctx_history_core::{
    derive_event_id, derive_native_session_id, ActivityInvocation, ActivityJsonCapture,
    ActivityResult, ActivityTextCapture, AgentScope, CaptureProvider, CoreActivity, CoreRecord,
    CoreRecordAnnotation, EventIdentityInput, LiteralFactKind, NativeItemKey, ProviderDeclaredFact,
    ProviderNativeSessionRelationship, SourceKey, StableEntityId, TypedKey, CORE_ACTIVITY_REVISION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    record::parse_native_record,
    rows::{
        ClaudePhysicalLocator, ClaudeRetainedRow, ClaudeSessionMetadata, CLAUDE_MAX_RECORD_ROWS,
    },
    source::{classify_claude_path, claude_projects_root, ClaudeSessionKey, SessionLayout},
};
use crate::CLAUDE_PROJECTS_SOURCE_FORMAT;
use ctx_history_jsonl::{
    fit_jsonl_activity, selected_content_fits, JsonlActivityObservedBytes, JsonlFamilyAdapter,
    JsonlFamilyBaseScope,
};
use ctx_history_provider_runtime::{
    observe_opened_file,
    source_io::{OpenedProviderSourceFile, ProviderSourceRoot},
    CaptureError, JsonlAppendOccurrenceState, JsonlFamilyAppendMode, JsonlFamilyProjectionMode,
    JsonlFamilyProjector, JsonlFileObservation, JsonlRecordRef, ProviderBaseEventLookup,
    ProviderJsonlInventory, ProviderJsonlLeaf, ProviderJsonlReader, ProviderJsonlRuntime,
    ProviderJsonlWorkerContext, ProviderRuntimeBinding, Result,
};
use ctx_history_source_io::visit_bounded_tree_files;

type JsonlFamilyLeaf = ProviderJsonlLeaf;
type JsonlReader = ProviderJsonlReader;
type JsonlFamilyWorkerContext<B> = ProviderJsonlWorkerContext<B>;

const SOURCE_ANCHOR_NAMESPACE: &str = "claude.session-leaf";
const SESSION_KEY_NAMESPACE: &str = "claude.session";
const NATIVE_EVENT_KEY_NAMESPACE: &str = "claude.event";
const FALLBACK_EVENT_ID_VERSION: &str = "claude.fallback-event.v1";
const FALLBACK_EVENT_ID_DOMAIN: &[u8] = b"ctx-claude-fallback-event-id-v1\0";
const LOGICAL_SESSION_KIND: &str = "claude-session";
const LOGICAL_EVENT_KIND: &str = "claude-event";
const SOURCE_SCHEMA_VARIANT: &str = "claude-nativepath-jsonl-v6";
const PARSER_REVISION: &str = "claude-shared-jsonl-core-activity-v1";

mod binding;
mod normalized_body;

use binding::*;
use normalized_body::{event_kind, lexical_body};

#[derive(Debug, Default)]
struct ClaudeJsonlAdapter<B> {
    source_root_lineage: Option<[u8; 32]>,
    binding: std::marker::PhantomData<fn() -> B>,
}

pub(crate) fn claude_jsonl_adapter<B>(
) -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    // Automatic Claude routes retain the released unqualified source identity
    // so existing manifests remain refreshable across the 1.1 upgrade.
    claude_jsonl_adapter_with_source_root_lineage(None)
}

pub(crate) fn claude_jsonl_adapter_with_source_root_lineage<B>(
    source_root_lineage: Option<[u8; 32]>,
) -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    Arc::new(ClaudeJsonlAdapter {
        source_root_lineage,
        binding: std::marker::PhantomData,
    })
}

impl<B> JsonlFamilyAdapter for ClaudeJsonlAdapter<B>
where
    B: ProviderRuntimeBinding,
{
    type Runtime = ProviderJsonlRuntime<B>;

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
        JsonlFamilyAppendMode::ProjectorPreflight(true)
    }

    fn base_scope(&self) -> JsonlFamilyBaseScope {
        // Automatic and named routes can alternate ownership of the same
        // physical home. Reuse only the exact route's prior sources so either
        // transition cold-scans the new owner while topology retirement drops
        // the old route atomically.
        JsonlFamilyBaseScope::Route
    }

    fn discover(&self, root: &Path) -> Result<ProviderJsonlInventory> {
        match fs::symlink_metadata(root) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: root.to_path_buf(),
                    reason: "Claude source-backed discovery requires a projects directory",
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return ProviderJsonlInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        }
        let canonical_root = fs::canonicalize(root)?;
        let projects_root = claude_projects_root(&canonical_root);
        let source_root_lineage = self.source_root_lineage;
        let authority = Arc::new(ProviderSourceRoot::open(&canonical_root)?);
        let mut paths = BTreeSet::new();
        visit_bounded_tree_files::<CaptureError, _>(
            &canonical_root,
            &mut |candidate| {
                candidate
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    == Some("jsonl")
            },
            &mut |source_file| {
                paths.insert(fs::canonicalize(source_file.path())?);
                Ok(())
            },
        )?;

        let mut selected = HashMap::<[u8; 32], JsonlFileObservation>::new();
        let mut leaves = Vec::new();
        for path in paths {
            let Some((project_dir, layout, key)) = classify_claude_path(&projects_root, &path)?
            else {
                continue;
            };
            let binding = Binding {
                project_dir,
                source_root_lineage,
                key,
                layout,
            };
            let source = source_key(binding.source_root_lineage, &binding.key)?;
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
            leaves.push(ProviderJsonlLeaf::observe(
                source,
                path,
                Arc::clone(&authority),
                relative_path,
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?,
            )?);
        }
        ProviderJsonlInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &ProviderJsonlLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
        _checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<ProviderBaseEventLookup<B>>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = ProviderJsonlRuntime<B>>>> {
        let binding = decode_binding(leaf)?;
        if !source_key(binding.source_root_lineage, &binding.key)?
            .exact_descriptor_eq(leaf.source())
        {
            return Err(contract(
                "Claude family binding does not match its certified source",
            ));
        }
        let identities = identities(&binding)?;
        Ok(Box::new(ClaudeProjector {
            source: leaf.source().clone(),
            source_path: leaf.source_path().to_string_lossy().into_owned(),
            session: ClaudeSessionMetadata::new(binding.key.clone()),
            binding,
            identities,
            rejected_records: 0,
            fallback_identities: match (mode, base_event_lookup) {
                (JsonlFamilyProjectionMode::CertifiedAppend, Some(base_lookup)) => {
                    JsonlAppendOccurrenceState::for_append(base_lookup)
                }
                _ => JsonlAppendOccurrenceState::default(),
            },
        }))
    }
}

struct Identities {
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    agent_scope: AgentScope,
}

struct ClaudeProjector<B: ProviderRuntimeBinding> {
    source: SourceKey,
    source_path: String,
    binding: Binding,
    identities: Identities,
    session: ClaudeSessionMetadata,
    rejected_records: u64,
    fallback_identities: JsonlAppendOccurrenceState<[u8; 32], ProviderBaseEventLookup<B>>,
}

#[derive(Debug, Clone, Copy)]
struct FallbackEventIdentity {
    digest: [u8; 32],
    duplicate_occurrence: u64,
}

impl<B: ProviderRuntimeBinding> ClaudeProjector<B> {
    fn reject_record(&mut self) -> Result<()> {
        self.rejected_records = self.rejected_records.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload("Claude rejected-record count overflowed".to_owned())
        })?;
        Ok(())
    }
}

impl<B> JsonlFamilyProjector for ClaudeProjector<B>
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
        self.session = ClaudeSessionMetadata::new(self.binding.key.clone());
        self.rejected_records = 0;
        self.fallback_identities = JsonlAppendOccurrenceState::default();
    }

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<B>,
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
        let parsed = match parse_native_record(record.bytes(), ordinal, &locator) {
            Ok(parsed) => parsed,
            Err(_) => return self.reject_record(),
        };
        if parsed
            .session_id
            .as_deref()
            .filter(|session| !session.trim().is_empty())
            .is_some_and(|session| session != self.binding.key.root_session_id)
            || (parsed.rows.is_empty() && !parsed.ignored_private_thinking)
            || parsed.rows.len() > CLAUDE_MAX_RECORD_ROWS
        {
            return self.reject_record();
        }
        self.session.observe(
            parsed.timestamp.as_deref(),
            parsed.cwd.as_deref(),
            parsed.version.as_deref(),
            parsed.git_branch.as_deref(),
        );
        if parsed.rows.is_empty() {
            return Ok(());
        }
        for row in parsed.rows {
            let normalized_body = lexical_body(&row);
            let annotation =
                claude_annotation(&row, parsed.cwd.as_deref(), parsed.git_branch.as_deref())?;
            let fallback_identity = next_fallback_event_identity::<B>(
                &row,
                &self.source,
                self.identities.session_id,
                &mut self.fallback_identities,
            )?;
            let core = core_record(
                &self.source,
                &self.binding,
                &self.identities,
                row,
                fallback_identity,
                normalized_body,
                annotation,
            )?;
            emit(core)?;
        }
        Ok(())
    }

    fn provider_checkpoint(&self) -> Result<Option<TypedKey>> {
        Ok(None)
    }

    fn rejected_records(&self) -> u64 {
        self.rejected_records
    }
}

fn claude_annotation(
    row: &ClaudeRetainedRow,
    declared_cwd: Option<&str>,
    declared_branch: Option<&str>,
) -> Result<CoreRecordAnnotation> {
    let occurred_at_unix_ms = row
        .occurred_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .map(|value| value.timestamp_millis());
    let mut provider_call_id = None;
    let mut invocation = None;
    let mut result = None;
    let mut facts = Vec::new();
    if let Some(cwd) = declared_cwd.filter(|value| !value.is_empty()) {
        facts.push(provider_fact(LiteralFactKind::SessionCwd, cwd));
    }
    if let Some(branch) = declared_branch.filter(|value| !value.is_empty()) {
        facts.push(provider_fact(LiteralFactKind::Branch, branch));
    }
    let structured_content = if let Some(call) = &row.tool_call {
        extend_exact_facts(&mut facts, &call.literal_facts);
        if let (Some(call_id), Some(tool_name)) = (
            call.call_id.as_deref().filter(|value| !value.is_empty()),
            call.tool_name.as_deref().filter(|value| !value.is_empty()),
        ) {
            provider_call_id = Some(TypedKey::utf8(call_id).map_err(contract)?);
            let (protocol, server, tool) = exact_claude_tool_identity(
                tool_name,
                call.protocol.as_deref(),
                call.server.as_deref(),
                call.explicit_tool.as_deref(),
                call.mcp_identity_unavailable,
            );
            invocation = Some(ActivityInvocation {
                protocol,
                server,
                tool,
                arguments: json_capture(call.input.as_ref(), call.input_unavailable),
                started_at_unix_ms: occurred_at_unix_ms,
            });
        }
        (!call.native_content_unavailable).then(|| call.native_content.clone())
    } else if let Some(tool_result) = &row.tool_result {
        extend_exact_facts(&mut facts, &tool_result.literal_facts);
        if let Some(call_id) = tool_result
            .call_id
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            provider_call_id = Some(TypedKey::utf8(call_id).map_err(contract)?);
            result = Some(ActivityResult {
                status: None,
                completed_at_unix_ms: occurred_at_unix_ms,
                duration_ns: None,
                text: claude_result_text_capture(tool_result),
                structured_content: if tool_result.native_content_unavailable {
                    ActivityJsonCapture::Unavailable
                } else {
                    ActivityJsonCapture::Present {
                        value: tool_result.native_content.clone(),
                    }
                },
            });
        }
        (!tool_result.native_content_unavailable).then(|| tool_result.native_content.clone())
    } else {
        None
    };
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

fn claude_result_text_capture(result: &super::rows::ClaudeToolResult) -> ActivityTextCapture {
    if result.content_unavailable {
        return ActivityTextCapture::Unavailable;
    }
    match result.native_content.get("content") {
        Some(serde_json::Value::String(value)) => ActivityTextCapture::Present {
            value: value.clone(),
        },
        Some(_) | None => ActivityTextCapture::Absent,
    }
}

fn exact_claude_tool_identity(
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

fn provider_fact(kind: LiteralFactKind, value: &str) -> ProviderDeclaredFact {
    ProviderDeclaredFact {
        kind,
        value: value.to_owned(),
    }
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

fn extend_exact_facts(facts: &mut Vec<ProviderDeclaredFact>, native: &[ProviderDeclaredFact]) {
    if facts
        .len()
        .checked_add(native.len())
        .is_some_and(|count| count <= ctx_history_core::MAX_PROVIDER_DECLARED_FACTS)
    {
        facts.extend(native.iter().cloned());
    }
}

fn core_record(
    source: &SourceKey,
    binding: &Binding,
    identities: &Identities,
    row: ClaudeRetainedRow,
    fallback_identity: Option<FallbackEventIdentity>,
    normalized_body: String,
    annotation: CoreRecordAnnotation,
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
    let mut record = CoreRecord::new_selected(
        event_id,
        identities.session_id,
        source.clone(),
        event_sequence,
        event_kind(row.kind),
        PARSER_REVISION,
        normalized_body,
    )
    .map_err(contract)?;
    if let Some(parent_session_id) = identities.parent_session_id {
        record.parent_session_id = Some(parent_session_id);
        record.root_session_id = Some(identities.root_session_id);
        record.session_relationship = Some(match binding.layout {
            SessionLayout::WorkflowSubagent => ProviderNativeSessionRelationship::WorkflowChild,
            SessionLayout::Subagent => ProviderNativeSessionRelationship::Delegated,
            SessionLayout::Primary => ProviderNativeSessionRelationship::Root,
        });
    }
    record.provider_session_id = Some(binding.key.provider_session_id());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = row
        .occurred_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .map(|value| value.timestamp_millis());
    record.role = row.role;
    record.agent_scope = Some(identities.agent_scope);
    let mut structured_content = annotation.structured_content;
    let mut activity = annotation.activity;
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

fn identities(binding: &Binding) -> Result<Identities> {
    let native_session_key = session_typed_key(&binding.key)?;
    let source = source_key(binding.source_root_lineage, &binding.key)?;
    let session_id = session_identity(&source, native_session_key)?;
    let root_key = ClaudeSessionKey {
        root_session_id: binding.key.root_session_id.clone(),
        workflow_run_id: None,
        agent_id: None,
    };
    let root_source = source_key(binding.source_root_lineage, &root_key)?;
    let root_session_id = if binding.layout == SessionLayout::Primary {
        session_id
    } else {
        session_identity(&root_source, session_typed_key(&root_key)?)?
    };
    let parent_session_id = binding.key.agent_id.as_ref().map(|_| root_session_id);
    let agent_scope = match binding.layout {
        SessionLayout::Primary => AgentScope::Primary,
        SessionLayout::Subagent | SessionLayout::WorkflowSubagent => AgentScope::Subagent,
    };
    Ok(Identities {
        session_id,
        parent_session_id,
        root_session_id,
        agent_scope,
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

fn source_key(source_root_lineage: Option<[u8; 32]>, key: &ClaudeSessionKey) -> Result<SourceKey> {
    let native_key = match source_root_lineage {
        Some(source_root_lineage) => TypedKey::composite(vec![
            TypedKey::bytes(source_root_lineage.to_vec()).map_err(contract)?,
            session_typed_key(key)?,
        ])
        .map_err(contract)?,
        None => session_typed_key(key)?,
    };
    SourceKey::derive_provider_native(
        CaptureProvider::Claude.as_str(),
        CLAUDE_PROJECTS_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SOURCE_ANCHOR_NAMESPACE,
        native_key,
    )
    .map_err(contract)
}

fn session_identity(source: &SourceKey, native_key: TypedKey) -> Result<StableEntityId> {
    derive_native_session_id(
        source,
        LOGICAL_SESSION_KIND,
        SESSION_KEY_NAMESPACE,
        native_key,
    )
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

fn next_fallback_event_identity<B: ProviderRuntimeBinding>(
    row: &ClaudeRetainedRow,
    source: &SourceKey,
    session_id: StableEntityId,
    state: &mut JsonlAppendOccurrenceState<[u8; 32], ProviderBaseEventLookup<B>>,
) -> Result<Option<FallbackEventIdentity>> {
    if row.native_record_id.is_some() {
        return Ok(None);
    }
    let digest = fallback_event_digest(row)?;
    let occurrence = state.next(
        digest,
        || CaptureError::SystemInvariant("Claude fallback duplicate occurrence overflowed"),
        |base_lookup, occurrence| {
            base_occurrence_exists::<B>(base_lookup, source, session_id, digest, occurrence)
        },
    )?;
    let identity = FallbackEventIdentity {
        digest,
        duplicate_occurrence: occurrence,
    };
    Ok(Some(identity))
}

fn base_occurrence_exists<B: ProviderRuntimeBinding>(
    base_lookup: &ProviderBaseEventLookup<B>,
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

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_core::{ActivityJsonCapture, ActivityTextCapture};

    fn locator(bytes: &[u8]) -> ClaudePhysicalLocator {
        ClaudePhysicalLocator {
            path: PathBuf::from("fixture.jsonl"),
            byte_start: 0,
            byte_end_exclusive: bytes.len() as u64,
            line_number: 1,
            record_sha256: Sha256::digest(bytes).into(),
        }
    }

    #[test]
    fn identical_native_sessions_under_distinct_logical_roots_have_distinct_sources() {
        let key = ClaudeSessionKey {
            root_session_id: "shared-session".to_owned(),
            workflow_run_id: None,
            agent_id: None,
        };
        let personal = [1; 32];
        let work = [2; 32];

        let personal_source = source_key(Some(personal), &key).unwrap();
        let work_source = source_key(Some(work), &key).unwrap();

        assert!(!personal_source.exact_descriptor_eq(&work_source));
        assert!(personal_source.exact_descriptor_eq(&source_key(Some(personal), &key).unwrap()));
    }

    #[test]
    fn automatic_source_identity_keeps_the_released_unqualified_lineage() {
        let key = ClaudeSessionKey {
            root_session_id: "released-session".to_owned(),
            workflow_run_id: None,
            agent_id: None,
        };
        let released = SourceKey::derive_provider_native(
            CaptureProvider::Claude.as_str(),
            CLAUDE_PROJECTS_SOURCE_FORMAT,
            SOURCE_SCHEMA_VARIANT,
            1,
            SOURCE_ANCHOR_NAMESPACE,
            session_typed_key(&key).unwrap(),
        )
        .unwrap();

        assert!(released.exact_descriptor_eq(&source_key(None, &key).unwrap()));
    }

    #[test]
    fn malformed_record_is_rejected_before_any_core_activity_exists() {
        let bytes = b"not-json";
        assert!(parse_native_record(bytes, 0, &locator(bytes)).is_err());
    }

    #[test]
    fn claude_result_preserves_complete_native_block_without_renaming() {
        let bytes = br#"{"type":"user","uuid":"row","message":{"content":[{"type":"tool_result","tool_use_id":" call-1 ","is_error":true,"content":" exact text ","unknown":{"future":[1,2]}}]}}"#;
        let parsed = parse_native_record(bytes, 0, &locator(bytes)).unwrap();
        let row = &parsed.rows[0];
        let native = serde_json::json!({
            "type":"tool_result",
            "tool_use_id":" call-1 ",
            "is_error":true,
            "content":" exact text ",
            "unknown":{"future":[1,2]},
        });
        assert_eq!(row.tool_result.as_ref().unwrap().native_content, native);
        let annotation = claude_annotation(row, None, None).unwrap();
        assert_eq!(annotation.structured_content.as_ref(), Some(&native));
        let result = annotation.activity.unwrap().result.unwrap();
        assert_eq!(
            result.text,
            ActivityTextCapture::Present {
                value: " exact text ".to_owned(),
            }
        );
        assert_eq!(
            result.structured_content,
            ActivityJsonCapture::Present { value: native }
        );
    }

    #[test]
    fn claude_flattened_mcp_name_stays_native_and_facts_keep_raw_order() {
        let bytes = br#"{"type":"assistant","uuid":"row","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"mcp__forge__read","input":{"command":" c ","path":" p ","url":" u "}}]}}"#;
        let parsed = parse_native_record(bytes, 0, &locator(bytes)).unwrap();
        let annotation = claude_annotation(&parsed.rows[0], None, None).unwrap();
        let activity = annotation.activity.unwrap();
        let invocation = activity.invocation.unwrap();
        assert_eq!(invocation.tool, "mcp__forge__read");
        assert_eq!((invocation.protocol, invocation.server), (None, None));
        assert_eq!(
            activity
                .facts
                .iter()
                .map(|fact| (fact.kind, fact.value.as_str()))
                .collect::<Vec<_>>(),
            [
                (LiteralFactKind::Command, " c "),
                (LiteralFactKind::File, " p "),
                (LiteralFactKind::Url, " u "),
            ]
        );
    }

    #[test]
    fn claude_duplicate_result_content_retains_row_and_marks_capture_unavailable() {
        let bytes = br#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call","content":"one","content":"two"}]}}"#;
        let parsed = parse_native_record(bytes, 0, &locator(bytes)).unwrap();
        assert_eq!(parsed.rows.len(), 1);
        let annotation = claude_annotation(&parsed.rows[0], None, None).unwrap();
        assert!(annotation.structured_content.is_none());
        let result = annotation.activity.unwrap().result.unwrap();
        assert_eq!(result.text, ActivityTextCapture::Unavailable);
        assert_eq!(result.structured_content, ActivityJsonCapture::Unavailable);
    }
}
