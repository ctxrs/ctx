//! Pi adapter for the shared borrowed JSONL replacement family.

use std::{
    collections::{BTreeSet, HashMap},
    fs, io,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
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
#[cfg(test)]
use std::cell::Cell;

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        file_touches::visit_provider_file_touch_drafts_with_limit,
        provider_path_identity,
        providers::native_jsonl::visit_native_jsonl_files,
        source_backed::family::jsonl::{
            observe_opened_file, probe_first_record, JsonlFamilyAdapter, JsonlFamilyHydrator,
            JsonlFamilyInventory, JsonlFamilyLeaf, JsonlFamilyProjector, JsonlFileObservation,
            JsonlRecordRef,
        },
    },
    CaptureError, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::super::{
    pi_event_type,
    text::{pi_entry_text, pi_event_role, pi_result_content},
    PI_SOURCE_FORMAT,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "pi.session";
const NATIVE_SESSION_NAMESPACE: &str = "pi.session";
const NATIVE_EVENT_NAMESPACE: &str = "pi.entry";
const NATIVE_EVENT_POSITION_KIND: &str = "pi.jsonl.record-ordinal";
const LOGICAL_SESSION_KIND: &str = "pi-session";
const LOGICAL_EVENT_KIND: &str = "pi-event";
const SOURCE_SCHEMA_VARIANT: &str = "pi-nativepath-jsonl-v1";
const PARSER_REVISION: &str = "pi-shared-jsonl-v1";
const MAX_TOUCHES_PER_RECORD: usize = 63;

#[cfg(test)]
thread_local! {
    static HEADER_PROBES: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_pi_header_probes() {
    HEADER_PROBES.set(0);
}

#[cfg(test)]
pub(super) fn pi_header_probes() -> usize {
    HEADER_PROBES.get()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PiSourceBackedRoot {
    path: PathBuf,
}

impl PiSourceBackedRoot {
    pub(crate) fn winning(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if is_historical_omp_root(&path) {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path,
                reason: "historical Pi roots are accepted only when explicitly configured",
            });
        }
        Ok(Self { path })
    }

    pub(crate) fn explicit(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn pi_source_backed_adapter() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(PiJsonlAdapter::default())
}

#[derive(Default)]
struct PiJsonlAdapter {
    bindings: Mutex<HashMap<PathBuf, CachedBinding>>,
}

#[derive(Clone)]
struct CachedBinding {
    observation: JsonlFileObservation,
    binding: Binding,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    native_session_id: String,
    parent_session_id: Option<String>,
    cwd: Option<String>,
    header_digest: [u8; 32],
}

impl JsonlFamilyAdapter for PiJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        PI_SOURCE_FORMAT
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
        let canonical_root = fs::canonicalize(root)?;
        let authority_path = if fs::symlink_metadata(root)?.is_file() {
            canonical_root
                .parent()
                .ok_or(CaptureError::InvalidProviderTranscriptPath {
                    path: canonical_root.clone(),
                    reason: "selected Pi transcript has no authority directory",
                })?
                .to_path_buf()
        } else {
            canonical_root
        };
        let authority = Arc::new(ProviderSourceRoot::open(&authority_path)?);
        let mut paths = BTreeSet::new();
        visit_native_jsonl_files(root, self.provider(), &mut |path| {
            paths.insert(fs::canonicalize(path)?);
            Ok(())
        })?;

        let previous = self
            .bindings
            .lock()
            .map_err(|_| contract("Pi binding catalog lock was poisoned"))?
            .clone();
        let mut next = HashMap::with_capacity(paths.len());
        let mut sources = HashMap::<[u8; 32], JsonlFileObservation>::new();
        let mut leaves = Vec::with_capacity(paths.len());
        for path in paths {
            let relative_path = relative_to_authority(&authority, &path)?;
            let opened = Arc::new(authority.open_file(&relative_path)?);
            let observation = observe_opened_file(&path, opened.as_ref())?;
            let (binding, identity_probe) = match previous.get(&path) {
                Some(cached) if cached.observation == observation => (cached.binding.clone(), None),
                _ => {
                    let (binding, probe) =
                        probe_first_record(&path, &opened, parse_header_binding)?;
                    (binding, Some(probe))
                }
            };
            let source = source_key(&binding.native_session_id)?;
            let source_digest = source.exact_descriptor_digest();
            if let Some(selected) = sources.get(&source_digest) {
                if selected == &observation {
                    next.insert(
                        path,
                        CachedBinding {
                            observation,
                            binding,
                        },
                    );
                    continue;
                }
                return Err(CaptureError::InvalidPayload(
                    "Pi inventory repeats a native session identity".to_owned(),
                ));
            }
            sources.insert(source_digest, observation.clone());
            next.insert(
                path.clone(),
                CachedBinding {
                    observation,
                    binding: binding.clone(),
                },
            );
            let binding_key = TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?;
            leaves.push(match identity_probe {
                Some(probe) => JsonlFamilyLeaf::observe_after_identity_probe(
                    source,
                    path,
                    Arc::clone(&authority),
                    relative_path,
                    binding_key,
                    probe,
                )?,
                None => JsonlFamilyLeaf::observe(
                    source,
                    path,
                    Arc::clone(&authority),
                    relative_path,
                    binding_key,
                )?,
            });
        }
        *self
            .bindings
            .lock()
            .map_err(|_| contract("Pi binding catalog lock was poisoned"))? = next;
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        let binding = decode_binding(leaf)?;
        let session_id = session_identity(leaf.source(), &binding.native_session_id)?;
        let parent_session_id = binding
            .parent_session_id
            .as_deref()
            .map(session_identity_for_native)
            .transpose()?;
        Ok(Box::new(PiProjector {
            source: leaf.source().clone(),
            source_path: provider_path_identity(leaf.source_path())?,
            root_session_id: parent_session_id.unwrap_or(session_id),
            parent_session_id,
            session_id,
            binding,
        }))
    }

    fn hydrator(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> std::result::Result<Box<dyn JsonlFamilyHydrator>, HydrationFailure> {
        Ok(Box::new(PiHydrator {
            source: leaf.source().clone(),
            binding: decode_binding(leaf).map_err(unavailable)?,
            source_file,
        }))
    }
}

struct PiProjector {
    source: SourceKey,
    source_path: String,
    binding: Binding,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
}

impl JsonlFamilyProjector for PiProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(LexicalDocument) -> Result<()>,
    ) -> Result<()> {
        let evidence = record.evidence();
        let bytes = record.bytes();
        if evidence.physical_ordinal() == 0 {
            if Sha256::digest(bytes).as_slice() != self.binding.header_digest {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            return Ok(());
        }
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return Ok(());
        };
        if value.get("type").and_then(Value::as_str) == Some("session") {
            return Err(CaptureError::InvalidPayload(
                "Pi source contains more than one session header".to_owned(),
            ));
        }
        let Some(occurred_at) = event_timestamp(&value) else {
            return Ok(());
        };
        let event_type = pi_event_type(
            value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            value.get("message"),
        );
        let Some(body) = projected_body(&value, event_type) else {
            return Ok(());
        };
        let touched_files = match touched_files(&value)? {
            Some(paths) => paths,
            None => return Ok(()),
        };
        let ordinal = evidence.physical_ordinal();
        let native_id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let native_item_key = match native_id {
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
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .map_err(contract)?;
        let locator = SourceRecordLocator::new(
            self.source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: evidence.byte_start(),
                byte_length: evidence
                    .byte_end_exclusive()
                    .checked_sub(evidence.byte_start())
                    .ok_or(CaptureError::SystemInvariant("Pi record range underflowed"))?,
                physical_ordinal: ordinal,
                native_session_key: Some(
                    TypedKey::utf8(&self.binding.native_session_id).map_err(contract)?,
                ),
                native_event_key: Some(
                    native_id
                        .map(TypedKey::utf8)
                        .transpose()
                        .map_err(contract)?
                        .unwrap_or(TypedKey::U64(ordinal)),
                ),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            None,
            Sha256::digest(bytes).into(),
        )
        .map_err(contract)?;
        let is_primary = self.binding.parent_session_id.is_none();
        emit(LexicalDocument {
            event_id,
            session_id: self.session_id,
            parent_session_id: self.parent_session_id,
            root_session_id: self.root_session_id,
            source: self.source.clone(),
            locator,
            provider_session_id: Some(self.binding.native_session_id.clone()),
            branch: None,
            source_path: Some(self.source_path.clone()),
            agent_type: if is_primary {
                AgentType::Primary
            } else {
                AgentType::Subagent
            }
            .as_str()
            .to_owned(),
            is_primary,
            event_sequence: ordinal,
            occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
            event_type: event_type.as_str().to_owned(),
            role: value
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(Value::as_str)
                .map(pi_event_role)
                .map(|role| role.as_str().to_owned()),
            body,
            workspace: None,
            cwd: self.binding.cwd.clone(),
            touched_files,
        })
    }
}

struct PiHydrator {
    source: SourceKey,
    binding: Binding,
    source_file: Arc<OpenedProviderSourceFile>,
}

impl JsonlFamilyHydrator for PiHydrator {
    fn hydrate(
        &mut self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        let (byte_offset, byte_length, ordinal, native_event_key) =
            validate_locator(request.locator(), &self.source, &self.binding)?;
        let length = usize::try_from(byte_length)
            .map_err(|_| invalid("Pi locator range exceeds platform limits"))?;
        if length == 0 || length > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2) {
            return Err(invalid("Pi locator range is invalid"));
        }
        if byte_offset > 0
            && self
                .source_file
                .read_exact_range(byte_offset - 1, 1, 1)
                .map_err(stale)?
                != b"\n"
        {
            return Err(stale("Pi record boundary changed"));
        }
        let wire = self
            .source_file
            .read_exact_range(
                byte_offset,
                length,
                MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2),
            )
            .map_err(stale)?;
        let bytes = strip_jsonl_terminator(&wire);
        if Sha256::digest(bytes).as_slice() != request.locator().record_digest() {
            return Err(stale("Pi record digest changed"));
        }
        let value: Value =
            serde_json::from_slice(bytes).map_err(|_| stale("Pi record JSON changed"))?;
        match (&native_event_key, value.get("id").and_then(Value::as_str)) {
            (TypedKey::Utf8(expected), Some(observed)) if expected == observed => {}
            (TypedKey::U64(expected), _) if *expected == ordinal => {}
            _ => return Err(stale("Pi record identity changed")),
        }
        let event_type = pi_event_type(
            value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            value.get("message"),
        );
        let body = projected_body(&value, event_type)
            .ok_or_else(|| stale("Pi record is no longer projected"))?;
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: body.into_bytes(),
        })
    }
}

fn parse_header_binding(record: JsonlRecordRef<'_>) -> Result<Binding> {
    #[cfg(test)]
    HEADER_PROBES.with(|count| count.set(count.get().saturating_add(1)));
    if record.evidence().physical_ordinal() != 0 {
        return Err(CaptureError::SystemInvariant(
            "Pi identity probe did not read the first record",
        ));
    }
    let value: Value = serde_json::from_slice(record.bytes())?;
    if value.get("type").and_then(Value::as_str) != Some("session") {
        return Err(CaptureError::InvalidPayload(
            "Pi source does not start with a session header".to_owned(),
        ));
    }
    let native_session_id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| {
            CaptureError::InvalidPayload("Pi session header is missing its identity".to_owned())
        })?
        .to_owned();
    if event_timestamp(&value).is_none() {
        return Err(CaptureError::InvalidPayload(
            "Pi session header has no valid timestamp".to_owned(),
        ));
    }
    Ok(Binding {
        native_session_id,
        parent_session_id: value
            .get("parentSession")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_owned),
        header_digest: Sha256::digest(record.bytes()).into(),
    })
}

fn projected_body(value: &Value, event_type: EventType) -> Option<String> {
    let is_output = matches!(event_type, EventType::ToolOutput | EventType::CommandOutput);
    if is_output {
        let outcome = result_outcome(value, event_type);
        if !matches!(outcome, ResultOutcome::Failure | ResultOutcome::Timeout)
            || event_type == EventType::CommandOutput && !command_output_is_supported(value)
        {
            return None;
        }
    }
    let text = if is_output {
        pi_result_content(value).or_else(|| pi_entry_text(value, value.get("message")))
    } else {
        pi_entry_text(value, value.get("message"))
    }
    .unwrap_or_default();
    Some(if text.trim().is_empty() {
        event_type.as_str().to_owned()
    } else {
        text
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResultOutcome {
    Success,
    Failure,
    Timeout,
    Unknown,
}

fn result_outcome(value: &Value, event_type: EventType) -> ResultOutcome {
    let message = value.get("message").unwrap_or(value);
    let timed_out = ["timedOut", "timed_out", "timeout"]
        .into_iter()
        .any(|key| message.get(key).and_then(Value::as_bool).unwrap_or(false))
        || message
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| {
                matches!(
                    status.trim().to_ascii_lowercase().as_str(),
                    "timeout" | "timed_out" | "timedout"
                )
            });
    if timed_out {
        return ResultOutcome::Timeout;
    }
    match crate::provider::normalization::provider_result_outcome_evidence(event_type, value)
        .as_str()
    {
        Some("success") => ResultOutcome::Success,
        Some("failure") => ResultOutcome::Failure,
        _ => ResultOutcome::Unknown,
    }
}

fn command_output_is_supported(value: &Value) -> bool {
    let Some(message) = value.get("message") else {
        return false;
    };
    message.get("role").and_then(Value::as_str) == Some("bashExecution")
        && message
            .get("command")
            .and_then(Value::as_str)
            .filter(|command| !command.is_empty() && command.len() <= 64 * 1024)
            .is_some_and(|command| !command.contains('\0'))
}

fn touched_files(value: &Value) -> Result<Option<Vec<String>>> {
    let mut paths = Vec::new();
    let outcome = visit_provider_file_touch_drafts_with_limit(
        value,
        false,
        MAX_TOUCHES_PER_RECORD,
        |(_, touch)| {
            paths.push(touch.path);
            Ok::<(), CaptureError>(())
        },
    )?;
    Ok((!outcome.limit_exceeded()).then_some(paths))
}

fn event_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn source_key(native_session_id: &str) -> Result<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)?;
    SourceKey::derive(
        CaptureProvider::Pi.as_str(),
        PI_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )
    .map_err(contract)
}

fn session_identity(source: &SourceKey, native_session_id: &str) -> Result<StableEntityId> {
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

fn session_identity_for_native(native_session_id: &str) -> Result<StableEntityId> {
    let source = source_key(native_session_id)?;
    session_identity(&source, native_session_id)
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
    if locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(invalid("Pi locator revision policy is invalid"));
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = locator.coordinate()
    else {
        return Err(invalid("Pi locator is not a JSONL range"));
    };
    if native_session_key.as_ref() != Some(&TypedKey::Utf8(binding.native_session_id.clone())) {
        return Err(invalid("Pi locator session key is invalid"));
    }
    let native_event_key = native_event_key
        .clone()
        .ok_or_else(|| invalid("Pi locator event key is absent"))?;
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
            "Pi family binding is malformed".to_owned(),
        ));
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn relative_to_authority(authority: &ProviderSourceRoot, path: &Path) -> Result<PathBuf> {
    path.strip_prefix(authority.named_path())
        .map(Path::to_path_buf)
        .map_err(|_| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Pi transcripts must remain below their selected authority",
        })
}

fn strip_jsonl_terminator(record: &[u8]) -> &[u8] {
    let record = record.strip_suffix(b"\n").unwrap_or(record);
    record.strip_suffix(b"\r").unwrap_or(record)
}

fn is_historical_omp_root(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    components.len() >= 3
        && components[components.len() - 3] == ".omp"
        && components[components.len() - 2] == "agent"
        && components[components.len() - 1] == "sessions"
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
