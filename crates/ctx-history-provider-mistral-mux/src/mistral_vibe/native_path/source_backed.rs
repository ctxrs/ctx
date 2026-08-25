use std::{
    collections::HashSet,
    fs,
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_capture_model::normalization::provider_explicit_result_value_text;
use ctx_history_capture_runtime::BaseEventLookup;
use ctx_history_core::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    derive_event_id, derive_native_session_id, ActivityInvocation, ActivityJsonCapture,
    ActivityResult, ActivityTextCapture, CaptureProvider, CoreActivity, CoreRecord,
    EventIdentityInput, EventType, LiteralFactKind, NativeItemKey, PositionStability,
    ProviderDeclaredFact, SourceAnchorScope, SourceKey, StableEntityId, SubrecordSelector,
    TypedKey, CORE_ACTIVITY_REVISION, MAX_CORE_CONTENT_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::*;
use ctx_history_jsonl::{
    fit_jsonl_activity, FallbackEventIdentityState, JsonlActivityObservedBytes, JsonlFamilyAdapter,
    JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf, JsonlFamilyProjectionMode,
    JsonlFamilyProjector, JsonlFamilyWorkerContext, JsonlOversizedRecordPolicy, JsonlRecordRef,
    JsonlRecordRejections, SourceBackedRecordRejectionDrafts,
};
use ctx_history_provider_runtime::{
    source_io::{OpenedProviderSourceFile, ProviderSourceRoot},
    CaptureError, ProviderBaseEventLookup, ProviderJsonlRuntime, ProviderRuntimeBinding, Result,
};
use ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;

mod activity;
use activity::mistral_vibe_activity;

const SOURCE_SCHEMA_VARIANT: &str = "meta-json-messages-jsonl-v1";
const SOURCE_ANCHOR_NAMESPACE: &str = "mistral-vibe-session-id";
const NATIVE_SESSION_NAMESPACE: &str = "mistral-vibe-session";
const NATIVE_EVENT_NAMESPACE: &str = "mistral-vibe-message";
const NATIVE_EVENT_REUSED_TOOL_CALL_POSITION_KIND: &str =
    "mistral-vibe-duplicate-tool-call-id-ordinal";
const LOGICAL_SESSION_KIND: &str = "mistral-vibe-session";
const LOGICAL_EVENT_KIND: &str = "mistral-vibe-event";
const PARSER_REVISION: &str = "mistral-vibe-source-backed-v17-exact-parent-admission";
const EVENT_IDENTITY_REVISION: &str = "mistral-vibe-content-occurrence-v1";
const FALLBACK_FINGERPRINT_DOMAIN: &[u8] = b"ctx.mistral-vibe.fallback-event-fingerprint.v1\0";
const SOURCE_REVISION_DIGEST_DOMAIN: &[u8] = b"ctx.mistral-vibe.source-revision.v1\0";
const MAX_NATIVE_ID_CANDIDATES: usize = 8_192;
const MAX_RETAINED_NATIVE_IDS: usize = 4_096;
const MAX_RETAINED_NATIVE_ID_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub(crate) struct MistralVibeJsonlAdapter<B> {
    source_anchor_scope: SourceAnchorScope,
    binding: PhantomData<fn() -> B>,
}

pub(crate) fn mistral_vibe_jsonl_adapter<B>(
) -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    mistral_vibe_jsonl_adapter_with_source_root_lineage(None)
}

pub(crate) fn mistral_vibe_jsonl_adapter_with_source_root_lineage<B>(
    source_root_lineage: Option<[u8; 32]>,
) -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    Arc::new(MistralVibeJsonlAdapter {
        source_anchor_scope: source_root_lineage
            .map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
        binding: PhantomData,
    })
}

#[derive(Debug)]
struct Draft {
    source: SourceKey,
    source_path: PathBuf,
    messages_relative_path: PathBuf,
    metadata_relative_path: PathBuf,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    lineage_ambiguous: bool,
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
    lineage_ambiguous: bool,
    started_at_unix_ms: i64,
    cwd: Option<String>,
    branch: Option<String>,
    revision_digest: [u8; 32],
}

impl<B> JsonlFamilyAdapter for MistralVibeJsonlAdapter<B>
where
    B: ProviderRuntimeBinding,
{
    type Runtime = ProviderJsonlRuntime<B>;

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

    fn event_identity_revision(&self) -> &'static str {
        EVENT_IDENTITY_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::Replacement
    }

    fn oversized_record_policy(&self) -> JsonlOversizedRecordPolicy {
        JsonlOversizedRecordPolicy::RejectRecord
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory<CaptureError>> {
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
            let source = source_key_scoped(&session.provider_session_id, self.source_anchor_scope)?;
            let session_id = session_identity(&source, &session.provider_session_id)?;
            let revision_digest = admitted.revision_digest(&source)?;
            drafts.push(Draft {
                source,
                source_path: authority.named_path().join(&messages_relative_path),
                messages_relative_path,
                metadata_relative_path,
                provider_session_id: session.provider_session_id,
                parent_provider_session_id: session.parent_provider_session_id,
                lineage_ambiguous: session.lineage_ambiguous,
                session_id,
                started_at_unix_ms: session.started_at.timestamp_millis(),
                cwd: session.cwd,
                branch: mistral_vibe_metadata_string(&session.metadata, "git_branch"),
                revision_digest,
            });
        }
        let mut leaves = Vec::with_capacity(drafts.len());
        for draft in &drafts {
            let parent_session_id = draft
                .parent_provider_session_id
                .as_deref()
                .map(|parent| provider_session_identity(parent, self.source_anchor_scope))
                .transpose()?;
            let binding = Binding {
                metadata_relative_path: draft.metadata_relative_path.clone(),
                provider_session_id: draft.provider_session_id.clone(),
                session_id: draft.session_id,
                parent_session_id,
                lineage_ambiguous: draft.lineage_ambiguous,
                started_at_unix_ms: draft.started_at_unix_ms,
                cwd: draft.cwd.clone(),
                branch: draft.branch.clone(),
                revision_digest: draft.revision_digest,
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
        leaf: &JsonlFamilyLeaf<CaptureError>,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = ProviderJsonlRuntime<B>>>> {
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
        leaf: &JsonlFamilyLeaf<CaptureError>,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<ProviderBaseEventLookup<B>>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = ProviderJsonlRuntime<B>>>> {
        if checkpoint.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Mistral Vibe adapter does not accept provider checkpoint state".to_owned(),
            ));
        }
        let binding = decode_binding(leaf)?;
        Ok(Box::new(MistralProjector::<B> {
            source: leaf.source().clone(),
            fallback_identities: FallbackEventIdentityState::new(
                leaf.source().clone(),
                binding.session_id,
                LOGICAL_EVENT_KIND,
                "mistral-vibe.message.fallback",
                EVENT_IDENTITY_REVISION,
                mode.into(),
                base_event_lookup,
            )?,
            native_identities: MistralNativeIdentityTracker::default(),
            binding,
            rejections: JsonlRecordRejections::new(
                leaf.source().clone(),
                CaptureProvider::MistralVibe,
                leaf.source_path().display().to_string(),
            ),
        }))
    }
}

struct MistralProjector<B: ProviderRuntimeBinding> {
    source: SourceKey,
    binding: Binding,
    fallback_identities: FallbackEventIdentityState<ProviderBaseEventLookup<B>, CaptureError>,
    native_identities: MistralNativeIdentityTracker,
    rejections: JsonlRecordRejections,
}

impl<B> JsonlFamilyProjector for MistralProjector<B>
where
    B: ProviderRuntimeBinding,
{
    type Runtime = ProviderJsonlRuntime<B>;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let bytes = record.bytes();
        if record.oversized() {
            self.rejections.malformed(
                record,
                format!(
                    "Mistral Vibe record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
                ),
            );
            return Ok(());
        }
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                self.rejections
                    .malformed(record, format!("malformed Mistral Vibe JSONL: {error}"));
                return Ok(());
            }
        };
        if let Some(document) = core_record_with_value(
            &self.source,
            &self.binding,
            &mut self.fallback_identities,
            &mut self.native_identities,
            record,
            value,
        )? {
            emit(document)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.fallback_identities.finish()
    }

    fn rejected_records(&self) -> u64 {
        self.rejections.count()
    }

    fn take_record_rejections(&mut self) -> SourceBackedRecordRejectionDrafts {
        self.rejections.take_drafts()
    }
}

fn core_record_with_value<L>(
    source: &SourceKey,
    binding: &Binding,
    fallback_identities: &mut FallbackEventIdentityState<L, CaptureError>,
    native_identities: &mut MistralNativeIdentityTracker,
    record: JsonlRecordRef<'_>,
    value: Value,
) -> Result<Option<CoreRecord>>
where
    L: BaseEventLookup,
{
    let bytes = record.bytes();
    let ordinal = record.evidence().physical_ordinal();
    let requires_collision_position = native_identities.requires_collision_position(&value);
    let Ok(role) = valid_mistral_vibe_record_role(&value) else {
        return Ok(None);
    };
    let event_type = mistral_vibe_event_type(role, &value);
    let output = event_type == EventType::ToolOutput;
    let body = if output {
        let Some(body) = mistral_vibe_output_text(&value)? else {
            return Ok(None);
        };
        body
    } else {
        mistral_vibe_event_text(role, &value, mistral_vibe_event_type(role, &value))
    };
    if body.is_empty() {
        return Ok(None);
    }
    let native_event_id = provider_native_event_id(&value);
    let (native_item_key, native_event_id) = match native_event_id {
        Some(id) => {
            let native_event_id = TypedKey::utf8(id).map_err(contract)?;
            (
                NativeItemKey::native_id(NATIVE_EVENT_NAMESPACE, native_event_id.clone())
                    .map_err(contract)?,
                native_event_id,
            )
        }
        None => {
            let assignment = fallback_identities.assign(fallback_fingerprint(bytes)?, None)?;
            (
                assignment.native_item_key().clone(),
                assignment.native_event_id().clone(),
            )
        }
    };
    let collision_selector = requires_collision_position
        .then(|| {
            SubrecordSelector::certified_position(
                NATIVE_EVENT_REUSED_TOOL_CALL_POSITION_KIND,
                TypedKey::U64(ordinal),
                PositionStability::AppendStable,
            )
            .map_err(contract)
        })
        .transpose()?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: binding.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: collision_selector.as_ref(),
    })
    .map_err(contract)?;
    let role = ctx_history_capture_model::normalization::provider_role(Some(role));
    let mut facts = Vec::new();
    if let Some(cwd) = &binding.cwd {
        if let Some(fact) =
            admit_provider_declared_fact(LiteralFactKind::SessionCwd, cwd.clone(), facts.len())
        {
            facts.push(fact);
        }
    }
    if let Some(branch) = &binding.branch {
        if let Some(fact) =
            admit_provider_declared_fact(LiteralFactKind::Branch, branch.clone(), facts.len())
        {
            facts.push(fact);
        }
    }
    collect_file_facts(&value, &mut facts);
    let activity = mistral_vibe_activity(&value, event_type, &body, facts);
    let mut record = CoreRecord::new_selected(
        event_id,
        binding.session_id,
        source.clone(),
        ordinal,
        event_type.as_str(),
        PARSER_REVISION,
        body.clone(),
    )
    .map_err(contract)?;
    if !binding.lineage_ambiguous {
        record.parent_session_id = binding.parent_session_id;
    }
    record.provider_session_id = Some(binding.provider_session_id.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(
        mistral_vibe_record_timestamp(&value)
            .map(|timestamp| timestamp.timestamp_millis())
            .unwrap_or(binding.started_at_unix_ms),
    );
    record.role = Some(role.as_str().to_owned());
    record.content.structured_content = Some(value);
    record.content.activity = activity;
    fit_jsonl_activity(
        &body,
        record.content.structured_content.as_ref(),
        &mut record.content.activity,
        JsonlActivityObservedBytes::infer_from_present(),
        MAX_CORE_CONTENT_BYTES,
    );
    record
        .content
        .omit_provider_declared_facts_if_aggregate_exceeds_limit()
        .map_err(contract)?;
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()
        .map_err(contract)?;
    record.validate_contract().map_err(contract)?;
    Ok(Some(record))
}

#[cfg(test)]
fn core_record<L>(
    source: &SourceKey,
    binding: &Binding,
    fallback_identities: &mut FallbackEventIdentityState<L, CaptureError>,
    native_identities: &mut MistralNativeIdentityTracker,
    record: JsonlRecordRef<'_>,
) -> Result<Option<CoreRecord>>
where
    L: BaseEventLookup,
{
    let value = serde_json::from_slice::<Value>(record.bytes())?;
    core_record_with_value(
        source,
        binding,
        fallback_identities,
        native_identities,
        record,
        value,
    )
}

fn mistral_vibe_record_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(ctx_history_capture_model::time::parse_rfc3339_utc)
        .or_else(|| {
            value
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(ctx_history_capture_model::time::parse_rfc3339_utc)
        })
        .or_else(|| {
            value
                .pointer("/time/created")
                .and_then(Value::as_i64)
                .and_then(DateTime::<Utc>::from_timestamp_millis)
        })
}

fn fallback_fingerprint(bytes: &[u8]) -> Result<TypedKey> {
    let mut digest = Sha256::new();
    digest.update(FALLBACK_FINGERPRINT_DOMAIN);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    TypedKey::bytes(digest.finalize().to_vec()).map_err(contract)
}

#[derive(Debug, Default)]
struct MistralNativeIdentityTracker {
    occurrences: HashSet<String>,
    candidate_count: usize,
    retained_bytes: usize,
    position_remaining_tool_results: bool,
}

impl MistralNativeIdentityTracker {
    fn requires_collision_position(&mut self, value: &Value) -> bool {
        let Some(call_id) = unmessaged_tool_result_call_id(value) else {
            return false;
        };
        if self.position_remaining_tool_results {
            return true;
        }

        let Some(candidate_count) = self.candidate_count.checked_add(1) else {
            self.fall_forward_to_positions();
            return true;
        };
        if candidate_count > MAX_NATIVE_ID_CANDIDATES {
            self.fall_forward_to_positions();
            return true;
        }
        self.candidate_count = candidate_count;

        // Keep the first provider call-ID identity unchanged. Only later
        // occurrences need an append-stable positional selector.
        if self.occurrences.contains(call_id) {
            return true;
        }

        let Some(retained_bytes) = self.retained_bytes.checked_add(call_id.len()) else {
            self.fall_forward_to_positions();
            return true;
        };
        if self.occurrences.len() >= MAX_RETAINED_NATIVE_IDS
            || retained_bytes > MAX_RETAINED_NATIVE_ID_BYTES
        {
            self.fall_forward_to_positions();
            return true;
        }
        self.occurrences.insert(call_id.to_owned());
        self.retained_bytes = retained_bytes;
        false
    }

    fn fall_forward_to_positions(&mut self) {
        self.position_remaining_tool_results = true;
        self.occurrences.clear();
    }
}

fn unmessaged_tool_result_call_id(value: &Value) -> Option<&str> {
    (value.get("message_id").is_none() && value.get("role").and_then(Value::as_str) == Some("tool"))
        .then(|| value.get("tool_call_id").and_then(Value::as_str))
        .flatten()
        .filter(|call_id| {
            !call_id.trim().is_empty() && call_id.len() <= super::super::MISTRAL_VIBE_MAX_ID_BYTES
        })
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

fn decode_binding(leaf: &JsonlFamilyLeaf<CaptureError>) -> Result<Binding> {
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

#[cfg(test)]
fn source_key(native_session_id: &str) -> Result<SourceKey> {
    source_key_scoped(native_session_id, SourceAnchorScope::Unqualified)
}

fn source_key_scoped(
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<SourceKey> {
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::MistralVibe.as_str(),
        MISTRAL_VIBE_SOURCE_FORMAT,
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

fn provider_session_identity(
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<StableEntityId> {
    let source = source_key_scoped(native_session_id, source_anchor_scope)?;
    session_identity(&source, native_session_id)
}

fn mistral_vibe_output_text(value: &Value) -> Result<Option<String>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    let candidates = ["content", "output", "result"]
        .into_iter()
        .filter_map(|field| object.get(field))
        .filter(|value| !value.is_null())
        .collect::<Vec<_>>();
    let selected = match candidates.as_slice() {
        [] => return Ok(None),
        [selected] => *selected,
        _ => {
            return Err(CaptureError::InvalidPayload(
                "Mistral Vibe tool result exposes more than one candidate body field".to_owned(),
            ));
        }
    };
    Ok(provider_explicit_result_value_text(selected).filter(|text| !text.trim().is_empty()))
}

fn provider_native_event_id(value: &Value) -> Option<String> {
    value
        .get("message_id")
        .or_else(|| value.get("tool_call_id"))
        .and_then(Value::as_str)
        .filter(|value| {
            !value.trim().is_empty() && value.len() <= super::super::MISTRAL_VIBE_MAX_ID_BYTES
        })
        .map(str::to_owned)
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_history_jsonl::FallbackEventIdentityMode;

    #[derive(Clone)]
    struct EmptyLookup;

    impl BaseEventLookup for EmptyLookup {
        type Error = std::convert::Infallible;

        fn contains(&self, _event_id: uuid::Uuid) -> std::result::Result<bool, Self::Error> {
            Ok(false)
        }
    }

    fn binding() -> (SourceKey, Binding) {
        let source = source_key("session").unwrap();
        let session_id = session_identity(&source, "session").unwrap();
        (
            source,
            Binding {
                metadata_relative_path: PathBuf::from("meta.json"),
                provider_session_id: "session".to_owned(),
                session_id,
                parent_session_id: None,
                lineage_ambiguous: false,
                started_at_unix_ms: 0,
                cwd: None,
                branch: None,
                revision_digest: [0; 32],
            },
        )
    }

    fn fallback_identities(
        source: &SourceKey,
        binding: &Binding,
    ) -> FallbackEventIdentityState<EmptyLookup, CaptureError> {
        FallbackEventIdentityState::new(
            source.clone(),
            binding.session_id,
            LOGICAL_EVENT_KIND,
            "mistral-vibe.message.fallback",
            EVENT_IDENTITY_REVISION,
            FallbackEventIdentityMode::Cold,
            None,
        )
        .unwrap()
    }

    #[test]
    fn native_parent_metadata_emits_only_the_direct_parent_claim() {
        let (source, root_binding) = binding();
        let project = |binding: &Binding| {
            let mut fallback_identities = fallback_identities(&source, binding);
            let mut native_identities = MistralNativeIdentityTracker::default();
            let bytes = br#"{"role":"user","content":"Mistral scope fixture"}"#;
            core_record(
                &source,
                binding,
                &mut fallback_identities,
                &mut native_identities,
                JsonlRecordRef::for_test(bytes, 0),
            )
            .unwrap()
            .unwrap()
        };

        let root = project(&root_binding);
        assert_eq!(root.agent_scope, None);
        assert_eq!(root.parent_session_id, None);
        assert_eq!(root.root_session_id, None);
        assert_eq!(root.session_relationship, None);

        let parent_session_id = session_identity(&source, "parent").unwrap();
        let child_binding = Binding {
            parent_session_id: Some(parent_session_id),
            ..root_binding
        };
        let child = project(&child_binding);
        assert_eq!(child.parent_session_id, Some(parent_session_id));
        assert_eq!(child.agent_scope, None);
        assert_eq!(child.root_session_id, None);
        assert_eq!(child.session_relationship, None);
    }

    #[test]
    fn tool_results_keep_native_statuses_statusless_activity_and_large_content() {
        let (source, binding) = binding();
        let mut fallback_identities = fallback_identities(&source, &binding);
        let mut native_identities = MistralNativeIdentityTracker::default();
        for (status, expected) in [
            (Some("success"), "success"),
            (Some("failure"), "failure"),
            (None, "unknown"),
        ] {
            let mut value = serde_json::json!({
                "role": "tool",
                "content": format!("complete-{expected}"),
                "tool_call_id": "call-1",
                "name": "write_file",
            });
            if let Some(status) = status {
                value["status"] = Value::String(status.to_owned());
            }
            let bytes = serde_json::to_vec(&value).unwrap();
            let record = core_record(
                &source,
                &binding,
                &mut fallback_identities,
                &mut native_identities,
                JsonlRecordRef::for_test(&bytes, 0),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                record.content.meaningful_text(),
                format!("complete-{expected}")
            );
            assert_eq!(
                record
                    .content
                    .structured_content
                    .as_ref()
                    .and_then(|value| value.get("tool_call_id"))
                    .and_then(Value::as_str),
                Some("call-1")
            );
            assert_eq!(
                record
                    .content
                    .activity
                    .as_ref()
                    .and_then(|activity| activity.result.as_ref())
                    .and_then(|result| result.status.as_deref()),
                None
            );
        }

        let large = format!("{}tail", "x".repeat(9 * 1024 * 1024));
        let bytes = serde_json::to_vec(&serde_json::json!({
            "role": "tool",
            "content": large,
            "tool_call_id": "large",
        }))
        .unwrap();
        let record = core_record(
            &source,
            &binding,
            &mut fallback_identities,
            &mut native_identities,
            JsonlRecordRef::for_test(&bytes, 1),
        )
        .unwrap()
        .unwrap();
        assert_eq!(record.content.meaningful_text().len(), 9 * 1024 * 1024 + 4);
        assert!(record.content.meaningful_text().ends_with("tail"));

        let bytes = serde_json::to_vec(&serde_json::json!({
            "role": "tool",
            "content": "one",
            "output": "two",
        }))
        .unwrap();
        assert!(core_record(
            &source,
            &binding,
            &mut fallback_identities,
            &mut native_identities,
            JsonlRecordRef::for_test(&bytes, 2),
        )
        .is_err());
    }

    #[test]
    fn composite_name_and_transport_metadata_always_abstain_without_changing_result_capture() {
        let (source, binding) = binding();
        let project = |transport: &str| {
            let mut fallback_identities = fallback_identities(&source, &binding);
            let mut native_identities = MistralNativeIdentityTracker::default();
            let bytes = serde_json::to_vec(&serde_json::json!({
                "role": "tool",
                "content": "terminal result",
                "name": "docs_server_read_document",
                "tool_call_id": "call-exact",
                "status": "success",
                "tool_result": {
                    "output": {
                        "ok": true,
                        "server": transport,
                        "tool": "read_document",
                    },
                    "cancelled": false,
                },
            }))
            .unwrap();
            core_record(
                &source,
                &binding,
                &mut fallback_identities,
                &mut native_identities,
                JsonlRecordRef::for_test(&bytes, 1),
            )
            .unwrap()
            .unwrap()
        };

        let url = project("https://mcp.example.test/mcp");
        let stdio = project("uvx mcp-server-filesystem /tmp");

        for record in [&url, &stdio] {
            assert_eq!(
                record
                    .content
                    .activity
                    .as_ref()
                    .and_then(|activity| activity.result.as_ref())
                    .and_then(|result| result.status.as_deref()),
                None
            );
            assert_eq!(record.content.meaningful_text(), "terminal result");
            assert_eq!(record.event_type, EventType::ToolOutput.as_str());
            assert_eq!(record.parser_revision, PARSER_REVISION);
            let linkage = record.content.structured_content.as_ref().unwrap();
            assert_eq!(
                linkage.get("tool_call_id").and_then(Value::as_str),
                Some("call-exact")
            );
            assert_eq!(
                linkage.get("name").and_then(Value::as_str),
                Some("docs_server_read_document")
            );
            assert_eq!(
                linkage.get("status").and_then(Value::as_str),
                Some("success")
            );
        }

        assert_eq!(url.event_id, stdio.event_id);
        assert_ne!(
            url.content.structured_content,
            stdio.content.structured_content
        );
    }
}
