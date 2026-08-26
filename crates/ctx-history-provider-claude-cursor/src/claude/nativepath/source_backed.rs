//! Claude Code adapter for the shared borrowed JSONL replacement family.

use std::{
    collections::{HashMap, HashSet},
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
    ProviderNativeSessionRelationship, SourceAnchorScope, SourceKey, StableEntityId, TypedKey,
    CORE_ACTIVITY_REVISION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    record::parse_native_record,
    rows::{ClaudePhysicalLocator, ClaudeRetainedRow, ClaudeSessionMetadata},
    source::{classify_claude_path, claude_projects_root, ClaudeSessionKey, SessionLayout},
};
use crate::CLAUDE_PROJECTS_SOURCE_FORMAT;
use ctx_history_jsonl::{
    fit_jsonl_activity, selected_content_fits, JsonlActivityObservedBytes, JsonlFamilyAdapter,
    JsonlOversizedRecordPolicy, JsonlRecordRejections, SourceBackedRecordRejectionClass,
    SourceBackedRecordRejectionDrafts,
};
use ctx_history_provider_runtime::{
    observe_opened_file,
    source_io::{OpenedProviderSourceFile, ProviderSourceRoot},
    CaptureError, JsonlAppendOccurrenceState, JsonlFamilyAppendMode, JsonlFamilyProjectionMode,
    JsonlFamilyProjector, JsonlFamilyRejectedLeaf, JsonlRecordRef, ProviderBaseEventLookup,
    ProviderJsonlInventory, ProviderJsonlLeaf, ProviderJsonlReader, ProviderJsonlRuntime,
    ProviderJsonlWorkerContext, ProviderRuntimeBinding, Result,
};
use ctx_history_source_io::visit_bounded_tree_files_isolating_selected;

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
const PARSER_REVISION: &str = "claude-shared-jsonl-core-activity-v2-record-rejections";

mod binding;
mod normalized_body;
mod preflight;

use binding::*;
use normalized_body::{event_kind, lexical_body};
use preflight::{
    parsed_record_is_rejected, scope_claude_row_validation_error, stable_native_event_identity,
    typed_claude_record_key, validate_claude_row_annotation, ClaudeDuplicatePlan,
    ClaudePreflightError, ClaudeRecordKeyField, ClaudeRowValidationError,
};

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

    fn oversized_record_policy(&self) -> JsonlOversizedRecordPolicy {
        JsonlOversizedRecordPolicy::RejectRecord
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
        let mut observed = Vec::new();
        let mut unreadable = Vec::new();
        visit_bounded_tree_files_isolating_selected::<CaptureError, _>(
            &canonical_root,
            &mut |candidate| {
                candidate
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    == Some("jsonl")
            },
            &mut |source_file| {
                let path = source_file.path().to_path_buf();
                let relative_path = relative_to_authority(&authority, &path)?;
                let opened = authority.open_file(&relative_path)?;
                observed.push((path.clone(), observe_opened_file(&path, &opened)?));
                Ok(())
            },
            &mut |path, error| {
                if !is_quarantinable_claude_leaf_error(&error) {
                    return Err(error);
                }
                unreadable.push((path.to_path_buf(), error.to_string()));
                Ok(())
            },
        )?;

        let mut claimed_sources = HashMap::<[u8; 32], SourceKey>::new();
        let mut duplicate_sources = HashSet::new();
        let mut source_owner_paths = HashMap::<[u8; 32], PathBuf>::new();
        let mut prepared_observed = Vec::new();
        observed.sort_by(|left, right| left.0.cmp(&right.0));
        for (path, observation) in observed {
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
            let proof = TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?;
            let digest = source.exact_descriptor_digest();
            if claim_claude_source(&mut claimed_sources, &source)? == ClaudeSourceClaim::Duplicate {
                duplicate_sources.insert(digest);
            }
            source_owner_paths
                .entry(digest)
                .and_modify(|owner| {
                    if path.as_path() < owner.as_path() {
                        *owner = path.clone();
                    }
                })
                .or_insert_with(|| path.clone());
            prepared_observed.push((source, path, relative_path, proof, observation));
        }
        let mut prepared_unreadable = Vec::new();
        unreadable.sort_by(|left, right| left.0.cmp(&right.0));
        for (path, detail) in unreadable {
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
            let digest = source.exact_descriptor_digest();
            if claim_claude_source(&mut claimed_sources, &source)? == ClaudeSourceClaim::Duplicate {
                duplicate_sources.insert(digest);
            }
            source_owner_paths
                .entry(digest)
                .and_modify(|owner| {
                    if path.as_path() < owner.as_path() {
                        *owner = path.clone();
                    }
                })
                .or_insert_with(|| path.clone());
            prepared_unreadable.push((
                source,
                path,
                relative_path,
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?,
                detail,
            ));
        }

        let mut leaves = Vec::new();
        let mut rejected_leaves = Vec::new();
        for (source, path, relative_path, proof, observation) in prepared_observed {
            let digest = source.exact_descriptor_digest();
            if duplicate_sources.contains(&digest) {
                let mut rejected = JsonlFamilyRejectedLeaf::bind_observed(
                    path.clone(),
                    relative_path,
                    observation,
                    proof,
                    0,
                )
                .with_quarantined_source(source.clone());
                if source_owner_paths
                    .get(&digest)
                    .is_some_and(|owner| owner == &path)
                {
                    rejected = rejected.with_logical_source_failure(
                        source.clone(),
                        format!(
                            "Claude transcript {} repeats a native session identity claimed by another transcript",
                            path.display()
                        ),
                    );
                }
                rejected_leaves.push(rejected);
            } else {
                leaves.push(ProviderJsonlLeaf::bind_observed(
                    source,
                    path,
                    Arc::clone(&authority),
                    relative_path,
                    proof,
                    observation,
                ));
            }
        }
        for (source, path, relative_path, proof, detail) in prepared_unreadable {
            let digest = source.exact_descriptor_digest();
            let duplicate = duplicate_sources.contains(&digest);
            let failure_detail = if duplicate {
                format!(
                    "Claude transcript {} repeats a native session identity claimed by another transcript and is unreadable: {detail}",
                    path.display()
                )
            } else {
                format!(
                    "Claude transcript {} is unreadable: {detail}",
                    path.display()
                )
            };
            let mut rejected =
                JsonlFamilyRejectedLeaf::bind_unobserved(path.clone(), relative_path, proof, 0)
                    .with_quarantined_source(source.clone());
            if !duplicate
                || source_owner_paths
                    .get(&digest)
                    .is_some_and(|owner| owner == &path)
            {
                rejected = rejected.with_logical_source_failure(source, failure_detail);
            }
            rejected_leaves.push(rejected);
        }
        ProviderJsonlInventory::present_with_rejected(
            self.provider(),
            root,
            authority,
            leaves,
            rejected_leaves,
        )
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
            rejections: JsonlRecordRejections::new(
                leaf.source().clone(),
                CaptureProvider::Claude,
                leaf.source_path().to_string_lossy(),
            ),
            duplicate_plan: ClaudeDuplicatePlan::default(),
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
    rejections: JsonlRecordRejections,
    duplicate_plan: ClaudeDuplicatePlan,
    fallback_identities: JsonlAppendOccurrenceState<[u8; 32], ProviderBaseEventLookup<B>>,
}

#[derive(Debug, Clone, Copy)]
struct FallbackEventIdentity {
    digest: [u8; 32],
    duplicate_occurrence: u64,
}

impl<B: ProviderRuntimeBinding> ClaudeProjector<B> {
    fn reject_record(
        &mut self,
        record: JsonlRecordRef<'_>,
        class: SourceBackedRecordRejectionClass,
        detail: impl Into<String>,
    ) -> Result<()> {
        self.rejections.record(record, class, detail);
        Ok(())
    }
}

impl<B> JsonlFamilyProjector for ClaudeProjector<B>
where
    B: ProviderRuntimeBinding,
{
    type Runtime = ProviderJsonlRuntime<B>;

    fn preflight_with_failure_scope(
        &mut self,
        reader: &mut JsonlReader,
        certified_prefix_end: Option<u64>,
    ) -> std::result::Result<bool, ClaudePreflightError> {
        preflight::validate_source(
            reader,
            &self.source_path,
            &self.binding,
            &self.source,
            self.identities.session_id,
            certified_prefix_end,
            &mut self.duplicate_plan,
        )
    }

    fn retry_replacement(&mut self) {
        // Keep the whole-source duplicate plan produced by preflight: the
        // replacement pass needs it to suppress superseded observations.
        self.session = ClaudeSessionMetadata::new(self.binding.key.clone());
        self.rejections = JsonlRecordRejections::new(
            self.source.clone(),
            CaptureProvider::Claude,
            self.source_path.clone(),
        );
        self.fallback_identities = JsonlAppendOccurrenceState::default();
    }

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<B>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if record.oversized() {
            return self.reject_record(
                record,
                SourceBackedRecordRejectionClass::UnsupportedRecord,
                "Claude JSONL record exceeds the supported size bound",
            );
        }
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
            Err(error) => {
                return self.reject_record(
                    record,
                    SourceBackedRecordRejectionClass::MalformedRecord,
                    format!("malformed Claude JSONL record: {error}"),
                );
            }
        };
        if parsed_record_is_rejected(&parsed, &self.binding) {
            return self.reject_record(
                record,
                SourceBackedRecordRejectionClass::UnsupportedRecord,
                "Claude JSONL record is outside the admitted session shape",
            );
        }
        for row in &parsed.rows {
            match stable_native_event_identity(row, &self.source, self.identities.session_id) {
                Ok(_) => {}
                Err(error) => match scope_claude_row_validation_error(error) {
                    ClaudePreflightError::RecordRejection { detail } => {
                        return self.reject_record(
                            record,
                            SourceBackedRecordRejectionClass::UnsupportedRecord,
                            detail,
                        );
                    }
                    ClaudePreflightError::Internal(error) => return Err(error),
                    ClaudePreflightError::LogicalSourceFailure { .. } => {
                        return Err(CaptureError::SystemInvariant(
                            "Claude row validation produced a source failure",
                        ));
                    }
                },
            }
            match validate_claude_row_annotation(
                row,
                parsed.cwd.as_deref(),
                parsed.git_branch.as_deref(),
            ) {
                Ok(()) => {}
                Err(ClaudePreflightError::RecordRejection { detail }) => {
                    return self.reject_record(
                        record,
                        SourceBackedRecordRejectionClass::UnsupportedRecord,
                        detail,
                    );
                }
                Err(ClaudePreflightError::Internal(error)) => return Err(error),
                Err(ClaudePreflightError::LogicalSourceFailure { .. }) => {
                    return Err(CaptureError::SystemInvariant(
                        "Claude row validation produced a source failure",
                    ));
                }
            }
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
            let retained = self
                .duplicate_plan
                .retains(&row, &self.source, self.identities.session_id)
                .map_err(|error| match error {
                    ClaudeRowValidationError::Record(_) => CaptureError::SystemInvariant(
                        "Claude native identity changed after prevalidation",
                    ),
                    ClaudeRowValidationError::Fatal(error) => error,
                })?;
            if !retained {
                continue;
            }
            let normalized_body = lexical_body(&row);
            let annotation =
                claude_annotation(&row, parsed.cwd.as_deref(), parsed.git_branch.as_deref())
                    .map_err(|error| match error {
                        ClaudeRowValidationError::Record(_) => CaptureError::SystemInvariant(
                            "Claude row annotation changed after prevalidation",
                        ),
                        ClaudeRowValidationError::Fatal(error) => error,
                    })?;
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
        self.rejections.count()
    }

    fn take_record_rejections(&mut self) -> SourceBackedRecordRejectionDrafts {
        self.rejections.take_drafts()
    }
}

fn claude_annotation(
    row: &ClaudeRetainedRow,
    declared_cwd: Option<&str>,
    declared_branch: Option<&str>,
) -> std::result::Result<CoreRecordAnnotation, ClaudeRowValidationError> {
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
            provider_call_id = Some(typed_claude_record_key(
                ClaudeRecordKeyField::ProviderCallId,
                call_id,
            )?);
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
            provider_call_id = Some(typed_claude_record_key(
                ClaudeRecordKeyField::ProviderCallId,
                call_id,
            )?);
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
    let scope =
        source_root_lineage.map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage);
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::Claude.as_str(),
        CLAUDE_PROJECTS_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SOURCE_ANCHOR_NAMESPACE,
        session_typed_key(key)?,
        scope,
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

fn native_record_key_parts(
    row: &ClaudeRetainedRow,
) -> std::result::Result<Option<Vec<TypedKey>>, ClaudeRowValidationError> {
    let Some(native_record_id) = row.native_record_id.as_deref() else {
        return Ok(None);
    };
    Ok(Some(vec![
        typed_claude_record_key(ClaudeRecordKeyField::NativeRecordId, native_record_id)?,
        TypedKey::U64(row.identity.source_subrecord_index),
    ]))
}

fn prevalidated_native_record_key_parts(row: &ClaudeRetainedRow) -> Result<Option<Vec<TypedKey>>> {
    native_record_key_parts(row).map_err(|error| match error {
        ClaudeRowValidationError::Record(_) => {
            CaptureError::SystemInvariant("Claude native identity changed after prevalidation")
        }
        ClaudeRowValidationError::Fatal(error) => error,
    })
}

fn native_item_key(
    row: &ClaudeRetainedRow,
    fallback_identity: Option<FallbackEventIdentity>,
) -> Result<NativeItemKey> {
    if let Some(key_parts) = prevalidated_native_record_key_parts(row)? {
        return NativeItemKey::composite(NATIVE_EVENT_KEY_NAMESPACE, key_parts).map_err(contract);
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
    if let Some(key_parts) = prevalidated_native_record_key_parts(row)? {
        return TypedKey::composite(key_parts).map_err(contract);
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

#[cfg(test)]
mod tests;
