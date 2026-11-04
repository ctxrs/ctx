//! Pi adapter for the shared borrowed JSONL replacement family.

use std::{
    collections::{BTreeSet, HashMap},
    marker::PhantomData,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::{provider::source_backed::IndexBaseEventLookup, JsonlProviderRuntime};
use chrono::{DateTime, Utc};
use ctx_history_capture_runtime::{
    SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDraft,
    SourceBackedRecordRejectionDrafts,
};
use ctx_history_core::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    derive_event_id, derive_native_session_id, CaptureProvider, CoreRecord, EventIdentityInput,
    EventType, LiteralFactKind, NativeItemKey, PositionStability, ProviderDeclaredFact,
    ProviderNativeSessionRelationship, SourceAnchorScope, SourceKey, StableEntityId,
    SubrecordSelector, TypedKey, MAX_CORE_CONTENT_BYTES,
};
use ctx_history_jsonl::JsonlRecordRejections;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use ctx_history_capture_model::file_references::visit_provider_file_reference_drafts_with_limit;

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::{
        family::jsonl::{
            JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
            JsonlFamilyProjectionMode, JsonlFamilyProjector, JsonlFamilyWorkerContext,
            JsonlFileObservation, JsonlMutableFirstRecord, JsonlRecordRef,
        },
        FallbackEventIdentityState,
    },
    CaptureError, Result,
};

use super::super::{
    pi_event_type,
    text::{pi_entry_text, pi_event_role, pi_result_content},
    PI_SOURCE_FORMAT,
};

mod activity;
mod discovery;
use activity::{pi_activity, split_tool_calls};

const SOURCE_ANCHOR_NAMESPACE: &str = "pi.session";
const NATIVE_SESSION_NAMESPACE: &str = "pi.session";
const NATIVE_EVENT_NAMESPACE: &str = "pi.entry";
const LOGICAL_SESSION_KIND: &str = "pi-session";
const LOGICAL_EVENT_KIND: &str = "pi-event";
const SOURCE_SCHEMA_VARIANT: &str = "pi-nativepath-jsonl-v1";
const PARSER_REVISION: &str = "pi-shared-jsonl-v13-nested-tool-parent-evidence";
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

pub(crate) fn pi_source_backed_adapter<R: JsonlProviderRuntime>(
) -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
    pi_source_backed_adapter_with_source_root_lineage(None)
}

pub(crate) fn pi_source_backed_adapter_with_source_root_lineage<R: JsonlProviderRuntime>(
    source_root_lineage: Option<[u8; 32]>,
) -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
    Arc::new(PiJsonlAdapter {
        bindings: Mutex::new(HashMap::new()),
        source_anchor_scope: source_root_lineage
            .map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
        runtime: PhantomData,
    })
}

struct PiJsonlAdapter<R> {
    bindings: Mutex<HashMap<PathBuf, CachedBinding>>,
    source_anchor_scope: SourceAnchorScope,
    runtime: PhantomData<fn() -> R>,
}

impl<R> Default for PiJsonlAdapter<R> {
    fn default() -> Self {
        Self {
            bindings: Mutex::new(HashMap::new()),
            source_anchor_scope: SourceAnchorScope::Unqualified,
            runtime: PhantomData,
        }
    }
}

#[derive(Clone)]
struct CachedBinding {
    observation: JsonlFileObservation,
    source: SourceKey,
    binding: Binding,
    binding_key: TypedKey,
    identity_probe: crate::provider::source_backed::family::jsonl::JsonlProbe,
    leading_rejections: SourceBackedRecordRejectionDrafts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    native_session_id: String,
    cwd: Option<String>,
    parent_session_id: Option<StableEntityId>,
    parent_session_claim_present: bool,
    header_digest: [u8; 32],
    leading_rejected_records: u64,
}

#[derive(Deserialize)]
struct ParentSessionHeader {
    #[serde(rename = "parentSession")]
    parent_session: Option<String>,
}

impl<R: JsonlProviderRuntime> JsonlFamilyAdapter for PiJsonlAdapter<R> {
    type Runtime = R;

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
        discovery::discover(self, root)
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
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<IndexBaseEventLookup<R>>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = R>>> {
        if checkpoint.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Pi adapter does not accept provider checkpoint state".to_owned(),
            ));
        }
        let binding = decode_binding(leaf)?;
        let leading_rejections = if binding.leading_rejected_records == 0 {
            SourceBackedRecordRejectionDrafts::default()
        } else {
            self.bindings
                .lock()
                .map_err(|_| contract("Pi binding catalog lock was poisoned"))?
                .get(leaf.source_path())
                .ok_or(CaptureError::SystemInvariant(
                    "Pi leading rejection diagnostics are unavailable",
                ))?
                .leading_rejections
                .clone()
        };
        ensure_rejection_count(
            &leading_rejections,
            binding.leading_rejected_records,
            "Pi leading rejection diagnostics disagree with their count",
        )?;
        let session_id = session_identity(leaf.source(), &binding.native_session_id)?;
        Ok(Box::new(PiProjector::<R> {
            source: leaf.source().clone(),
            session_id,
            binding,
            fallback_identities: FallbackEventIdentityState::<R>::new(
                leaf.source().clone(),
                session_id,
                LOGICAL_EVENT_KIND,
                "pi.entry.fallback",
                EVENT_IDENTITY_REVISION,
                mode.into(),
                base_event_lookup,
            )?,
            leading_rejections,
            rejections: JsonlRecordRejections::new(
                leaf.source().clone(),
                CaptureProvider::Pi,
                leaf.source_path().display().to_string(),
            ),
        }))
    }
}

struct PiProjector<R: JsonlProviderRuntime> {
    source: SourceKey,
    binding: Binding,
    session_id: StableEntityId,
    fallback_identities: FallbackEventIdentityState<R>,
    leading_rejections: SourceBackedRecordRejectionDrafts,
    rejections: JsonlRecordRejections,
}

impl<R: JsonlProviderRuntime> JsonlFamilyProjector for PiProjector<R> {
    type Runtime = R;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<R>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let Some(value) = parse_pi_event_record(&mut self.rejections, record) else {
            return Ok(());
        };
        let evidence = record.evidence();
        let bytes = record.bytes();
        if value.get("type").and_then(Value::as_str) == Some("session") {
            return Err(CaptureError::InvalidPayload(
                "Pi source contains more than one session header".to_owned(),
            ));
        }
        let Some(occurred_at) = event_timestamp(&value) else {
            return Ok(());
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
        for (part, value) in split_tool_calls(value)? {
            let event_type = pi_event_type(
                value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                value.get("message"),
            );
            let body = projected_body(&value, event_type);
            let mut facts = literal_facts(&value)?;
            let selector = SubrecordSelector::certified_position(
                "pi.tool-call",
                TypedKey::U64(u64::from(part)),
                PositionStability::StableSlot,
            )
            .map_err(contract)?;
            let event_id = derive_event_id(EventIdentityInput {
                source: &self.source,
                session_id: self.session_id,
                logical_item_kind: LOGICAL_EVENT_KIND,
                native_item_key: &native_item_key,
                subrecord_selector: (part != 0).then_some(&selector),
            })
            .map_err(contract)?;
            let message = value.get("message").unwrap_or(&value);
            if let Some(cwd) = &self.binding.cwd {
                if let Some(fact) = admit_provider_declared_fact(
                    LiteralFactKind::SessionCwd,
                    cwd.clone(),
                    facts.len(),
                ) {
                    facts.insert(0, fact);
                }
            }
            let activity = pi_activity(message, event_type, facts)?;
            let sequence = ordinal
                .checked_mul(u64::from(u32::MAX) + 1)
                .and_then(|base| base.checked_add(u64::from(part)))
                .ok_or_else(|| contract("Pi event sequence overflow"))?;
            let mut core = CoreRecord::new_selected(
                event_id,
                self.session_id,
                self.source.clone(),
                sequence,
                event_type.as_str(),
                PARSER_REVISION,
                body.clone(),
            )
            .map_err(contract)?;
            core.provider_session_id = Some(self.binding.native_session_id.clone());
            if let Some(parent_session_id) = self.binding.parent_session_id {
                core.parent_session_id = Some(parent_session_id);
                core.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
            }
            core.native_event_id = Some(native_event_id.clone());
            core.occurred_at_unix_ms = Some(occurred_at.timestamp_millis());
            core.role = value
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(Value::as_str)
                .map(pi_event_role)
                .map(|role| role.as_str().to_owned());
            // Preserve the header claim even if its parent ID could not resolve.
            core.content.structured_content = Some(serde_json::json!({
                "entry": value,
                "session_parent_claim_present": self.binding.parent_session_claim_present,
            }));
            core.content.activity = activity;
            ctx_history_jsonl::fit_jsonl_activity(
                &body,
                core.content.structured_content.as_ref(),
                &mut core.content.activity,
                ctx_history_jsonl::JsonlActivityObservedBytes::infer_from_present(),
                MAX_CORE_CONTENT_BYTES,
            );
            core.content
                .omit_provider_declared_facts_if_aggregate_exceeds_limit()
                .map_err(contract)?;
            core.content
                .omit_structured_content_if_aggregate_exceeds_limit()
                .map_err(contract)?;
            core.validate_contract().map_err(contract)?;
            emit(core)?;
        }
        Ok(())
    }

    fn rejected_records(&self) -> u64 {
        self.rejections.count()
    }

    fn take_record_rejections(&mut self) -> SourceBackedRecordRejectionDrafts {
        let mut rejections = std::mem::take(&mut self.leading_rejections);
        rejections.merge(self.rejections.take_drafts());
        rejections
    }

    fn finish(&mut self) -> Result<()> {
        self.fallback_identities.finish()
    }
}

fn parse_pi_event_record(
    rejections: &mut JsonlRecordRejections,
    record: JsonlRecordRef<'_>,
) -> Option<Value> {
    if record.bytes().iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    let value = match serde_json::from_slice::<Value>(record.bytes()) {
        Ok(value) => value,
        Err(error) => {
            rejections.malformed(record, format!("malformed Pi JSONL: {error}"));
            return None;
        }
    };
    if value.get("type").and_then(Value::as_str) == Some("title") {
        rejections.malformed(record, "Pi title record appears after the session header");
        return None;
    }
    Some(value)
}

fn leading_rejection_detail(record: JsonlRecordRef<'_>) -> String {
    match serde_json::from_slice::<Value>(record.bytes()) {
        Err(error) => format!("malformed Pi JSONL before the session header: {error}"),
        Ok(value) if value.get("type").and_then(Value::as_str) == Some("session") => {
            "Pi session header is malformed".to_owned()
        }
        Ok(_) => "Pi record appears before the required session header".to_owned(),
    }
}

fn pi_leading_rejection_drafts(
    source: &SourceKey,
    path: &Path,
    details: Vec<(u64, String)>,
) -> Result<SourceBackedRecordRejectionDrafts> {
    let mut rejections = SourceBackedRecordRejectionDrafts::default();
    let source_selector = path.display().to_string();
    for (line_number, detail) in details {
        rejections.record(SourceBackedRecordRejectionDraft {
            source: source.clone(),
            provider: CaptureProvider::Pi,
            source_selector: source_selector.clone(),
            line_number,
            payload_type: None,
            class: SourceBackedRecordRejectionClass::MalformedRecord,
            detail,
        });
    }
    Ok(rejections)
}

fn ensure_rejection_count(
    rejections: &SourceBackedRecordRejectionDrafts,
    expected: u64,
    mismatch: &'static str,
) -> Result<()> {
    let (recorded, omitted) = rejections.clone().into_parts();
    let observed = u64::try_from(recorded.len().saturating_add(omitted))
        .map_err(|_| CaptureError::SystemInvariant(mismatch))?;
    if observed != expected {
        return Err(CaptureError::SystemInvariant(mismatch));
    }
    Ok(())
}

fn fallback_fingerprint(bytes: &[u8]) -> Result<TypedKey> {
    let mut digest = Sha256::new();
    digest.update(FALLBACK_FINGERPRINT_DOMAIN);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    TypedKey::bytes(digest.finalize().to_vec()).map_err(contract)
}

fn parse_header_binding(
    record: JsonlRecordRef<'_>,
    source_anchor_scope: SourceAnchorScope,
) -> Result<Option<Binding>> {
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
    let parent_session_id = serde_json::from_slice::<ParentSessionHeader>(record.bytes())
        .ok()
        .and_then(|header| header.parent_session)
        .and_then(omp_parent_native_session_id)
        .filter(|parent| parent != &native_session_id)
        .and_then(|parent| session_identity_for_native(&parent, source_anchor_scope).ok());
    Ok(Some(Binding {
        native_session_id,
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_owned),
        parent_session_id,
        parent_session_claim_present: value.get("parentSession").is_some(),
        header_digest: Sha256::digest(record.bytes()).into(),
        leading_rejected_records: 0,
    }))
}

fn omp_parent_native_session_id(parent: String) -> Option<String> {
    let parent = admit_optional_metadata_text(Some(parent))?;
    if !looks_like_absolute_path(&parent) {
        return Some(parent);
    }
    omp_session_id_from_path(&parent).map(str::to_owned)
}

fn looks_like_absolute_path(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with(r"\\")
        || value.as_bytes().get(..3).is_some_and(|prefix| {
            prefix[0].is_ascii_alphabetic()
                && prefix[1] == b':'
                && matches!(prefix[2], b'/' | b'\\')
        })
}

fn omp_session_id_from_path(path: &str) -> Option<&str> {
    let stem = path.rsplit(['/', '\\']).next()?.strip_suffix(".jsonl")?;
    let (timestamp, session_id) = stem.split_once('_')?;
    (is_omp_filename_timestamp(timestamp) && !session_id.is_empty()).then_some(session_id)
}

fn is_omp_filename_timestamp(timestamp: &str) -> bool {
    let bytes = timestamp.as_bytes();
    bytes.len() == 24
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 | 13 | 16 | 19 => *byte == b'-',
            10 => *byte == b'T',
            23 => *byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

fn is_omp_title_slot(record: JsonlRecordRef<'_>) -> bool {
    record.evidence().physical_ordinal() == 0
        && JsonlMutableFirstRecord::OmpTitleSlotV1.validates_record(record.bytes())
}

fn projected_body(value: &Value, event_type: EventType) -> String {
    let is_output = matches!(event_type, EventType::ToolOutput | EventType::CommandOutput);
    let text = if is_output {
        pi_result_content(value).or_else(|| pi_entry_text(value, value.get("message")))
    } else {
        pi_entry_text(value, value.get("message"))
    }
    .unwrap_or_default();
    if text.trim().is_empty() {
        event_type.as_str().to_owned()
    } else {
        text
    }
}

fn literal_facts(value: &Value) -> Result<Vec<ProviderDeclaredFact>> {
    let mut facts = Vec::new();
    let outcome = visit_provider_file_reference_drafts_with_limit(
        value,
        MAX_TOUCHES_PER_RECORD,
        |(_, reference)| {
            if let Some(fact) =
                admit_provider_declared_fact(reference.kind, reference.value, facts.len())
            {
                facts.push(fact);
            }
            Ok::<(), CaptureError>(())
        },
    )?;
    Ok(if outcome.limit_exceeded() {
        Vec::new()
    } else {
        facts
    })
}

fn event_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

#[cfg(test)]
fn source_key(native_session_id: &str) -> Result<SourceKey> {
    source_key_scoped(native_session_id, SourceAnchorScope::Unqualified)
}

fn source_key_scoped(
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<SourceKey> {
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::Pi.as_str(),
        PI_SOURCE_FORMAT,
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

fn session_identity_for_native(
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<StableEntityId> {
    let source = source_key_scoped(native_session_id, source_anchor_scope)?;
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

#[cfg(test)]
mod rejection_tests;

#[cfg(test)]
mod tests;
