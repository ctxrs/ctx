use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, PositionStability, SessionIdentityInput, SourceAnchor,
    SourceKey, StableEntityId, TypedKey,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::*;
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::family::jsonl::{
        JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
        JsonlFamilyProjector, JsonlRecordRef,
    },
    CaptureError, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};

const SOURCE_SCHEMA_VARIANT: &str = "meta-json-messages-jsonl-v1";
const SOURCE_ANCHOR_NAMESPACE: &str = "mistral-vibe-session-id";
const NATIVE_SESSION_NAMESPACE: &str = "mistral-vibe-session";
const NATIVE_EVENT_NAMESPACE: &str = "mistral-vibe-message";
const NATIVE_EVENT_POSITION_KIND: &str = "mistral-vibe-messages-jsonl-ordinal";
const LOGICAL_SESSION_KIND: &str = "mistral-vibe-session";
const LOGICAL_EVENT_KIND: &str = "mistral-vibe-event";
const PARSER_REVISION: &str = "mistral-vibe-source-backed-v2";
const SOURCE_REVISION_DIGEST_DOMAIN: &[u8] = b"ctx.mistral-vibe.source-revision.v1\0";

#[derive(Debug, Clone, Copy)]
pub(crate) struct MistralVibeJsonlAdapter;

pub(crate) fn scan_mistral_vibe_source_backed() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(MistralVibeJsonlAdapter)
}

#[derive(Debug)]
struct Draft {
    source: SourceKey,
    source_path: PathBuf,
    messages_relative_path: PathBuf,
    metadata_relative_path: PathBuf,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    session_id: StableEntityId,
    started_at_unix_ms: i64,
    cwd: Option<String>,
    branch: Option<String>,
    revision_digest: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    metadata_relative_path: PathBuf,
    provider_session_id: String,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    started_at_unix_ms: i64,
    cwd: Option<String>,
    branch: Option<String>,
    revision_digest: [u8; 32],
    is_primary: bool,
}

impl JsonlFamilyAdapter for MistralVibeJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::MistralVibe
    }

    fn source_format(&self) -> &'static str {
        MISTRAL_VIBE_SOURCE_FORMAT
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
        if fs::symlink_metadata(root)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            return JsonlFamilyInventory::missing(self.provider(), root);
        }
        let mut discovered = Vec::new();
        visit_mistral_vibe_session_sources(root, &mut |source| {
            discovered.push(source);
            Ok(())
        })?;
        discovered.sort_by(|left, right| left.messages_path.cmp(&right.messages_path));
        let selected = fs::canonicalize(root)?;
        let authority_path = if fs::symlink_metadata(root)?.is_file() {
            selected
                .parent()
                .ok_or(CaptureError::InvalidProviderTranscriptPath {
                    path: selected.clone(),
                    reason: "Mistral Vibe selected file has no authority directory",
                })?
                .to_path_buf()
        } else {
            selected
        };
        let authority = Arc::new(ProviderSourceRoot::open(&authority_path)?);
        let mut drafts = Vec::with_capacity(discovered.len());
        let mut sessions = HashSet::with_capacity(discovered.len());
        for native in discovered {
            let metadata_relative_path = relative_to_authority(&authority, &native.metadata_path)?;
            let messages_relative_path = relative_to_authority(&authority, &native.messages_path)?;
            let admitted =
                admit_metadata(&authority, &metadata_relative_path, &messages_relative_path)?;
            let (session, _) =
                SessionFact::from_admitted(&native, DateTime::<Utc>::UNIX_EPOCH, &admitted.bytes)?;
            if !sessions.insert(session.provider_session_id.clone()) {
                return Err(CaptureError::InvalidPayload(
                    "Mistral Vibe inventory repeats a session ID".to_owned(),
                ));
            }
            let source = source_key(&session.provider_session_id)?;
            let session_id = session_identity(&source, &session.provider_session_id)?;
            let revision_digest = admitted.revision_digest(&source)?;
            drafts.push(Draft {
                source,
                source_path: authority.named_path().join(&messages_relative_path),
                messages_relative_path,
                metadata_relative_path,
                provider_session_id: session.provider_session_id,
                parent_provider_session_id: session.parent_provider_session_id,
                session_id,
                started_at_unix_ms: session.started_at.timestamp_millis(),
                cwd: session.cwd,
                branch: mistral_vibe_metadata_string(&session.metadata, "git_branch"),
                revision_digest,
            });
        }
        let by_session = drafts
            .iter()
            .map(|draft| (draft.provider_session_id.as_str(), draft))
            .collect::<BTreeMap<_, _>>();
        let mut leaves = Vec::with_capacity(drafts.len());
        for draft in &drafts {
            let parent_session_id = draft
                .parent_provider_session_id
                .as_deref()
                .map(provider_session_identity)
                .transpose()?;
            let root_session_id = root_session_identity(draft, &by_session)?;
            let binding = Binding {
                metadata_relative_path: draft.metadata_relative_path.clone(),
                provider_session_id: draft.provider_session_id.clone(),
                session_id: draft.session_id,
                parent_session_id,
                root_session_id,
                started_at_unix_ms: draft.started_at_unix_ms,
                cwd: draft.cwd.clone(),
                branch: draft.branch.clone(),
                revision_digest: draft.revision_digest,
                is_primary: draft.parent_provider_session_id.is_none(),
            };
            leaves.push(JsonlFamilyLeaf::observe(
                draft.source.clone(),
                draft.source_path.clone(),
                Arc::clone(&authority),
                draft.messages_relative_path.clone(),
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
        Ok(Box::new(MistralProjector {
            source: leaf.source().clone(),
            binding: decode_binding(leaf)?,
        }))
    }
}

struct MistralProjector {
    source: SourceKey,
    binding: Binding,
}

impl JsonlFamilyProjector for MistralProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        if let Some(document) = core_record(&self.source, &self.binding, record)? {
            emit(document)?;
        }
        Ok(())
    }
}

fn core_record(
    source: &SourceKey,
    binding: &Binding,
    record: JsonlRecordRef<'_>,
) -> Result<Option<CoreRecord>> {
    let bytes = record.bytes();
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Ok(None);
    };
    let Ok(role) = valid_mistral_vibe_record_role(&value) else {
        return Ok(None);
    };
    let mut event_type = mistral_vibe_event_type(role, &value);
    let output = (event_type == EventType::ToolOutput).then(|| output_classification(&value));
    if output.as_ref().is_some_and(|output| {
        !matches!(
            output.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        )
    }) {
        return Ok(None);
    }
    let body = if let Some(output) = &output {
        if output.kind == OutputObservationKind::Command {
            event_type = EventType::CommandOutput;
        }
        mistral_vibe_lexical_text(&value, role, true)
    } else {
        mistral_vibe_lexical_text(&value, role, false)
    };
    if body.is_empty() {
        return Ok(None);
    }
    let evidence = record.evidence();
    let ordinal = evidence.physical_ordinal();
    let native_event_id = provider_native_event_id(&value);
    let native_item_key = match native_event_id.as_deref() {
        Some(id) => NativeItemKey::native_id(
            NATIVE_EVENT_NAMESPACE,
            TypedKey::utf8(id).map_err(contract)?,
        )
        .map_err(contract)?,
        None => NativeItemKey::certified_position(
            NATIVE_EVENT_POSITION_KIND,
            TypedKey::U64(ordinal),
            PositionStability::AppendStable,
        )
        .map_err(contract)?,
    };
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: binding.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(contract)?;
    let native_event_id = native_event_id
        .map(TypedKey::utf8)
        .transpose()
        .map_err(contract)?
        .unwrap_or(TypedKey::U64(ordinal));
    let role = crate::provider::normalization::provider_role(Some(role));
    let touched_files = collect_touched_paths(&value)?;
    let tool_name = value
        .get("name")
        .or_else(|| value.get("tool_name"))
        .cloned();
    let structured_content = (!touched_files.is_empty() || tool_name.is_some()).then(|| {
        serde_json::json!({
            "tool_name": tool_name,
            "file_touches": touched_files,
        })
    });
    let agent_type = if binding.is_primary {
        AgentType::Primary
    } else {
        AgentType::Subagent
    };
    let mut record = CoreRecord::new_selected(
        event_id,
        binding.session_id,
        binding.root_session_id,
        source.clone(),
        ordinal,
        event_type.as_str(),
        agent_type.as_str(),
        binding.is_primary,
        PARSER_REVISION,
        body,
    )
    .map_err(contract)?;
    record.parent_session_id = binding.parent_session_id;
    record.provider_session_id = Some(binding.provider_session_id.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(
        native_jsonl_timestamp(&value)
            .map(|timestamp| timestamp.timestamp_millis())
            .unwrap_or(binding.started_at_unix_ms),
    );
    record.role = Some(role.as_str().to_owned());
    record.branch = binding.branch.clone();
    record.cwd = binding.cwd.clone();
    record.content.structured_content = structured_content;
    record.validate_contract().map_err(contract)?;
    Ok(Some(record))
}

struct AdmittedMetadata {
    bytes: Vec<u8>,
    observation: SourceObservation,
}

impl AdmittedMetadata {
    fn revision_digest(&self, source: &SourceKey) -> Result<[u8; 32]> {
        let observation = projection_observation(source, &self.observation)?;
        let mut digest = Sha256::new();
        digest.update(SOURCE_REVISION_DIGEST_DOMAIN);
        digest.update((observation.revision().len() as u64).to_be_bytes());
        digest.update(observation.revision());
        Ok(digest.finalize().into())
    }
}

fn admit_metadata(
    authority: &ProviderSourceRoot,
    metadata_relative_path: &Path,
    messages_relative_path: &Path,
) -> Result<AdmittedMetadata> {
    let metadata = authority.open_file(metadata_relative_path)?;
    let messages = authority.open_file(messages_relative_path)?;
    let bytes = metadata.read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES)?;
    let observation = SourceObservation {
        canonical_metadata_path: authority.named_path().join(metadata_relative_path),
        canonical_messages_path: authority.named_path().join(messages_relative_path),
        metadata: FileStamp::from_metadata(metadata.metadata())?,
        messages: FileStamp::from_metadata(messages.metadata())?,
        metadata_sha256: Sha256::digest(&bytes).into(),
        exact_content_revision: super::super::source::mistral_vibe_source_revision_from_admitted(
            metadata.metadata(),
            messages.metadata(),
        )?,
    };
    metadata.revalidate()?;
    messages.revalidate()?;
    Ok(AdmittedMetadata { bytes, observation })
}

fn projection_observation(
    source: &SourceKey,
    observation: &SourceObservation,
) -> Result<ctx_history_core::SourceObservation> {
    #[derive(Serialize)]
    struct Composite<'a> {
        capture_revision: u32,
        policy_revision: u32,
        metadata: &'a FileStamp,
        messages: &'a FileStamp,
        metadata_sha256: [u8; 32],
        exact_content_revision: &'a str,
    }
    ctx_history_core::SourceObservation::new(
        source.clone(),
        "mistral-vibe-meta-messages-observation-v1",
        serde_json::to_vec(&Composite {
            capture_revision: MISTRAL_VIBE_CAPTURE_REVISION,
            policy_revision: MISTRAL_VIBE_POLICY_REVISION,
            metadata: &observation.metadata,
            messages: &observation.messages,
            metadata_sha256: observation.metadata_sha256,
            exact_content_revision: &observation.exact_content_revision,
        })?,
    )
    .map_err(contract)
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> Result<Binding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(CaptureError::InvalidPayload(
            "Mistral Vibe family binding is malformed".to_owned(),
        ));
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn relative_to_authority(authority: &ProviderSourceRoot, path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path)?
        .strip_prefix(authority.named_path())
        .map(Path::to_path_buf)
        .map_err(|_| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Mistral Vibe leaves must share one authority root",
        })
}

fn source_key(native_session_id: &str) -> Result<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)?;
    SourceKey::derive(
        CaptureProvider::MistralVibe.as_str(),
        MISTRAL_VIBE_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )
    .map_err(contract)
}

fn session_identity(source: &SourceKey, native_session_id: &str) -> Result<StableEntityId> {
    let native = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native,
    })
    .map_err(contract)
}

fn provider_session_identity(native_session_id: &str) -> Result<StableEntityId> {
    let source = source_key(native_session_id)?;
    session_identity(&source, native_session_id)
}

fn root_session_identity(
    lineage: &Draft,
    lineages: &BTreeMap<&str, &Draft>,
) -> Result<StableEntityId> {
    let mut current = lineage;
    let mut root = lineage.session_id;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current.provider_session_id.as_str()) {
            return Err(CaptureError::InvalidPayload(
                "Mistral Vibe session lineage contains a cycle".to_owned(),
            ));
        }
        let Some(parent_id) = current.parent_provider_session_id.as_deref() else {
            return Ok(root);
        };
        root = provider_session_identity(parent_id)?;
        let Some(parent) = lineages.get(parent_id) else {
            return Ok(root);
        };
        current = parent;
    }
}

fn mistral_vibe_lexical_text(value: &Value, role: &str, failed_output: bool) -> String {
    if failed_output {
        format!(
            "Mistral Vibe failed {} output",
            value.get("name").and_then(Value::as_str).unwrap_or("tool")
        )
    } else {
        mistral_vibe_event_text(role, value, mistral_vibe_event_type(role, value))
    }
}

fn provider_native_event_id(value: &Value) -> Option<String> {
    value
        .get("message_id")
        .or_else(|| value.get("tool_call_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
