//! Thin OpenClaw legacy-session adapter for the shared borrowed JSONL family.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, EventHydrationRequest,
    EventIdentityInput, EventType, HydratedProviderRecord, HydrationFailure, HydrationFailureKind,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    PositionStability, SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator,
    StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{complete_content, discover_inventory, normalization, openclaw_output_metadata};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        file_touches::visit_all_file_touch_drafts,
        normalization::provider_timestamp_value,
        source_backed::family::jsonl::{
            JsonlFamilyAdapter, JsonlFamilyHydrator, JsonlFamilyInventory, JsonlFamilyLeaf,
            JsonlFamilyProjector, JsonlRecordRef,
        },
    },
    provider_sources::{provider_source_for_path, ProviderSourceStatus},
    CaptureError, OutputObservationKind, OutputOutcome, Result, MAX_OPENCLAW_SESSION_INDEX_BYTES,
    MAX_PROVIDER_JSONL_LINE_BYTES, OPENCLAW_SOURCE_FORMAT,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "openclaw.legacy-session";
const NATIVE_SESSION_NAMESPACE: &str = "openclaw.legacy-session";
const NATIVE_EVENT_NAMESPACE: &str = "openclaw.legacy-event";
const NATIVE_EVENT_POSITION_KIND: &str = "openclaw.legacy-jsonl.raw-ordinal";
const LOGICAL_SESSION_KIND: &str = "openclaw-legacy-session";
const LOGICAL_EVENT_KIND: &str = "openclaw-legacy-event";
const SOURCE_SCHEMA_VARIANT: &str = "openclaw-legacy-jsonl-v1";
const PARSER_REVISION: &str = "openclaw-source-backed-v0";

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
        if compound.revision_digest != binding.revision_digest {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let session_id = session_identity(leaf.source(), &binding.native_session_id)?;
        let session = SessionState::new(
            leaf.source_path(),
            &binding.native_session_id,
            &binding.index,
            imported_at,
            session_id,
        )?;
        Ok(Box::new(OpenClawProjector {
            source: leaf.source().clone(),
            source_path: leaf.source_path().display().to_string(),
            binding,
            session_id,
            session,
            index_file: compound.index_file,
            authority: Arc::clone(leaf.authority()),
        }))
    }

    fn hydrator(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> std::result::Result<Box<dyn JsonlFamilyHydrator>, HydrationFailure> {
        let binding = decode_binding(leaf).map_err(unavailable)?;
        let compound = admit_compound(
            leaf.authority(),
            leaf.source_path(),
            &binding.index_relative_path,
            source_file.as_ref(),
        )
        .map_err(stale)?;
        if compound.revision_digest != binding.revision_digest {
            return Err(stale("OpenClaw compound source revision changed"));
        }
        Ok(Box::new(OpenClawHydrator {
            source: leaf.source().clone(),
            binding,
            source_file,
            index_file: compound.index_file,
            authority: Arc::clone(leaf.authority()),
        }))
    }
}

struct OpenClawProjector {
    source: SourceKey,
    source_path: String,
    binding: Binding,
    session_id: StableEntityId,
    session: SessionState,
    index_file: Option<OpenedProviderSourceFile>,
    authority: Arc<ProviderSourceRoot>,
}

impl JsonlFamilyProjector for OpenClawProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(LexicalDocument) -> Result<()>,
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
        if let Some(output) = openclaw_output_metadata(&value) {
            if !matches!(
                output.outcome.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            ) {
                return Ok(());
            }
            if output.kind == OutputObservationKind::Command {
                event.event_type = EventType::CommandOutput;
            }
        }
        let body = event.lexical_text;
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
        let touched_files = touched_files(&value)?;
        let locator = SourceRecordLocator::new(
            self.source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: evidence.byte_start(),
                byte_length: evidence
                    .byte_end_exclusive()
                    .checked_sub(evidence.byte_start())
                    .ok_or(CaptureError::SystemInvariant(
                        "OpenClaw record range underflowed",
                    ))?,
                physical_ordinal: evidence.physical_ordinal(),
                native_session_key: Some(
                    TypedKey::utf8(self.binding.native_session_id.clone()).map_err(contract)?,
                ),
                native_event_key: Some(native_event_key),
            },
            LocatorRevisionPolicy::ExactSourceRevision,
            Some(self.binding.revision_digest),
            Sha256::digest(bytes).into(),
        )
        .map_err(contract)?;
        emit(LexicalDocument {
            event_id,
            session_id: self.session_id,
            parent_session_id: self.session.parent_session_id,
            root_session_id: self.session.root_session_id,
            source: self.source.clone(),
            locator,
            provider_session_id: Some(self.session.provider_session_id.clone()),
            branch: self.session.branch.clone(),
            source_path: Some(self.source_path.clone()),
            agent_type: AgentType::Primary.as_str().to_owned(),
            is_primary: true,
            event_sequence: event.provider_event_index,
            occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
            event_type: event.event_type.as_str().to_owned(),
            role: event.role.map(|role| role.as_str().to_owned()),
            body,
            workspace: None,
            cwd: self.session.cwd.clone(),
            touched_files,
        })
    }

    fn finish(&mut self) -> Result<()> {
        if let Some(index) = &self.index_file {
            index.revalidate()?;
        }
        self.authority.revalidate()
    }
}

struct OpenClawHydrator {
    source: SourceKey,
    binding: Binding,
    source_file: Arc<OpenedProviderSourceFile>,
    index_file: Option<OpenedProviderSourceFile>,
    authority: Arc<ProviderSourceRoot>,
}

impl JsonlFamilyHydrator for OpenClawHydrator {
    fn hydrate(
        &mut self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        let (byte_offset, byte_length, ordinal, native_event_key) =
            validate_locator(request.locator(), &self.source, &self.binding)?;
        let length = usize::try_from(byte_length)
            .map_err(|_| invalid("OpenClaw locator range exceeds platform limits"))?;
        if length == 0 || length > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2) {
            return Err(invalid("OpenClaw locator range is invalid"));
        }
        if byte_offset > 0
            && self
                .source_file
                .read_exact_range(byte_offset - 1, 1, 1)
                .map_err(stale)?
                != b"\n"
        {
            return Err(stale("OpenClaw record boundary changed"));
        }
        let wire = self
            .source_file
            .read_exact_range(
                byte_offset,
                length,
                MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2),
            )
            .map_err(stale)?;
        let payload = strip_jsonl_terminator(&wire);
        if Sha256::digest(payload).as_slice() != request.locator().record_digest() {
            return Err(stale("OpenClaw record digest changed"));
        }
        let value: Value =
            serde_json::from_slice(payload).map_err(|_| stale("OpenClaw record JSON changed"))?;
        match (&native_event_key, value.get("id").and_then(Value::as_str)) {
            (TypedKey::Utf8(expected), Some(observed)) if expected == observed => {}
            (TypedKey::U64(expected), _) if *expected == ordinal => {}
            _ => return Err(stale("OpenClaw record identity changed")),
        }
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or_else(|| invalid("OpenClaw locator ordinal is invalid"))?;
        let mut event =
            normalization::event_fact(ordinal, line_number, &value, DateTime::<Utc>::UNIX_EPOCH);
        if let Some(output) = openclaw_output_metadata(&value) {
            if output.kind == OutputObservationKind::Command {
                event.event_type = EventType::CommandOutput;
            }
        }
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: event.lexical_text.into_bytes(),
        })
    }

    fn finish(&mut self) -> std::result::Result<(), HydrationFailure> {
        if let Some(index) = &self.index_file {
            index.revalidate().map_err(stale)?;
        }
        self.authority.revalidate().map_err(stale)
    }
}

struct CompoundAdmission {
    revision_digest: [u8; 32],
    index: Value,
    index_file: Option<OpenedProviderSourceFile>,
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
    Ok(CompoundAdmission {
        revision_digest,
        index: observation.index,
        index_file,
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
}

impl SessionState {
    fn new(
        path: &Path,
        native_session_id: &str,
        index: &Value,
        imported_at: DateTime<Utc>,
        direct_session_id: StableEntityId,
    ) -> Result<Self> {
        let agent_id =
            super::super::openclaw_agent_id(path).map(|value| super::capped_text(&value));
        let provider_session_id = native_session_id.to_owned();
        let parent_provider_session_id = related_session_id(
            index,
            agent_id.as_deref(),
            &["parentSessionId", "parent_session_id"],
        );
        let root_provider_session_id = related_session_id(
            index,
            agent_id.as_deref(),
            &["rootSessionId", "root_session_id"],
        )
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

fn validate_locator(
    locator: &SourceRecordLocator,
    source: &SourceKey,
    binding: &Binding,
) -> std::result::Result<(u64, u64, u64, TypedKey), HydrationFailure> {
    locator.validate_contract().map_err(invalid)?;
    source
        .validate_exact_descriptor(locator.source())
        .map_err(invalid)?;
    if locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
        || locator.certified_source_revision_digest() != Some(&binding.revision_digest)
    {
        return Err(invalid("OpenClaw locator revision is invalid"));
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = locator.coordinate()
    else {
        return Err(invalid("OpenClaw locator is not a JSONL range"));
    };
    if native_session_key.as_ref() != Some(&TypedKey::Utf8(binding.native_session_id.clone())) {
        return Err(invalid("OpenClaw locator session key is invalid"));
    }
    let native_event_key = native_event_key
        .clone()
        .ok_or_else(|| invalid("OpenClaw locator event key is absent"))?;
    Ok((
        *byte_offset,
        *byte_length,
        *physical_ordinal,
        native_event_key,
    ))
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
#[path = "source_backed_tests.rs"]
mod tests;
