//! Thin OpenClaw legacy-session adapter for the shared borrowed JSONL family.
use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, EventIdentityInput,
    EventType, NativeItemKey, NativeSessionKey, RepositoryAbstentionReason,
    RepositoryFileObservationKind, SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId,
    TypedKey,
};
use ctx_history_index::BaseEventIdentityLookup;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use super::{discover_inventory, normalization, openclaw_output_metadata};
use crate::repository_attribution::{
    apply_annotation, linked_outcome_evidence, AttributionInput, LinkedOutcomeInput,
    RepositoryAttributor, UnscopedFileObservation,
};
#[cfg(test)]
use crate::OutputOutcome;
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        normalization::provider_timestamp_value,
        source_backed::family::jsonl::{
            JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
            JsonlFamilyProjector, JsonlRecordRef,
        },
    },
    provider_sources::{provider_source_for_path, ProviderSourceStatus},
    CaptureError, OutputObservationKind, Result, MAX_OPENCLAW_SESSION_INDEX_BYTES,
    OPENCLAW_SOURCE_FORMAT,
};

mod checkpoint;
mod native;
mod projection;

use checkpoint::*;
use native::*;

const SOURCE_ANCHOR_NAMESPACE: &str = "openclaw.legacy-session";
const NATIVE_SESSION_NAMESPACE: &str = "openclaw.legacy-session";
const NATIVE_EVENT_NAMESPACE: &str = "openclaw.legacy-event";
const FALLBACK_EVENT_ID_VERSION: &str = "openclaw.fallback-event.v1";
const FALLBACK_EVENT_ID_DOMAIN: &[u8] = b"ctx-openclaw-fallback-event-id-v1\0";
const LOGICAL_SESSION_KIND: &str = "openclaw-legacy-session";
const LOGICAL_EVENT_KIND: &str = "openclaw-legacy-event";
const SOURCE_SCHEMA_VARIANT: &str = "openclaw-legacy-jsonl-v2";
const PARSER_REVISION: &str = "openclaw-source-backed-v3-block-projection";
const MAX_PENDING_CALLS: usize = 4096;
const MAX_RUNNING_PROCESSES: usize = 256;

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
        self.projector_with_provider_checkpoint(leaf, source_file, imported_at, None, None)
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<BaseEventIdentityLookup>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        let binding = decode_binding(leaf)?;
        let compound = admit_compound(
            leaf.authority(),
            leaf.source_path(),
            &binding.index_relative_path,
            source_file.as_ref(),
        )?;
        if compound.index != binding.index
            || compound.parent_native_session_id != binding.parent_native_session_id
            || compound.root_native_session_id != binding.root_native_session_id
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let session_id = session_identity(leaf.source(), &binding.native_session_id)?;
        let mut session = SessionState::new(
            leaf.source_path(),
            &binding.native_session_id,
            &binding.index,
            binding.parent_native_session_id.as_deref(),
            binding.root_native_session_id.as_deref(),
            imported_at,
            session_id,
        )?;
        let restored = checkpoint
            .map(|checkpoint| decode_projector_checkpoint(checkpoint, &binding))
            .transpose()?;
        let (pending_calls, running_processes, linkage_capacity_exceeded) = restored.map_or_else(
            || (HashMap::new(), HashMap::new(), false),
            |restored| {
                session.restore(restored.session);
                (
                    restored.pending_calls,
                    restored.running_processes,
                    restored.linkage_capacity_exceeded,
                )
            },
        );
        Ok(Box::new(OpenClawProjector {
            source: leaf.source().clone(),
            native_session_id: binding.native_session_id,
            session_id,
            session,
            index_file: compound.index_file,
            authority: Arc::clone(leaf.authority()),
            attributor: RepositoryAttributor::default(),
            pending_calls,
            running_processes,
            linkage_capacity_exceeded,
            fallback_identities: FallbackEventIdentityState::new(base_event_lookup),
        }))
    }
}

struct OpenClawProjector {
    source: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
    session: SessionState,
    index_file: Option<OpenedProviderSourceFile>,
    authority: Arc<ProviderSourceRoot>,
    attributor: RepositoryAttributor,
    pending_calls: HashMap<String, PendingCallState>,
    running_processes: HashMap<String, PendingCallState>,
    linkage_capacity_exceeded: bool,
    fallback_identities: FallbackEventIdentityState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingCall {
    origin_call_id: String,
    command: Option<String>,
    declared_workdir: Option<String>,
    event_sequence: u64,
    continuation_call_id_sha256: Vec<[u8; 32]>,
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
    value: &Value,
    event: &normalization::OpenClawEventFact,
    source: &SourceKey,
    session_id: StableEntityId,
    state: &mut FallbackEventIdentityState,
) -> Result<(NativeItemKey, TypedKey)> {
    match native_record_id {
        Some(id) => {
            let key = TypedKey::utf8(id).map_err(contract)?;
            Ok((
                NativeItemKey::native_id(NATIVE_EVENT_NAMESPACE, key.clone()).map_err(contract)?,
                key,
            ))
        }
        None => {
            let identity = next_fallback_event_identity(value, event, source, session_id, state)?;
            let parts = fallback_event_key_parts(identity)?;
            Ok((
                NativeItemKey::composite(NATIVE_EVENT_NAMESPACE, parts.clone())
                    .map_err(contract)?,
                TypedKey::composite(parts).map_err(contract)?,
            ))
        }
    }
}

#[cfg(test)]
fn remember_pending_call(
    pending_calls: &mut HashMap<String, PendingCallState>,
    linkage_capacity_exceeded: &mut bool,
    capacity: usize,
    call_id: &str,
    state: PendingCallState,
) {
    if let Some(existing) = pending_calls.get_mut(call_id) {
        *existing = PendingCallState::Ambiguous;
    } else if pending_calls.len() < capacity {
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
                "openclaw_tool_result_call_id_is_ambiguous",
            ));
            (None, true)
        }
        PendingCallLookup::Missing => {
            let (reason, detail) = if linkage_capacity_exceeded {
                (
                    RepositoryAbstentionReason::LinkageCapacityExceeded,
                    "openclaw_tool_result_linkage_capacity_exceeded",
                )
            } else {
                (
                    RepositoryAbstentionReason::ProviderOutputUnjoined,
                    "openclaw_tool_result_has_no_exact_unique_call_link",
                )
            };
            input.outcome_abstentions.push((reason, detail));
            (None, true)
        }
    }
}

fn next_fallback_event_identity(
    value: &Value,
    event: &normalization::OpenClawEventFact,
    source: &SourceKey,
    session_id: StableEntityId,
    state: &mut FallbackEventIdentityState,
) -> Result<FallbackEventIdentity> {
    let digest = fallback_event_digest(value, event)?;
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
                "OpenClaw fallback duplicate occurrence overflowed",
            ))?,
    );
    Ok(identity)
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
                    "OpenClaw fallback duplicate occurrence overflowed",
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
    let native_item_key =
        NativeItemKey::composite(NATIVE_EVENT_NAMESPACE, fallback_event_key_parts(identity)?)
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

fn fallback_event_digest(
    value: &Value,
    event: &normalization::OpenClawEventFact,
) -> Result<[u8; 32]> {
    let logical = serde_json::to_vec(&(
        event.event_type.as_str(),
        event.role.map(|role| role.as_str()),
        value,
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

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests;
