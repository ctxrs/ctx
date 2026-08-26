//! Thin OpenClaw legacy-session adapter for the shared borrowed JSONL family.
use crate::{
    provider::source_backed::{BaseEventLookup as _, IndexBaseEventLookup},
    JsonlProviderRuntime,
};
use chrono::{DateTime, Utc};
use ctx_history_capture_model::normalization::{
    provider_explicit_result_value_text, provider_timestamp_value,
};
use ctx_history_core::{
    derive_event_id, derive_native_session_id, AgentScope, CaptureProvider, CoreRecord,
    EventIdentityInput, EventType, NativeItemKey, ProviderNativeSessionRelationship,
    SourceAnchorScope, SourceKey, StableEntityId, TypedKey,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    fs, io,
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Arc,
};

use super::{discover_inventory, normalization};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::family::jsonl::{
        JsonlAppendOccurrenceState, JsonlFamilyAdapter, JsonlFamilyAppendMode,
        JsonlFamilyInventory, JsonlFamilyLeaf, JsonlFamilyProjectionMode, JsonlFamilyProjector,
        JsonlFamilyTerminalProof, JsonlFamilyWorkerContext, JsonlOversizedRecordPolicy,
        JsonlReader, JsonlRecordRef, JsonlRecordRejections, JsonlTerminalAuthority,
        JsonlTerminalObservationRegion, SourceBackedRecordRejectionClass,
        SourceBackedRecordRejectionDrafts,
    },
    CaptureError, Result, MAX_OPENCLAW_SESSION_INDEX_BYTES, OPENCLAW_SOURCE_FORMAT,
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
const PARSER_REVISION: &str = "openclaw-source-backed-v20-direct-parent-explicit-root";
const MAX_TERMINAL_CALL_IDS: usize = 4096;
const MAX_TERMINAL_LINKAGE_IDS: usize = MAX_TERMINAL_CALL_IDS * 2;
const MAX_SELECTOR_CALL_ID_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy)]
struct OpenClawJsonlAdapter<R> {
    source_anchor_scope: SourceAnchorScope,
    runtime: PhantomData<fn() -> R>,
}

pub(crate) fn openclaw_source_backed_adapter_v0<R: JsonlProviderRuntime>(
) -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
    openclaw_source_backed_adapter_v0_with_source_root_lineage(None)
}

pub(crate) fn openclaw_source_backed_adapter_v0_with_source_root_lineage<
    R: JsonlProviderRuntime,
>(
    source_root_lineage: Option<[u8; 32]>,
) -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
    Arc::new(OpenClawJsonlAdapter {
        source_anchor_scope: source_root_lineage
            .map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
        runtime: PhantomData,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    index_relative_path: PathBuf,
    native_session_id: String,
    index: Value,
    native_session_family: OpenClawNativeSessionFamily,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum OpenClawNativeSessionFamily {
    Absent,
    Resolved { parent_native_session_id: String },
    Invalid,
}

impl<R: JsonlProviderRuntime> JsonlFamilyAdapter for OpenClawJsonlAdapter<R> {
    type Runtime = R;

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
        JsonlFamilyAppendMode::ProjectorPreflight(true)
    }

    fn oversized_record_policy(&self) -> JsonlOversizedRecordPolicy {
        JsonlOversizedRecordPolicy::RejectRecord
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        match fs::symlink_metadata(root) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        }
        reject_unsupported_openclaw_root(root)?;
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
        let mut exact_dependency_paths = BTreeSet::new();
        let mut exact_dependencies = Vec::new();
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
            let source = source_key_scoped(&native_session_id, self.source_anchor_scope)?;
            let transcript = Arc::new(authority.open_file(&transcript_relative_path)?);
            let compound = admit_compound(
                &authority,
                &path,
                &index_relative_path,
                Arc::clone(&transcript),
            )?;
            transcript.revalidate()?;
            if exact_dependency_paths.insert(index_relative_path.clone()) {
                if let Some(index_file) = &compound.index_file {
                    exact_dependencies.push(JsonlFamilyTerminalProof::exact_opened_path(
                        authority.named_path().join(&index_relative_path),
                        Arc::clone(&authority),
                        index_relative_path.clone(),
                        index_file,
                    )?);
                }
            }
            let binding = Binding {
                index_relative_path,
                native_session_id,
                index: compound.index,
                native_session_family: compound.native_session_family,
            };
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                path,
                Arc::clone(&authority),
                transcript_relative_path,
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?,
            )?);
        }
        Ok(
            JsonlFamilyInventory::present(self.provider(), root, authority, leaves)?
                .with_exact_dependencies(exact_dependencies),
        )
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = R>>> {
        self.projector_with_provider_checkpoint(
            leaf,
            source_file,
            imported_at,
            None,
            None,
            JsonlFamilyProjectionMode::Cold,
        )
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<IndexBaseEventLookup<R>>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = R>>> {
        let binding = decode_binding(leaf)?;
        let compound = admit_compound(
            leaf.authority(),
            leaf.source_path(),
            &binding.index_relative_path,
            Arc::clone(&source_file),
        )?;
        if compound.index != binding.index
            || compound.native_session_family != binding.native_session_family
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let session_id = session_identity(leaf.source(), &binding.native_session_id)?;
        let mut session = SessionState::new(
            leaf.source_path(),
            &binding.native_session_id,
            &binding.index,
            &binding.native_session_family,
            imported_at,
            session_id,
            self.source_anchor_scope,
        )?;
        let replacement_session = session.checkpoint();
        let restored = checkpoint
            .map(|checkpoint| decode_projector_checkpoint(checkpoint, &binding))
            .transpose()?;
        if let Some(restored) = restored {
            session.restore(restored.session);
        }
        Ok(Box::new(OpenClawProjector::<R> {
            source: leaf.source().clone(),
            native_session_id: binding.native_session_id,
            session_id,
            session,
            replacement_session,
            index_file: compound.index_file,
            authority: Arc::clone(leaf.authority()),
            terminal_authority: OpenClawTerminalAuthority::default(),
            fallback_identities: match (mode, base_event_lookup) {
                (JsonlFamilyProjectionMode::CertifiedAppend, Some(base_lookup)) => {
                    FallbackEventIdentityState::<R>::for_append(base_lookup)
                }
                _ => FallbackEventIdentityState::<R>::default(),
            },
            rejections: JsonlRecordRejections::new(
                leaf.source().clone(),
                CaptureProvider::OpenClaw,
                leaf.source_path().display().to_string(),
            ),
        }))
    }
}

struct OpenClawProjector<R: JsonlProviderRuntime> {
    source: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
    session: SessionState,
    replacement_session: SessionCheckpoint,
    index_file: Option<OpenedProviderSourceFile>,
    authority: Arc<ProviderSourceRoot>,
    terminal_authority: OpenClawTerminalAuthority,
    fallback_identities: FallbackEventIdentityState<R>,
    rejections: JsonlRecordRejections,
}

#[derive(Debug, Clone, Copy)]
struct FallbackEventIdentity {
    digest: [u8; 32],
    duplicate_occurrence: u64,
}

type FallbackEventIdentityState<R> = JsonlAppendOccurrenceState<[u8; 32], IndexBaseEventLookup<R>>;

#[cfg(test)]
fn source_key(native_session_id: &str) -> Result<SourceKey> {
    source_key_scoped(native_session_id, SourceAnchorScope::Unqualified)
}

fn source_key_scoped(
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<SourceKey> {
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::OpenClaw.as_str(),
        OPENCLAW_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
        source_anchor_scope,
    )
    .map_err(contract)
}

fn session_identity(source: &SourceKey, native_session_id: &str) -> Result<StableEntityId> {
    derive_native_session_id(
        source,
        LOGICAL_SESSION_KIND,
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)
}

fn related_session_identity(
    related: &str,
    direct: &str,
    direct_session_id: StableEntityId,
    source_anchor_scope: SourceAnchorScope,
) -> Result<StableEntityId> {
    if related == direct {
        return Ok(direct_session_id);
    }
    let source = source_key_scoped(related, source_anchor_scope)?;
    session_identity(&source, related)
}

fn native_event_keys<R: JsonlProviderRuntime>(
    native_record_id: Option<&str>,
    value: &Value,
    event: &normalization::OpenClawEventFact,
    source: &SourceKey,
    session_id: StableEntityId,
    state: &mut FallbackEventIdentityState<R>,
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
            let identity =
                next_fallback_event_identity::<R>(value, event, source, session_id, state)?;
            let parts = fallback_event_key_parts(identity)?;
            Ok((
                NativeItemKey::composite(NATIVE_EVENT_NAMESPACE, parts.clone())
                    .map_err(contract)?,
                TypedKey::composite(parts).map_err(contract)?,
            ))
        }
    }
}

fn next_fallback_event_identity<R: JsonlProviderRuntime>(
    value: &Value,
    event: &normalization::OpenClawEventFact,
    source: &SourceKey,
    session_id: StableEntityId,
    state: &mut FallbackEventIdentityState<R>,
) -> Result<FallbackEventIdentity> {
    let digest = fallback_event_digest(value, event)?;
    let occurrence = state.next(
        digest,
        || CaptureError::SystemInvariant("OpenClaw fallback duplicate occurrence overflowed"),
        |base_lookup, occurrence| {
            base_occurrence_exists::<R>(base_lookup, source, session_id, digest, occurrence)
        },
    )?;
    let identity = FallbackEventIdentity {
        digest,
        duplicate_occurrence: occurrence,
    };
    Ok(identity)
}

fn base_occurrence_exists<R: JsonlProviderRuntime>(
    base_lookup: &IndexBaseEventLookup<R>,
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

#[derive(Debug, Default)]
struct OpenClawRelatedSessionClaim {
    value: Option<String>,
    invalid: bool,
}

fn related_session_claim(
    index: &Value,
    agent_id: Option<&str>,
    fields: &[&str],
) -> OpenClawRelatedSessionClaim {
    let mut resolved = OpenClawRelatedSessionClaim::default();
    for field in fields {
        let Some(value) = index.get(*field) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let Some(claim) = value.as_str().filter(|claim| !claim.trim().is_empty()) else {
            resolved.invalid = true;
            continue;
        };
        let claim = super::qualify_session_id(agent_id, claim);
        if resolved
            .value
            .as_ref()
            .is_some_and(|current| current != &claim)
        {
            resolved.invalid = true;
        } else {
            resolved.value = Some(claim);
        }
    }
    resolved
}

fn native_session_family(path: &Path, index: &Value) -> OpenClawNativeSessionFamily {
    let direct_id = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let selected = super::super::openclaw_session_index_for_file(path, index);
    if selected.is_null() {
        return OpenClawNativeSessionFamily::Invalid;
    }
    let selected_spawned_by = match lineage_claim(&selected, "spawnedBy") {
        OpenClawLineageClaim::Absent => return OpenClawNativeSessionFamily::Absent,
        OpenClawLineageClaim::Valid(claim) => claim,
        OpenClawLineageClaim::Invalid => return OpenClawNativeSessionFamily::Invalid,
    };
    let Some(entries) = index.as_object() else {
        return OpenClawNativeSessionFamily::Invalid;
    };
    let mut matching = entries.iter().filter(|(_, entry)| {
        entry
            .get("sessionId")
            .and_then(Value::as_str)
            .is_some_and(|id| id == direct_id)
    });
    let Some((_, direct_entry)) = matching.next() else {
        return OpenClawNativeSessionFamily::Invalid;
    };
    if matching.next().is_some() {
        return OpenClawNativeSessionFamily::Invalid;
    }
    if lineage_claim(direct_entry, "spawnedBy") != OpenClawLineageClaim::Valid(selected_spawned_by)
    {
        return OpenClawNativeSessionFamily::Invalid;
    }
    let Some(parent_entry) = entries.get(selected_spawned_by) else {
        return OpenClawNativeSessionFamily::Invalid;
    };
    let OpenClawLineageClaim::Valid(parent_session_id) = lineage_claim(parent_entry, "sessionId")
    else {
        return OpenClawNativeSessionFamily::Invalid;
    };
    let parent_agent = selected_spawned_by
        .strip_prefix("agent:")
        .and_then(|value| value.split(':').next());
    OpenClawNativeSessionFamily::Resolved {
        parent_native_session_id: super::qualify_session_id(parent_agent, parent_session_id),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenClawLineageClaim<'a> {
    Absent,
    Valid(&'a str),
    Invalid,
}

fn lineage_claim<'a>(value: &'a Value, field: &str) -> OpenClawLineageClaim<'a> {
    let Some(value) = value.get(field) else {
        return OpenClawLineageClaim::Absent;
    };
    if value.is_null() {
        return OpenClawLineageClaim::Absent;
    }
    value
        .as_str()
        .filter(|claim| !claim.trim().is_empty())
        .map(OpenClawLineageClaim::Valid)
        .unwrap_or(OpenClawLineageClaim::Invalid)
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

fn reject_unsupported_openclaw_root(root: &Path) -> Result<()> {
    if root.is_dir() && !discover_inventory(root)?.paths.is_empty() {
        return Ok(());
    }
    if contains_openclaw_sqlite(root)? {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason:
                "OpenClaw SQLite history must be routed through the OpenClaw SQLite source adapter",
        });
    }
    Ok(())
}

fn contains_openclaw_sqlite(path: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if metadata.is_file() {
        return Ok(path.file_name().and_then(|name| name.to_str()) == Some("openclaw-agent.sqlite"));
    }
    if !metadata.is_dir() {
        return Ok(false);
    }
    if named_regular_file(&path.join("openclaw-agent.sqlite"))?
        || named_regular_file(&path.join("agent").join("openclaw-agent.sqlite"))?
    {
        return Ok(true);
    }
    let agents = if path.file_name().and_then(|name| name.to_str()) == Some("agents") {
        path.to_path_buf()
    } else {
        path.join("agents")
    };
    let entries = match fs::read_dir(&agents) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        if named_regular_file(&entry.path().join("agent").join("openclaw-agent.sqlite"))? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn named_regular_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests;
