//! Pi adapter for the shared borrowed JSONL replacement family.

use std::{
    collections::{BTreeSet, HashMap},
    fs, io,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, EventIdentityInput,
    EventType, NativeItemKey, NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey,
    StableEntityId, TypedKey,
};
use ctx_history_index::BaseEventIdentityLookup;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        file_touches::visit_provider_file_touch_drafts_with_limit,
        providers::native_jsonl::visit_native_jsonl_files,
        source_backed::{
            family::jsonl::{
                observe_opened_file, probe_records_until, JsonlFamilyAdapter,
                JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
                JsonlFamilyProjectionMode, JsonlFamilyProjector, JsonlFileObservation,
                JsonlRecordRef,
            },
            FallbackEventIdentityState,
        },
    },
    CaptureError, Result,
};

use super::super::{
    pi_event_type,
    text::{pi_entry_text, pi_event_role, pi_result_content},
    PI_SOURCE_FORMAT,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "pi.session";
const NATIVE_SESSION_NAMESPACE: &str = "pi.session";
const NATIVE_EVENT_NAMESPACE: &str = "pi.entry";
const LOGICAL_SESSION_KIND: &str = "pi-session";
const LOGICAL_EVENT_KIND: &str = "pi-event";
const SOURCE_SCHEMA_VARIANT: &str = "pi-nativepath-jsonl-v1";
const PARSER_REVISION: &str = "pi-shared-jsonl-v2";
const EVENT_IDENTITY_REVISION: &str = "pi-content-occurrence-v1";
const FALLBACK_FINGERPRINT_DOMAIN: &[u8] = b"ctx.pi.fallback-event-fingerprint.v1\0";
const MAX_TOUCHES_PER_RECORD: usize = 63;
const MAX_HEADER_PROBE_RECORDS: usize = 64;

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
    identity_probe: crate::provider::source_backed::family::jsonl::JsonlProbe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    native_session_id: String,
    parent_session_id: Option<String>,
    cwd: Option<String>,
    header_digest: [u8; 32],
    leading_rejected_records: u64,
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

    fn event_identity_revision(&self) -> &'static str {
        EVENT_IDENTITY_REVISION
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
                Some(cached) if cached.observation == observation => {
                    (cached.binding.clone(), cached.identity_probe.clone())
                }
                _ => {
                    let mut leading_rejected_records = 0_u64;
                    let (binding, probe) = probe_records_until(
                        &path,
                        &opened,
                        MAX_HEADER_PROBE_RECORDS,
                        |record| -> Result<Option<Binding>> {
                            let Some(mut binding) = parse_header_binding(record)? else {
                                leading_rejected_records = leading_rejected_records
                                    .checked_add(1)
                                    .ok_or(CaptureError::SystemInvariant(
                                        "Pi identity rejection count overflowed",
                                    ))?;
                                return Ok(None);
                            };
                            binding.leading_rejected_records = leading_rejected_records;
                            Ok(Some(binding))
                        },
                    )?
                    .ok_or_else(|| {
                        CaptureError::InvalidPayload(
                            "Pi source has no valid session header within the bounded identity probe"
                                .to_owned(),
                        )
                    })?;
                    (binding, probe)
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
                            identity_probe,
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
                    identity_probe: identity_probe.clone(),
                },
            );
            let binding_key = TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?;
            leaves.push(JsonlFamilyLeaf::observe_after_identity_probe(
                source,
                path,
                Arc::clone(&authority),
                relative_path,
                binding_key,
                identity_probe,
                binding.leading_rejected_records,
            )?);
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
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<BaseEventIdentityLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        if checkpoint.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Pi adapter does not accept provider checkpoint state".to_owned(),
            ));
        }
        let binding = decode_binding(leaf)?;
        let session_id = session_identity(leaf.source(), &binding.native_session_id)?;
        let parent_session_id = binding
            .parent_session_id
            .as_deref()
            .map(session_identity_for_native)
            .transpose()?;
        Ok(Box::new(PiProjector {
            source: leaf.source().clone(),
            root_session_id: parent_session_id.unwrap_or(session_id),
            parent_session_id,
            session_id,
            binding,
            fallback_identities: FallbackEventIdentityState::new(
                leaf.source().clone(),
                session_id,
                LOGICAL_EVENT_KIND,
                "pi.entry.fallback",
                EVENT_IDENTITY_REVISION,
                mode.into(),
                base_event_lookup,
            )?,
            rejected_records: 0,
        }))
    }
}

struct PiProjector {
    source: SourceKey,
    binding: Binding,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    fallback_identities: FallbackEventIdentityState,
    rejected_records: u64,
}

impl JsonlFamilyProjector for PiProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let evidence = record.evidence();
        let bytes = record.bytes();
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            self.rejected_records =
                self.rejected_records
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Pi projection rejection count overflowed",
                    ))?;
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
        let (native_item_key, native_event_id) = match native_id {
            Some(id) => {
                let native_event_id = TypedKey::utf8(id).map_err(contract)?;
                (
                    NativeItemKey::native_id(NATIVE_EVENT_NAMESPACE, native_event_id.clone())
                        .map_err(contract)?,
                    native_event_id,
                )
            }
            None => {
                let assignment = self
                    .fallback_identities
                    .assign(fallback_fingerprint(bytes)?, None)?;
                (
                    assignment.native_item_key().clone(),
                    assignment.native_event_id().clone(),
                )
            }
        };
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .map_err(contract)?;
        let is_primary = self.binding.parent_session_id.is_none();
        let message = value.get("message").unwrap_or(&value);
        let tool_name = message
            .get("toolName")
            .or_else(|| message.get("tool_name"))
            .or_else(|| message.get("name"))
            .cloned();
        let call_id = message
            .get("toolCallId")
            .or_else(|| message.get("tool_call_id"))
            .or_else(|| message.get("callId"))
            .cloned();
        let structured_content =
            (!touched_files.is_empty() || tool_name.is_some() || call_id.is_some()).then(|| {
                serde_json::json!({
                    "tool_name": tool_name,
                    "call_id": call_id,
                    "file_touches": touched_files,
                })
            });
        let agent_type = if is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        };
        let mut core = CoreRecord::new_selected(
            event_id,
            self.session_id,
            self.root_session_id,
            self.source.clone(),
            ordinal,
            event_type.as_str(),
            agent_type.as_str(),
            is_primary,
            PARSER_REVISION,
            body,
        )
        .map_err(contract)?;
        core.parent_session_id = self.parent_session_id;
        core.provider_session_id = Some(self.binding.native_session_id.clone());
        core.native_event_id = Some(native_event_id);
        core.occurred_at_unix_ms = Some(occurred_at.timestamp_millis());
        core.role = value
            .get("message")
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            .map(pi_event_role)
            .map(|role| role.as_str().to_owned());
        core.cwd = self.binding.cwd.clone();
        core.content.structured_content = structured_content;
        core.validate_contract().map_err(contract)?;
        emit(core)
    }

    fn rejected_records(&self) -> u64 {
        self.rejected_records
    }

    fn finish(&mut self) -> Result<()> {
        self.fallback_identities.finish()
    }
}

fn fallback_fingerprint(bytes: &[u8]) -> Result<TypedKey> {
    let mut digest = Sha256::new();
    digest.update(FALLBACK_FINGERPRINT_DOMAIN);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    TypedKey::bytes(digest.finalize().to_vec()).map_err(contract)
}

fn parse_header_binding(record: JsonlRecordRef<'_>) -> Result<Option<Binding>> {
    let Ok(value) = serde_json::from_slice::<Value>(record.bytes()) else {
        return Ok(None);
    };
    if value.get("type").and_then(Value::as_str) != Some("session") {
        return Ok(None);
    }
    let Some(native_session_id) = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    if event_timestamp(&value).is_none() {
        return Ok(None);
    }
    Ok(Some(Binding {
        native_session_id,
        parent_session_id: value
            .get("parentSession")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_owned),
        header_digest: Sha256::digest(record.bytes()).into(),
        leading_rejected_records: 0,
    }))
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
