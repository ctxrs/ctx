//! Thin OpenClaw legacy-session adapter for the shared borrowed JSONL family.
use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, EventIdentityInput,
    EventType, NativeItemKey, NativeSessionKey, PositionStability, RepositoryFileObservationKind,
    SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId, TypedKey,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use super::{complete_content, discover_inventory, normalization, openclaw_output_metadata};
use crate::repository_attribution::{
    apply_annotation, linked_outcome_evidence, AttributionInput, LinkedOutcomeInput,
    RepositoryAttributor, UnscopedFileObservation,
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        file_touches::visit_all_file_touch_drafts,
        normalization::provider_timestamp_value,
        source_backed::family::jsonl::{
            JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
            JsonlFamilyProjector, JsonlRecordRef,
        },
    },
    provider_sources::{provider_source_for_path, ProviderSourceStatus},
    CaptureError, OutputObservationKind, OutputOutcome, Result, MAX_OPENCLAW_SESSION_INDEX_BYTES,
    OPENCLAW_SOURCE_FORMAT,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "openclaw.legacy-session";
const NATIVE_SESSION_NAMESPACE: &str = "openclaw.legacy-session";
const NATIVE_EVENT_NAMESPACE: &str = "openclaw.legacy-event";
const NATIVE_EVENT_POSITION_KIND: &str = "openclaw.legacy-jsonl.raw-ordinal";
const LOGICAL_SESSION_KIND: &str = "openclaw-legacy-session";
const LOGICAL_EVENT_KIND: &str = "openclaw-legacy-event";
const SOURCE_SCHEMA_VARIANT: &str = "openclaw-legacy-jsonl-v2";
const PARSER_REVISION: &str = "openclaw-source-backed-v1";

#[derive(Debug, Clone, Copy, Default)]
struct OpenClawJsonlAdapter;

pub(crate) fn openclaw_source_backed_adapter_v0() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(OpenClawJsonlAdapter)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    index_relative_path: PathBuf,
    native_session_id: String,
    revision_digest: [u8; 32],
    index: Value,
    parent_native_session_id: Option<String>,
    root_native_session_id: Option<String>,
}

impl JsonlFamilyAdapter for OpenClawJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::OpenClaw
    }

    fn source_format(&self) -> &'static str {
        OPENCLAW_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::Replacement
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        match fs::symlink_metadata(root) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        }
        let selected = provider_source_for_path(CaptureProvider::OpenClaw, root.to_path_buf());
        if selected.status == ProviderSourceStatus::Unsupported {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: selected
                    .unsupported_reason
                    .unwrap_or("unsupported OpenClaw history format"),
            });
        }
        let inventory = discover_inventory(root)?;
        let canonical_root = fs::canonicalize(root)?;
        let authority_path = if fs::symlink_metadata(root)?.is_file() {
            canonical_root
                .parent()
                .ok_or(CaptureError::InvalidProviderTranscriptPath {
                    path: canonical_root.clone(),
                    reason: "selected OpenClaw transcript has no authority directory",
                })?
                .to_path_buf()
        } else {
            canonical_root
        };
        let authority = Arc::new(ProviderSourceRoot::open(&authority_path)?);
        let mut leaves = Vec::with_capacity(inventory.paths.len());
        let mut identities = BTreeSet::new();
        for path in inventory.paths {
            let transcript_relative_path = relative_to_authority(&authority, &path)?;
            let index_relative_path = transcript_relative_path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join("sessions.json");
            let native_session_id = native_session_id(&path);
            if !identities.insert(native_session_id.clone()) {
                return Err(CaptureError::InvalidPayload(
                    "OpenClaw inventory repeats a native session identity".to_owned(),
                ));
            }
            let source = source_key(&native_session_id)?;
            let transcript = authority.open_file(&transcript_relative_path)?;
            let compound = admit_compound(&authority, &path, &index_relative_path, &transcript)?;
            transcript.revalidate()?;
            let binding = Binding {
                index_relative_path,
                native_session_id,
                revision_digest: compound.revision_digest,
                index: compound.index,
                parent_native_session_id: compound.parent_native_session_id,
                root_native_session_id: compound.root_native_session_id,
            };
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                path,
                Arc::clone(&authority),
                transcript_relative_path,
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
        let binding = decode_binding(leaf)?;
        let compound = admit_compound(
            leaf.authority(),
            leaf.source_path(),
            &binding.index_relative_path,
            source_file.as_ref(),
        )?;
        if compound.revision_digest != binding.revision_digest
            || compound.parent_native_session_id != binding.parent_native_session_id
            || compound.root_native_session_id != binding.root_native_session_id
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let session_id = session_identity(leaf.source(), &binding.native_session_id)?;
        let session = SessionState::new(
            leaf.source_path(),
            &binding.native_session_id,
            &binding.index,
            binding.parent_native_session_id.as_deref(),
            binding.root_native_session_id.as_deref(),
            imported_at,
            session_id,
        )?;
        Ok(Box::new(OpenClawProjector {
            source: leaf.source().clone(),
            binding,
            session_id,
            session,
            index_file: compound.index_file,
            authority: Arc::clone(leaf.authority()),
            attributor: RepositoryAttributor::default(),
            pending_calls: HashMap::new(),
            running_processes: HashMap::new(),
        }))
    }
}

struct OpenClawProjector {
    source: SourceKey,
    binding: Binding,
    session_id: StableEntityId,
    session: SessionState,
    index_file: Option<OpenedProviderSourceFile>,
    authority: Arc<ProviderSourceRoot>,
    attributor: RepositoryAttributor,
    pending_calls: HashMap<String, PendingCall>,
    running_processes: HashMap<String, PendingCall>,
}

#[derive(Clone)]
struct PendingCall {
    origin_call_id: String,
    command: Option<String>,
    declared_workdir: Option<String>,
    event_sequence: u64,
    continuation_call_id_sha256: Vec<[u8; 32]>,
}

impl JsonlFamilyProjector for OpenClawProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let bytes = record.bytes();
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return Ok(());
        };
        if !value.is_object() {
            return Ok(());
        }
        if value.get("type").and_then(Value::as_str) == Some("session") {
            self.session.observe_header(&value);
            return Ok(());
        }
        let evidence = record.evidence();
        let line_number = usize::try_from(evidence.physical_ordinal())
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw line number exceeds platform limits",
            ))?;
        let occurred_at = provider_timestamp_value(value.get("timestamp"), self.session.started_at);
        let mut event = normalization::event_fact(
            evidence.physical_ordinal(),
            line_number,
            &value,
            occurred_at,
        );
        let tool_call = native_tool_call(&value);
        let tool_result = native_tool_result(&value);
        let output = openclaw_output_metadata(&value);
        if tool_call.is_some() {
            event.event_type = EventType::ToolCall;
        }
        if let Some(output) = &output {
            if output.kind == OutputObservationKind::Command {
                event.event_type = EventType::CommandOutput;
            }
        }
        let body = tool_call
            .as_ref()
            .and_then(|call| serde_json::to_string(call.block).ok())
            .or_else(|| {
                tool_result
                    .as_ref()
                    .and_then(|result| serde_json::to_string(result.message).ok())
            })
            .unwrap_or(event.lexical_text);
        if body.trim().is_empty() {
            return Ok(());
        }
        let (native_item_key, native_event_key) = native_event_keys(
            event.provider_event_hash.as_deref(),
            evidence.physical_ordinal(),
        )?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .map_err(contract)?;
        let event_sequence = event.provider_event_index.checked_mul(1_u64 << 16).ok_or(
            CaptureError::SystemInvariant("OpenClaw event sequence overflowed"),
        )?;
        let structured_content = tool_call
            .as_ref()
            .map(|call| call.block.clone())
            .or_else(|| tool_result.as_ref().map(|result| result.message.clone()));
        let mut record = CoreRecord::new_selected(
            event_id,
            self.session_id,
            self.session.root_session_id,
            self.source.clone(),
            event_sequence,
            event.event_type.as_str(),
            self.session.agent_type.as_str(),
            self.session.is_primary,
            PARSER_REVISION,
            body,
        )
        .map_err(contract)?;
        record.parent_session_id = self.session.parent_session_id;
        record.provider_session_id = Some(self.session.provider_session_id.clone());
        record.native_event_id = Some(native_event_key);
        record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
        record.role = event.role.map(|role| role.as_str().to_owned());
        record.branch = self.session.branch.clone();
        record.cwd = self.session.cwd.clone();
        let mut input = AttributionInput {
            activity_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
            session_cwd: self.session.cwd.clone(),
            structured_content,
            ..AttributionInput::default()
        };
        if let Some(call) = &tool_call {
            input.command = call.command.clone();
            input.declared_tool_workdir = call.declared_workdir.clone();
            input.file_observations = call.file_observations.clone();
            if let Some(call_id) = call.call_id.filter(|id| !id.is_empty()) {
                if self.pending_calls.len() < 4096 && !self.pending_calls.contains_key(call_id) {
                    let pending = call
                        .process_session_id
                        .and_then(|session_id| self.running_processes.get(session_id))
                        .cloned()
                        .map(|mut pending| {
                            if pending.continuation_call_id_sha256.len() < 64 {
                                pending
                                    .continuation_call_id_sha256
                                    .push(Sha256::digest(call_id.as_bytes()).into());
                            }
                            pending
                        })
                        .unwrap_or_else(|| PendingCall {
                            origin_call_id: call_id.to_owned(),
                            command: call.command.clone(),
                            declared_workdir: call.declared_workdir.clone(),
                            event_sequence,
                            continuation_call_id_sha256: Vec::new(),
                        });
                    input.command = pending.command.clone();
                    input.declared_tool_workdir = pending.declared_workdir.clone();
                    self.pending_calls.insert(call_id.to_owned(), pending);
                }
            }
        }
        let mut outcome_relevant = false;
        if let (Some(result), Some(output)) = (&tool_result, &output) {
            if let Some(context) = result
                .call_id
                .and_then(|call_id| self.pending_calls.remove(call_id))
            {
                if let Some(process_session_id) = result.running_process_session_id {
                    if self.running_processes.len() < 256
                        && !self.running_processes.contains_key(process_session_id)
                    {
                        self.running_processes
                            .insert(process_session_id.to_owned(), context);
                    }
                    return Ok(());
                }
                input.command = context.command.clone();
                input.declared_tool_workdir = context.declared_workdir.clone();
                if let (Some(command), Some(result_call_id)) =
                    (context.command.as_deref(), result.call_id)
                {
                    if let Some(linked) = linked_outcome_evidence(LinkedOutcomeInput {
                        provider: "openclaw",
                        command,
                        session_cwd: self.session.cwd.as_deref(),
                        declared_workdir: context.declared_workdir.as_deref(),
                        origin_call_id: &context.origin_call_id,
                        result_call_id,
                        origin_event_sequence: context.event_sequence,
                        continuation_call_id_sha256: &context.continuation_call_id_sha256,
                        result_record_sha256: Sha256::digest(bytes).into(),
                        observed_at_unix_ms: event.occurred_at.timestamp_millis(),
                        result_outcome: output.outcome.outcome,
                        result_output: result.output,
                        structured_commit_oid: result.structured_commit_oid,
                        output_repository_path: result.output_workdir,
                    }) {
                        outcome_relevant = true;
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
            if !outcome_relevant
                && !matches!(
                    output.outcome.outcome,
                    OutputOutcome::Failure | OutputOutcome::Timeout
                )
            {
                return Ok(());
            }
        }
        let annotation = self.attributor.attribute(input);
        apply_annotation(&mut record, annotation);
        record.validate_contract().map_err(contract)?;
        emit(record)
    }

    fn finish(&mut self) -> Result<()> {
        if let Some(index) = &self.index_file {
            index.revalidate()?;
        }
        self.authority.revalidate()
    }
}

struct NativeToolCall<'a> {
    block: &'a Value,
    call_id: Option<&'a str>,
    command: Option<String>,
    declared_workdir: Option<String>,
    file_observations: Vec<UnscopedFileObservation>,
    process_session_id: Option<&'a str>,
}

struct NativeToolResult<'a> {
    message: &'a Value,
    call_id: Option<&'a str>,
    output: &'a Value,
    structured_commit_oid: Option<&'a str>,
    output_workdir: Option<&'a str>,
    running_process_session_id: Option<&'a str>,
}

fn native_tool_call(value: &Value) -> Option<NativeToolCall<'_>> {
    let message = value.get("message").unwrap_or(value);
    let block = message
        .get("content")?
        .as_array()?
        .iter()
        .find(|block| block.get("type").and_then(Value::as_str) == Some("toolCall"))?;
    let arguments = block.get("arguments").and_then(Value::as_object);
    let string = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| arguments?.get(*key).and_then(Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    let command = string(&["command"]);
    let declared_workdir = string(&["workdir", "cwd"]);
    let tool_name = block
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = match tool_name.to_ascii_lowercase().as_str() {
        "read" | "read_file" | "grep" | "glob" | "search" => RepositoryFileObservationKind::Read,
        "edit" | "edit_file" | "apply_patch" => RepositoryFileObservationKind::Modified,
        "write" | "write_file" => RepositoryFileObservationKind::Unknown,
        _ => RepositoryFileObservationKind::Unknown,
    };
    let file_observations = ["path", "file_path", "filePath"]
        .into_iter()
        .filter_map(|key| arguments?.get(key).and_then(Value::as_str))
        .filter(|path| !path.trim().is_empty() && path.len() <= 16 * 1024)
        .take(64)
        .map(|path| UnscopedFileObservation {
            path: path.to_owned(),
            prior_path: None,
            kind,
        })
        .collect();
    Some(NativeToolCall {
        block,
        call_id: block.get("id").and_then(Value::as_str),
        command,
        declared_workdir,
        file_observations,
        process_session_id: arguments
            .and_then(|arguments| arguments.get("sessionId"))
            .and_then(Value::as_str),
    })
}

fn native_tool_result(value: &Value) -> Option<NativeToolResult<'_>> {
    let message = value.get("message").unwrap_or(value);
    let role = message.get("role").and_then(Value::as_str)?;
    if !matches!(role, "tool" | "toolResult") {
        return None;
    }
    let details = message.get("details");
    let output = details
        .or_else(|| message.get("content"))
        .unwrap_or(message);
    let structured_commit_oid = details.and_then(|details| {
        details
            .get("commit_oid")
            .or_else(|| details.get("commitOid"))
            .and_then(Value::as_str)
    });
    Some(NativeToolResult {
        message,
        call_id: message
            .get("toolCallId")
            .or_else(|| message.get("tool_call_id"))
            .and_then(Value::as_str),
        output,
        structured_commit_oid,
        output_workdir: details
            .and_then(|details| details.get("cwd"))
            .and_then(Value::as_str),
        running_process_session_id: details
            .filter(|details| details.get("status").and_then(Value::as_str) == Some("running"))
            .and_then(|details| details.get("sessionId"))
            .and_then(Value::as_str),
    })
}

struct CompoundAdmission {
    revision_digest: [u8; 32],
    index: Value,
    index_file: Option<OpenedProviderSourceFile>,
    parent_native_session_id: Option<String>,
    root_native_session_id: Option<String>,
}

fn admit_compound(
    authority: &ProviderSourceRoot,
    path: &Path,
    index_relative_path: &Path,
    transcript: &OpenedProviderSourceFile,
) -> Result<CompoundAdmission> {
    let index_file = match authority.open_file(index_relative_path) {
        Ok(index) => Some(index),
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let index_bytes = index_file
        .as_ref()
        .map(|index| index.read_all_bounded(MAX_OPENCLAW_SESSION_INDEX_BYTES))
        .transpose()?;
    if let Some(index) = &index_file {
        index.revalidate()?;
    }
    let observation = super::super::OpenClawSessionObservation::from_admitted(
        path.to_path_buf(),
        transcript.metadata(),
        index_file
            .as_ref()
            .zip(index_bytes.as_deref())
            .map(|(index, bytes)| (index.metadata(), bytes)),
    )?;
    let revision_digest =
        complete_content::exact_source_revision_digest(&observation.source_revision());
    let (parent_native_session_id, root_native_session_id) = index_bytes
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .map(|index| native_session_family(path, &index))
        .unwrap_or((None, None));
    Ok(CompoundAdmission {
        revision_digest,
        index: observation.index,
        index_file,
        parent_native_session_id,
        root_native_session_id,
    })
}

struct SessionState {
    provider_session_id: String,
    agent_id: Option<String>,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    branch: Option<String>,
    agent_type: AgentType,
    is_primary: bool,
}

impl SessionState {
    fn new(
        path: &Path,
        native_session_id: &str,
        index: &Value,
        native_parent_session_id: Option<&str>,
        native_root_session_id: Option<&str>,
        imported_at: DateTime<Utc>,
        direct_session_id: StableEntityId,
    ) -> Result<Self> {
        let agent_id =
            super::super::openclaw_agent_id(path).map(|value| super::capped_text(&value));
        let provider_session_id = native_session_id.to_owned();
        let parent_provider_session_id =
            native_parent_session_id.map(str::to_owned).or_else(|| {
                related_session_id(
                    index,
                    agent_id.as_deref(),
                    &["parentSessionId", "parent_session_id"],
                )
            });
        let root_provider_session_id = native_root_session_id
            .map(str::to_owned)
            .or_else(|| {
                related_session_id(
                    index,
                    agent_id.as_deref(),
                    &["rootSessionId", "root_session_id"],
                )
            })
            .or_else(|| parent_provider_session_id.clone());
        let parent_session_id = parent_provider_session_id
            .as_deref()
            .map(|related| related_session_identity(related, native_session_id, direct_session_id))
            .transpose()?;
        let root_session_id = root_provider_session_id
            .as_deref()
            .map(|related| related_session_identity(related, native_session_id, direct_session_id))
            .transpose()?
            .or(parent_session_id)
            .unwrap_or(direct_session_id);
        Ok(Self {
            provider_session_id,
            agent_id,
            parent_session_id,
            root_session_id,
            started_at: imported_at,
            cwd: None,
            branch: explicit_branch(index),
            agent_type: if parent_session_id.is_some() {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            is_primary: parent_session_id.is_none(),
        })
    }

    fn observe_header(&mut self, value: &Value) {
        if let Some(id) = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        {
            self.provider_session_id = super::qualify_session_id(self.agent_id.as_deref(), id);
        }
        self.started_at = provider_timestamp_value(value.get("timestamp"), self.started_at);
        self.cwd = value
            .get("cwd")
            .and_then(Value::as_str)
            .map(super::capped_text);
        self.branch = self.branch.clone().or_else(|| explicit_branch(value));
    }
}

fn source_key(native_session_id: &str) -> Result<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)?;
    SourceKey::derive(
        CaptureProvider::OpenClaw.as_str(),
        OPENCLAW_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )
    .map_err(contract)
}

fn session_identity(source: &SourceKey, native_session_id: &str) -> Result<StableEntityId> {
    let key = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &key,
    })
    .map_err(contract)
}

fn related_session_identity(
    related: &str,
    direct: &str,
    direct_session_id: StableEntityId,
) -> Result<StableEntityId> {
    if related == direct {
        return Ok(direct_session_id);
    }
    let source = source_key(related)?;
    session_identity(&source, related)
}

fn native_event_keys(
    native_record_id: Option<&str>,
    ordinal: u64,
) -> Result<(NativeItemKey, TypedKey)> {
    match native_record_id {
        Some(id) => {
            let key = TypedKey::utf8(id).map_err(contract)?;
            Ok((
                NativeItemKey::native_id(NATIVE_EVENT_NAMESPACE, key.clone()).map_err(contract)?,
                key,
            ))
        }
        None => Ok((
            NativeItemKey::certified_position(
                NATIVE_EVENT_POSITION_KIND,
                TypedKey::U64(ordinal),
                PositionStability::AppendStable,
            )
            .map_err(contract)?,
            TypedKey::U64(ordinal),
        )),
    }
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> Result<Binding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(CaptureError::InvalidPayload(
            "OpenClaw family binding is malformed".to_owned(),
        ));
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn relative_to_authority(authority: &ProviderSourceRoot, path: &Path) -> Result<PathBuf> {
    path.strip_prefix(authority.named_path())
        .map(Path::to_path_buf)
        .map_err(|_| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "OpenClaw transcripts must remain below their selected authority",
        })
}

fn native_session_id(path: &Path) -> String {
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("openclaw-session");
    super::qualify_session_id(
        super::super::openclaw_agent_id(path).as_deref(),
        fallback_id,
    )
}

fn related_session_id(index: &Value, agent_id: Option<&str>, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| index.get(*field).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(|value| super::qualify_session_id(agent_id, value))
}

fn native_session_family(path: &Path, index: &Value) -> (Option<String>, Option<String>) {
    let Some(entries) = index.as_object() else {
        return (None, None);
    };
    let direct_id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let Some((_, selected)) = entries.iter().find(|(_, entry)| {
        entry
            .get("sessionId")
            .and_then(Value::as_str)
            .is_some_and(|id| id == direct_id)
    }) else {
        return (None, None);
    };
    let Some(first_parent_key) = selected.get("spawnedBy").and_then(Value::as_str) else {
        return (None, None);
    };
    let mut current_key = first_parent_key;
    let mut parent = None;
    let mut root = None;
    let mut visited = BTreeSet::new();
    for depth in 0..16 {
        if !visited.insert(current_key.to_owned()) {
            return (None, None);
        }
        let Some(entry) = entries.get(current_key) else {
            return (None, None);
        };
        let Some(session_id) = entry.get("sessionId").and_then(Value::as_str) else {
            return (None, None);
        };
        let agent = current_key
            .strip_prefix("agent:")
            .and_then(|value| value.split(':').next());
        let qualified = super::qualify_session_id(agent, session_id);
        if depth == 0 {
            parent = Some(qualified.clone());
        }
        root = Some(qualified);
        let Some(next) = entry.get("spawnedBy").and_then(Value::as_str) else {
            break;
        };
        current_key = next;
    }
    let root = root.or_else(|| parent.clone());
    (parent, root)
}

fn explicit_branch(value: &Value) -> Option<String> {
    ["branch", "gitBranch", "git_branch"]
        .into_iter()
        .find_map(|field| value.get(field).and_then(Value::as_str))
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(super::capped_text)
}

fn touched_files(value: &Value) -> Result<Vec<String>> {
    let mut paths = BTreeSet::new();
    visit_all_file_touch_drafts(value, |draft| {
        paths.insert(draft.path);
        Ok::<(), CaptureError>(())
    })?;
    Ok(paths.into_iter().collect())
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HISTORY: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/repository_attribution/openclaw-native.jsonl"
    ));
    const SESSIONS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/repository_attribution/openclaw-sessions.json"
    ));

    #[test]
    fn native_tool_call_result_and_spawned_family_are_exact() {
        let lines = HISTORY
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let call = native_tool_call(&lines[1]).unwrap();
        assert_eq!(call.call_id, Some("call-1"));
        assert_eq!(call.command.as_deref(), Some("git commit -m exact"));
        assert_eq!(call.declared_workdir.as_deref(), Some("/tmp/repository"));

        let result = native_tool_result(&lines[2]).unwrap();
        assert_eq!(result.call_id, Some("call-1"));
        assert_eq!(result.output_workdir, Some("/tmp/repository"));
        assert_eq!(
            openclaw_output_metadata(&lines[2]).unwrap().outcome.outcome,
            OutputOutcome::Success
        );

        let index = serde_json::from_str::<Value>(SESSIONS).unwrap();
        assert_eq!(
            native_session_family(
                Path::new("/agents/worker/sessions/child-session.jsonl"),
                &index
            ),
            (
                Some("main/parent-session".to_owned()),
                Some("main/parent-session".to_owned())
            )
        );
    }
}
