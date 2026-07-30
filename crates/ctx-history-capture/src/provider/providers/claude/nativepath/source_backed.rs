//! Claude Code adapter for the shared borrowed JSONL replacement family.

use std::{
    collections::{BTreeSet, HashMap},
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, EventHydrationRequest, EventIdentityInput,
    EventType, HydratedProviderRecord, HydrationFailure, HydrationFailureKind,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    PositionStability, SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator,
    StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    record::parse_native_record,
    rows::{
        ClaudeEventKind, ClaudeOutputOutcome, ClaudePhysicalLocator, ClaudeRetainedRow,
        ClaudeSessionMetadata, CLAUDE_MAX_RECORD_ROWS,
    },
    source::{classify_claude_path, claude_projects_root, ClaudeSessionKey, SessionLayout},
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        normalization::provider_policy_event_text,
        providers::native_jsonl::visit_native_jsonl_files,
        source_backed::family::jsonl::{
            observe_opened_file, JsonlFamilyAdapter, JsonlFamilyHydrator, JsonlFamilyInventory,
            JsonlFamilyLeaf, JsonlFamilyProjector, JsonlFileObservation, JsonlRecordRef,
        },
    },
    CaptureError, Result, CLAUDE_PROJECTS_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "claude.session-leaf";
const SESSION_KEY_NAMESPACE: &str = "claude.session";
const NATIVE_EVENT_KEY_NAMESPACE: &str = "claude.event";
const EVENT_POSITION_KIND: &str = "claude.jsonl.event-position";
const LOGICAL_SESSION_KIND: &str = "claude-session";
const LOGICAL_EVENT_KIND: &str = "claude-event";
const SOURCE_SCHEMA_VARIANT: &str = "claude-nativepath-jsonl-v5";
const PARSER_REVISION: &str = "claude-shared-jsonl-v1";

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
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        let binding = decode_binding(leaf)?;
        let identities = identities(&binding)?;
        Ok(Box::new(ClaudeProjector {
            source: leaf.source().clone(),
            source_path: leaf.source_path().to_string_lossy().into_owned(),
            session: ClaudeSessionMetadata::new(binding.key.clone()),
            binding,
            identities,
        }))
    }

    fn hydrator(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> std::result::Result<Box<dyn JsonlFamilyHydrator>, HydrationFailure> {
        Ok(Box::new(ClaudeHydrator {
            source: leaf.source().clone(),
            native_session_key: session_typed_key(&decode_binding(leaf).map_err(unavailable)?.key)
                .map_err(unavailable)?,
            source_file,
        }))
    }
}

struct Identities {
    native_session_key: TypedKey,
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
}

impl JsonlFamilyProjector for ClaudeProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(LexicalDocument) -> Result<()>,
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
            emit(lexical_document(
                &self.source,
                &self.source_path,
                &self.binding,
                &self.identities,
                &self.session,
                row,
            )?)?;
        }
        Ok(())
    }
}

struct ClaudeHydrator {
    source: SourceKey,
    native_session_key: TypedKey,
    source_file: Arc<OpenedProviderSourceFile>,
}

impl JsonlFamilyHydrator for ClaudeHydrator {
    fn hydrate(
        &mut self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        let (byte_offset, byte_length, ordinal) =
            validate_locator(request.locator(), &self.source, &self.native_session_key)?;
        let length = usize::try_from(byte_length)
            .map_err(|_| invalid("Claude locator range exceeds platform limits"))?;
        if length == 0 || length > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(1) {
            return Err(invalid("Claude locator range is invalid"));
        }
        if byte_offset > 0
            && self
                .source_file
                .read_exact_range(byte_offset - 1, 1, 1)
                .map_err(stale)?
                != b"\n"
        {
            return Err(stale("Claude record boundary changed"));
        }
        let wire = self
            .source_file
            .read_exact_range(
                byte_offset,
                length,
                MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(1),
            )
            .map_err(stale)?;
        let bytes = strip_jsonl_terminator(&wire);
        if Sha256::digest(bytes).as_slice() != request.locator().record_digest() {
            return Err(stale("Claude record digest changed"));
        }
        let value: Value =
            serde_json::from_slice(bytes).map_err(|_| stale("Claude record JSON changed"))?;
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or_else(|| invalid("Claude locator ordinal is invalid"))?;
        let (text, _) = super::super::complete_content::claude_complete_content_message_record(
            &value,
            line_number,
        )
        .ok_or_else(|| stale("Claude record has no exact message display"))?;
        let retained = provider_policy_event_text(EventType::Message, &text, &Value::Null);
        let text = if retained.text.trim().is_empty() {
            "message".to_owned()
        } else {
            retained.text
        };
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: text.into_bytes(),
        })
    }
}

fn lexical_document(
    source: &SourceKey,
    source_path: &str,
    binding: &Binding,
    identities: &Identities,
    session: &ClaudeSessionMetadata,
    row: ClaudeRetainedRow,
) -> Result<LexicalDocument> {
    let native_item_key = native_item_key(&row)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: identities.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(contract)?;
    let byte_length = row
        .locator
        .byte_end_exclusive
        .checked_sub(row.locator.byte_start)
        .ok_or(CaptureError::SystemInvariant(
            "Claude record range underflowed",
        ))?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: row.locator.byte_start,
            byte_length,
            physical_ordinal: row.identity.source_record_ordinal,
            native_session_key: Some(identities.native_session_key.clone()),
            native_event_key: Some(native_event_typed_key(&row)?),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        row.locator.record_sha256,
    )
    .map_err(contract)?;
    let event_sequence = row
        .identity
        .source_record_ordinal
        .checked_mul(1_u64 << 16)
        .and_then(|value| value.checked_add(row.identity.source_subrecord_index))
        .ok_or(CaptureError::SystemInvariant(
            "Claude event sequence overflowed",
        ))?;
    let touched_files = row
        .tool_call
        .as_ref()
        .map(|call| {
            call.file_touches
                .iter()
                .map(|touch| touch.path.clone())
                .collect()
        })
        .unwrap_or_default();
    Ok(LexicalDocument {
        event_id,
        session_id: identities.session_id,
        parent_session_id: identities.parent_session_id,
        root_session_id: identities.root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(binding.key.provider_session_id()),
        branch: session.git_branch.clone(),
        source_path: Some(source_path.to_owned()),
        agent_type: identities.agent_type.to_owned(),
        is_primary: identities.is_primary,
        event_sequence,
        occurred_at_unix_ms: row
            .occurred_at
            .as_deref()
            .and_then(|value| value.parse::<DateTime<Utc>>().ok())
            .map(|value| value.timestamp_millis()),
        event_type: event_kind(row.kind).to_owned(),
        role: row.role.clone(),
        body: lexical_body(&row),
        workspace: binding.project_dir.to_str().map(str::to_owned),
        cwd: session.cwd.clone(),
        touched_files,
    })
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
        native_session_key,
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

fn native_item_key(row: &ClaudeRetainedRow) -> Result<NativeItemKey> {
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
    NativeItemKey::certified_position(
        EVENT_POSITION_KIND,
        native_event_typed_key(row)?,
        PositionStability::AppendStable,
    )
    .map_err(contract)
}

fn native_event_typed_key(row: &ClaudeRetainedRow) -> Result<TypedKey> {
    TypedKey::composite(vec![
        row.native_record_id
            .as_deref()
            .map(TypedKey::utf8)
            .transpose()
            .map_err(contract)?
            .unwrap_or(TypedKey::Null),
        TypedKey::U64(row.identity.source_record_ordinal),
        TypedKey::U64(row.identity.source_subrecord_index),
    ])
    .map_err(contract)
}

fn lexical_body(row: &ClaudeRetainedRow) -> String {
    let text = row
        .body
        .clone()
        .or_else(|| {
            row.tool_call.as_ref().map(|call| {
                let mut parts = vec!["tool call".to_owned()];
                parts.extend(call.tool_name.clone());
                parts.extend(call.call_id.clone());
                parts.extend(call.file_touches.iter().map(|touch| touch.path.clone()));
                parts.join(" ")
            })
        })
        .or_else(|| {
            row.sparse_output.as_ref().map(|output| {
                format!(
                    "tool output {}{}{}",
                    match output.outcome {
                        ClaudeOutputOutcome::Failure => "failure",
                        ClaudeOutputOutcome::Timeout => "timeout",
                    },
                    output
                        .call_id
                        .as_deref()
                        .map(|id| format!(" {id}"))
                        .unwrap_or_default(),
                    output
                        .exit_code
                        .map(|code| format!(" exit {code}"))
                        .unwrap_or_default()
                )
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

fn validate_locator(
    locator: &SourceRecordLocator,
    source: &SourceKey,
    native_session_key: &TypedKey,
) -> std::result::Result<(u64, u64, u64), HydrationFailure> {
    locator.validate_contract().map_err(invalid)?;
    source
        .validate_exact_descriptor(locator.source())
        .map_err(invalid)?;
    if locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(invalid("Claude locator revision is invalid"));
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key: observed_session,
        ..
    } = locator.coordinate()
    else {
        return Err(invalid("Claude locator is not a JSONL range"));
    };
    if observed_session.as_ref() != Some(native_session_key) {
        return Err(invalid("Claude locator session key is invalid"));
    }
    Ok((*byte_offset, *byte_length, *physical_ordinal))
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

fn strip_jsonl_terminator(record: &[u8]) -> &[u8] {
    let record = record.strip_suffix(b"\n").unwrap_or(record);
    record.strip_suffix(b"\r").unwrap_or(record)
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn invalid(error: impl std::fmt::Display) -> HydrationFailure {
    HydrationFailure {
        kind: HydrationFailureKind::InvalidLocator,
        detail: error.to_string(),
    }
}

fn stale(error: impl std::fmt::Display) -> HydrationFailure {
    HydrationFailure {
        kind: HydrationFailureKind::StaleRecordEvidence,
        detail: error.to_string(),
    }
}

fn unavailable(error: impl std::fmt::Display) -> HydrationFailure {
    HydrationFailure {
        kind: HydrationFailureKind::TemporarilyUnavailable,
        detail: error.to_string(),
    }
}

#[cfg(test)]
#[path = "source_backed/tests.rs"]
mod tests;

pub(crate) mod registration {
    use super::claude_source_backed_adapter;
    use crate::{
        provider::source_backed::{
            executable_route, family::jsonl::jsonl_family_driver, SourceBackedCoordinatorResult,
            SourceBackedProviderRegistry, SourceBackedRouteSelection,
            SourceBackedSelectorAuthority,
        },
        ProviderSource,
    };

    pub(crate) fn register(
        registry: &mut SourceBackedProviderRegistry,
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
    ) -> SourceBackedCoordinatorResult<()> {
        let driver = jsonl_family_driver(claude_source_backed_adapter(), source.path.clone());
        registry.register(executable_route(
            source,
            selection,
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )?);
        Ok(())
    }
}
