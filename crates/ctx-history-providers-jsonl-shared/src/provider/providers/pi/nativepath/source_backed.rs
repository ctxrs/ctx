//! Pi adapter for the shared borrowed JSONL replacement family.

use std::{
    collections::{BTreeSet, HashMap},
    fs, io,
    marker::PhantomData,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::{provider::source_backed::IndexBaseEventLookup, JsonlProviderRuntime};
use chrono::{DateTime, Utc};
use ctx_history_core::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    derive_event_id, derive_native_session_id, ActivityInvocation, ActivityJsonCapture,
    ActivityResult, ActivityTextCapture, CaptureProvider, CoreActivity, CoreRecord,
    EventIdentityInput, EventType, LiteralFactKind, NativeItemKey, ProviderDeclaredFact,
    ProviderNativeSessionRelationship, SourceAnchorScope, SourceKey, StableEntityId, TypedKey,
    CORE_ACTIVITY_REVISION, MAX_CORE_CONTENT_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use ctx_history_capture_model::file_references::visit_provider_file_reference_drafts_with_limit;

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        providers::native_jsonl::visit_native_jsonl_files,
        source_backed::{
            family::jsonl::{
                observe_opened_file, probe_records_until, JsonlFamilyAdapter,
                JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
                JsonlFamilyProjectionMode, JsonlFamilyProjector, JsonlFamilyWorkerContext,
                JsonlFileObservation, JsonlRecordRef,
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
const PARSER_REVISION: &str = "pi-shared-jsonl-v11-omp-parent-lineage";
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
    binding: Binding,
    identity_probe: crate::provider::source_backed::family::jsonl::JsonlProbe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    native_session_id: String,
    cwd: Option<String>,
    #[serde(skip)]
    parent_session: Option<String>,
    resolved_parent_session_id: Option<StableEntityId>,
    header_digest: [u8; 32],
    leading_rejected_records: u64,
}

#[derive(Deserialize)]
struct ParentSessionHeader {
    #[serde(rename = "parentSession")]
    parent_session: Option<String>,
}

struct DiscoveredPiSource {
    path: PathBuf,
    relative_path: PathBuf,
    observation: JsonlFileObservation,
    binding: Binding,
    identity_probe: crate::provider::source_backed::family::jsonl::JsonlProbe,
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
        let mut discovered = Vec::with_capacity(paths.len());
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
                                if !is_omp_title_slot(record) {
                                    leading_rejected_records = leading_rejected_records
                                        .checked_add(1)
                                        .ok_or(CaptureError::SystemInvariant(
                                            "Pi identity rejection count overflowed",
                                        ))?;
                                }
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
            discovered.push(DiscoveredPiSource {
                path,
                relative_path,
                observation,
                binding,
                identity_probe,
            });
        }
        resolve_parent_sessions(&mut discovered, self.source_anchor_scope)?;
        let mut sources = HashMap::<[u8; 32], JsonlFileObservation>::new();
        let mut leaves = Vec::with_capacity(discovered.len());
        for discovered in discovered {
            let DiscoveredPiSource {
                path,
                relative_path,
                observation,
                binding,
                identity_probe,
            } = discovered;
            let source = source_key_scoped(&binding.native_session_id, self.source_anchor_scope)?;
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
            rejected_records: 0,
        }))
    }
}

struct PiProjector<R: JsonlProviderRuntime> {
    source: SourceKey,
    binding: Binding,
    session_id: StableEntityId,
    fallback_identities: FallbackEventIdentityState<R>,
    rejected_records: u64,
}

impl<R: JsonlProviderRuntime> JsonlFamilyProjector for PiProjector<R> {
    type Runtime = R;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<R>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let evidence = record.evidence();
        let bytes = record.bytes();
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            self.reject_record()?;
            return Ok(());
        };
        if value.get("type").and_then(Value::as_str) == Some("title") {
            self.reject_record()?;
            return Ok(());
        }
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
        let body = projected_body(&value, event_type);
        let mut facts = literal_facts(&value)?;
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
        let message = value.get("message").unwrap_or(&value);
        if let Some(cwd) = &self.binding.cwd {
            if let Some(fact) =
                admit_provider_declared_fact(LiteralFactKind::SessionCwd, cwd.clone(), facts.len())
            {
                facts.insert(0, fact);
            }
        }
        let activity = pi_activity(message, event_type, &body, facts)?;
        let mut core = CoreRecord::new_selected(
            event_id,
            self.session_id,
            self.source.clone(),
            ordinal,
            event_type.as_str(),
            PARSER_REVISION,
            body.clone(),
        )
        .map_err(contract)?;
        core.provider_session_id = Some(self.binding.native_session_id.clone());
        if let Some(parent_session_id) = self.binding.resolved_parent_session_id {
            core.parent_session_id = Some(parent_session_id);
            core.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
        }
        core.native_event_id = Some(native_event_id);
        core.occurred_at_unix_ms = Some(occurred_at.timestamp_millis());
        core.role = value
            .get("message")
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            .map(pi_event_role)
            .map(|role| role.as_str().to_owned());
        core.content.structured_content = Some(value);
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
        emit(core)
    }

    fn rejected_records(&self) -> u64 {
        self.rejected_records
    }

    fn finish(&mut self) -> Result<()> {
        self.fallback_identities.finish()
    }
}

impl<R: JsonlProviderRuntime> PiProjector<R> {
    fn reject_record(&mut self) -> Result<()> {
        self.rejected_records =
            self.rejected_records
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Pi projection rejection count overflowed",
                ))?;
        Ok(())
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
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_owned),
        parent_session: serde_json::from_slice::<ParentSessionHeader>(record.bytes())
            .ok()
            .and_then(|header| header.parent_session)
            .filter(|parent| !parent.trim().is_empty())
            .map(|parent| parent.trim().to_owned()),
        resolved_parent_session_id: None,
        header_digest: Sha256::digest(record.bytes()).into(),
        leading_rejected_records: 0,
    }))
}

fn resolve_parent_sessions(
    discovered: &mut [DiscoveredPiSource],
    source_anchor_scope: SourceAnchorScope,
) -> Result<()> {
    let paths = discovered
        .iter()
        .enumerate()
        .filter_map(|(index, source)| omp_path_identity(&source.path).map(|path| (path, index)))
        .collect::<HashMap<_, _>>();
    let mut native_ids = HashMap::<&str, Option<usize>>::new();
    for (index, source) in discovered.iter().enumerate() {
        native_ids
            .entry(&source.binding.native_session_id)
            .and_modify(|candidate| *candidate = None)
            .or_insert(Some(index));
    }
    let mut resolved = discovered
        .iter()
        .enumerate()
        .map(|(child_index, source)| {
            let parent = source.binding.parent_session.as_deref()?;
            let parent_index = if Path::new(parent).is_absolute() {
                paths
                    .get(&omp_path_identity(Path::new(parent))?)
                    .copied()
                    .filter(|index| {
                        omp_session_path_matches_id(
                            parent,
                            &discovered[*index].binding.native_session_id,
                        )
                    })
            } else {
                native_ids.get(parent).copied().flatten()
            }?;
            (parent_index != child_index).then_some(parent_index)
        })
        .collect::<Vec<_>>();
    clear_cyclic_parent_references(&mut resolved);
    let parent_session_ids = resolved
        .into_iter()
        .map(|parent_index| {
            parent_index
                .map(|parent_index| {
                    let parent = &discovered[parent_index].binding.native_session_id;
                    session_identity_for_native(parent, source_anchor_scope)
                })
                .transpose()
        })
        .collect::<Result<Vec<_>>>()?;
    for (child, parent_session_id) in discovered.iter_mut().zip(parent_session_ids) {
        child.binding.resolved_parent_session_id = parent_session_id;
    }
    Ok(())
}

fn omp_session_path_matches_id(parent: &str, native_session_id: &str) -> bool {
    let Some(file_name) = Path::new(parent).file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    file_name
        .strip_suffix(".jsonl")
        .and_then(|stem| stem.strip_suffix(native_session_id))
        .is_some_and(|prefix| prefix.ends_with('_'))
}

fn omp_path_identity(path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use std::{
            ffi::OsString,
            os::windows::ffi::{OsStrExt as _, OsStringExt as _},
            path::{Component, Prefix},
        };

        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return None;
        }
        let Component::Prefix(prefix) = path.components().next()? else {
            return None;
        };
        let verbatim = match prefix.kind() {
            Prefix::Disk(_) => false,
            Prefix::VerbatimDisk(_) => true,
            Prefix::UNC(_, _)
            | Prefix::VerbatimUNC(_, _)
            | Prefix::DeviceNS(_)
            | Prefix::Verbatim(_) => return None,
        };
        let identity = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if !verbatim {
            return Some(PathBuf::from(OsString::from_wide(&identity)));
        }
        const VERBATIM_NAMESPACE: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
        identity
            .strip_prefix(VERBATIM_NAMESPACE)
            .map(OsString::from_wide)
            .map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        Some(path.to_path_buf())
    }
}

fn clear_cyclic_parent_references(parents: &mut [Option<usize>]) {
    let mut state = vec![0_u8; parents.len()];
    let mut positions = vec![None; parents.len()];
    for start in 0..parents.len() {
        if state[start] != 0 {
            continue;
        }
        let mut path = Vec::new();
        let mut current = start;
        loop {
            match state[current] {
                0 => {
                    state[current] = 1;
                    positions[current] = Some(path.len());
                    path.push(current);
                    let Some(parent) = parents[current] else {
                        break;
                    };
                    current = parent;
                }
                1 => {
                    let cycle_start =
                        positions[current].expect("active parent traversal must have a position");
                    for node in &path[cycle_start..] {
                        parents[*node] = None;
                    }
                    break;
                }
                _ => break,
            }
        }
        for node in path {
            state[node] = 2;
            positions[node] = None;
        }
    }
}

fn is_omp_title_slot(record: JsonlRecordRef<'_>) -> bool {
    if record.evidence().physical_ordinal() != 0 {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<Value>(record.bytes()) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("title")
        && value.get("v").and_then(Value::as_f64) == Some(1.0)
        && value.get("title").is_some_and(Value::is_string)
        && value.get("updatedAt").is_some_and(Value::is_string)
        && value.get("pad").is_some_and(Value::is_string)
        && match value.get("source") {
            None => true,
            Some(source) => matches!(source.as_str(), Some("auto" | "user")),
        }
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

fn pi_activity(
    message: &Value,
    event_type: EventType,
    body: &str,
    facts: Vec<ProviderDeclaredFact>,
) -> Result<Option<CoreActivity>> {
    let call_ids = ["toolCallId", "tool_call_id", "callId"]
        .into_iter()
        .filter_map(|field| message.get(field).and_then(Value::as_str))
        .collect::<Vec<_>>();
    let provider_call_id = match call_ids.as_slice() {
        [id] => admit_optional_provider_call_id(Some((*id).to_owned())),
        _ => None,
    };
    let tools = ["toolName", "tool_name", "name"]
        .into_iter()
        .filter_map(|field| message.get(field).and_then(Value::as_str))
        .collect::<Vec<_>>();
    let invocation = if provider_call_id.is_some() && event_type == EventType::ToolCall {
        match tools.as_slice() {
            [tool] => admit_optional_metadata_text(Some((*tool).to_owned())).map(|tool| {
                ActivityInvocation {
                    protocol: None,
                    server: None,
                    tool,
                    arguments: message.get("arguments").map_or(
                        ActivityJsonCapture::Absent,
                        |value| ActivityJsonCapture::Present {
                            value: value.clone(),
                        },
                    ),
                    started_at_unix_ms: None,
                }
            }),
            _ => None,
        }
    } else {
        None
    };
    let result = (provider_call_id.is_some()
        && matches!(event_type, EventType::ToolOutput | EventType::CommandOutput))
    .then(|| ActivityResult {
        status: None,
        completed_at_unix_ms: None,
        duration_ns: None,
        text: ActivityTextCapture::Present {
            value: body.to_owned(),
        },
        structured_content: ActivityJsonCapture::Present {
            value: message.clone(),
        },
    });
    if invocation.is_none() && result.is_none() && facts.is_empty() {
        return Ok(None);
    }
    Ok(Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id,
        invocation,
        result,
        facts,
    }))
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
mod tests {
    use super::*;

    #[test]
    fn source_and_related_session_identities_are_root_scoped() {
        let released = source_key("same-session").unwrap();
        let compatibility =
            source_key_scoped("same-session", SourceAnchorScope::Unqualified).unwrap();
        let first = source_key_scoped("same-session", SourceAnchorScope::Lineage([1; 32])).unwrap();
        let second =
            source_key_scoped("same-session", SourceAnchorScope::Lineage([2; 32])).unwrap();

        assert!(released.exact_descriptor_eq(&compatibility));
        assert_ne!(first.identity(), second.identity());
        assert_ne!(
            session_identity_for_native("parent", SourceAnchorScope::Lineage([1; 32])).unwrap(),
            session_identity_for_native("parent", SourceAnchorScope::Lineage([2; 32])).unwrap()
        );
    }

    #[test]
    fn omp_title_slot_is_ignored_only_at_the_first_physical_record() {
        let title = br#"{"type":"title","v":1,"title":"fixture","updatedAt":"2026-08-20T15:12:20.989Z","pad":""}"#;

        assert!(is_omp_title_slot(JsonlRecordRef::for_test(title, 0)));
        assert!(!is_omp_title_slot(JsonlRecordRef::for_test(title, 1)));
    }

    #[test]
    fn malformed_omp_title_slots_remain_rejected() {
        for title in [
            br#"{"type":"title","v":2,"title":"fixture","updatedAt":"2026-08-20T15:12:20.989Z","pad":""}"#.as_slice(),
            br#"{"type":"title","v":1,"title":"fixture","updatedAt":"2026-08-20T15:12:20.989Z"}"#.as_slice(),
            br#"{"type":"title","v":1,"title":"fixture","updatedAt":"2026-08-20T15:12:20.989Z","pad":"","source":"other"}"#.as_slice(),
        ] {
            assert!(!is_omp_title_slot(JsonlRecordRef::for_test(title, 0)));
        }
    }

    #[test]
    fn omp_path_parent_requires_the_native_session_id_filename_suffix() {
        assert!(omp_session_path_matches_id(
            "/tmp/2026-09-01T00-00-00-000Z_parent.jsonl",
            "parent"
        ));
        assert!(!omp_session_path_matches_id(
            "/tmp/2026-09-01T00-00-00-000Z_other.jsonl",
            "parent"
        ));
        assert!(!omp_session_path_matches_id("/tmp/parent.jsonl", "parent"));
        assert!(!omp_session_path_matches_id(
            "/tmp/2026-09-01T00-00-00-000Z_parent.jsonl.bak",
            "parent"
        ));
    }

    #[cfg(windows)]
    #[test]
    fn omp_path_identity_equates_only_ordinary_and_verbatim_disk_paths() {
        let ordinary = Path::new(r"C:\Users\ctx\.omp\agent\sessions\session.jsonl");
        let verbatim = Path::new(r"\\?\C:\Users\ctx\.omp\agent\sessions\session.jsonl");

        assert_eq!(
            omp_path_identity(ordinary),
            omp_path_identity(verbatim),
            "Windows canonical paths use verbatim spelling while OMP records ordinary paths"
        );
        assert!(omp_path_identity(Path::new(r"\\server\share\session.jsonl")).is_none());
        assert!(omp_path_identity(Path::new(r"C:\tmp\..\session.jsonl")).is_none());
    }

    #[test]
    fn unlinked_output_withholds_result_but_preserves_literal_facts() {
        let message = serde_json::json!({
            "type": "bashExecution",
            "output": "future output",
        });
        let facts = vec![ProviderDeclaredFact {
            kind: LiteralFactKind::Command,
            value: "printf future".to_owned(),
        }];

        let activity = pi_activity(
            &message,
            EventType::CommandOutput,
            "future output",
            facts.clone(),
        )
        .unwrap()
        .unwrap();

        assert!(activity.provider_call_id.is_none());
        assert!(activity.invocation.is_none());
        assert!(activity.result.is_none());
        assert_eq!(activity.facts, facts);
    }

    #[test]
    fn exact_call_id_retains_linked_output_result() {
        let message = serde_json::json!({
            "type": "toolResult",
            "toolCallId": "pi-call-1",
            "content": "provider output",
        });

        let activity = pi_activity(
            &message,
            EventType::ToolOutput,
            "provider output",
            Vec::new(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            activity.provider_call_id,
            Some(TypedKey::utf8("pi-call-1").unwrap())
        );
        assert!(activity.invocation.is_none());
        assert!(activity.result.is_some());
    }

    #[test]
    fn unadmitted_optional_linkage_does_not_emit_empty_activity() {
        let oversized = "x".repeat(64 * 1024 + 1);
        let output = serde_json::json!({"toolCallId": oversized});
        assert_eq!(
            pi_activity(&output, EventType::ToolOutput, "output", Vec::new()).unwrap(),
            None
        );

        let call = serde_json::json!({
            "toolCallId": "pi-call-1",
            "toolName": oversized,
        });
        assert_eq!(
            pi_activity(&call, EventType::ToolCall, "call", Vec::new()).unwrap(),
            None
        );
    }

    #[test]
    fn provider_output_is_projected_without_adjudicating_success() {
        let successful_tool = serde_json::json!({
            "type": "message",
            "message": {
                "role": "toolResult",
                "toolCallId": "pi-call-success",
                "content": "done",
                "isError": false,
            },
        });
        assert_eq!(
            projected_body(&successful_tool, EventType::ToolOutput),
            "done"
        );

        let successful_command = serde_json::json!({
            "type": "message",
            "message": {
                "role": "bashExecution",
                "command": "true",
                "output": "done",
                "exitCode": 0,
                "cancelled": false,
            },
        });
        assert_eq!(
            projected_body(&successful_command, EventType::CommandOutput),
            "done"
        );

        let failed_tool = serde_json::json!({
            "type": "message",
            "message": {
                "role": "toolResult",
                "toolCallId": "pi-call-failed",
                "content": "failure details",
                "isError": true,
            },
        });
        assert_eq!(
            projected_body(&failed_tool, EventType::ToolOutput),
            "failure details"
        );
    }
}
