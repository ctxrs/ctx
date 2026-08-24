use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read},
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Arc,
};

mod projection;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, CaptureProvider,
    CoreActivity, CoreRecord, CoreRecordAnnotation, CoreRecordError, LiteralFactKind,
    ProjectionContractError, ProviderDeclaredFact, SourceKey, StableEntityId, TypedKey,
    CORE_ACTIVITY_REVISION, MAX_CORE_CONTENT_BYTES,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::dto::{GeminiEventBody, GeminiTranscriptLayout};
use super::parser::{read_gemini_session_header, GeminiBorrowedRecordParser};
use super::{
    discover_gemini_transcripts, GeminiFileObservation, GeminiScanError, GeminiSession,
    GeminiTranscriptSource,
};
use crate::io::{OpenedProviderSourceFile, ProviderSourceRoot};
use ctx_history_jsonl::{
    JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
    JsonlFamilyProjector, JsonlFamilyTerminalProof, JsonlFamilyWorkerContext, JsonlReader,
    JsonlRecordRef,
};

use crate::{GeminiError, GeminiResult, GeminiRuntime, GEMINI_CLI_SOURCE_FORMAT};
#[cfg(any(test, feature = "test-support"))]
use projection::gemini_legacy_v1_source_key;
use projection::{gemini_event_id, gemini_session_id, gemini_source_key, project_event};

const GEMINI_SOURCE_ANCHOR_NAMESPACE: &str = "gemini.session";
const GEMINI_SOURCE_IDENTITY_VERSION: u32 = 2;
const GEMINI_NATIVE_SESSION_NAMESPACE: &str = "gemini.session";
const GEMINI_NATIVE_EVENT_NAMESPACE: &str = "gemini.event";
const GEMINI_LOGICAL_SESSION_KIND: &str = "gemini-session";
const GEMINI_LOGICAL_EVENT_KIND: &str = "gemini-event";
const GEMINI_SOURCE_SCHEMA_VARIANT: &str = "gemini-nativepath-jsonl-v0";
#[cfg(any(test, feature = "test-support"))]
const GEMINI_SOURCE_BACKED_PARSER_REVISION_V1: &str = "gemini-nativepath-core-activity-v1";
const GEMINI_SOURCE_BACKED_PARSER_REVISION: &str = "gemini-nativepath-core-activity-v2";
const MAX_GEMINI_ACTIVITY_FIELD_BYTES: usize = 64 * 1024;

#[cfg(any(test, feature = "test-support"))]
std::thread_local! {
    static AFTER_GEMINI_RECORDING_DISCOVERY_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn install_after_gemini_recording_discovery_hook(hook: impl FnOnce() + 'static) {
    AFTER_GEMINI_RECORDING_DISCOVERY_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "Gemini recording-discovery hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(any(test, feature = "test-support"))]
fn run_after_gemini_recording_discovery_hook() {
    let hook = AFTER_GEMINI_RECORDING_DISCOVERY_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[derive(Debug, Error)]
pub(crate) enum GeminiSourceBackedError {
    #[error(transparent)]
    Gemini(#[from] GeminiError),
    #[error(transparent)]
    Scan(#[from] GeminiScanError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Core(#[from] CoreRecordError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub(crate) type GeminiSourceBackedResult<T> = Result<T, GeminiSourceBackedError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeminiFamilyBinding {
    relative_path: PathBuf,
    layout: GeminiTranscriptLayout,
    observation: GeminiFileObservation,
    ordinary_file_token: [u8; 32],
    authority_relative_path: PathBuf,
    session: GeminiSession,
}

impl GeminiFamilyBinding {
    fn transcript(&self, leaf: &JsonlFamilyLeaf<GeminiError>) -> GeminiTranscriptSource {
        GeminiTranscriptSource {
            path: leaf.source_path().to_path_buf(),
            relative_path: self.relative_path.clone(),
            layout: self.layout.clone(),
            observation: self.observation.clone(),
            ordinary_file_token: self.ordinary_file_token,
            authority_relative_path: self.authority_relative_path.clone(),
            authority: leaf.authority().as_ref().clone(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum GeminiSourceIdentityRevision {
    #[cfg(any(test, feature = "test-support"))]
    LegacyV1,
    CurrentV2,
}

impl GeminiSourceIdentityRevision {
    fn source_key(self, session: &GeminiSession) -> GeminiSourceBackedResult<SourceKey> {
        match self {
            #[cfg(any(test, feature = "test-support"))]
            Self::LegacyV1 => gemini_legacy_v1_source_key(&session.native_session_id),
            Self::CurrentV2 => gemini_source_key(session),
        }
    }

    fn parser_revision(self) -> &'static str {
        match self {
            #[cfg(any(test, feature = "test-support"))]
            Self::LegacyV1 => GEMINI_SOURCE_BACKED_PARSER_REVISION_V1,
            Self::CurrentV2 => GEMINI_SOURCE_BACKED_PARSER_REVISION,
        }
    }
}

#[derive(Debug)]
struct GeminiJsonlAdapter<R> {
    source_identity_revision: GeminiSourceIdentityRevision,
    _runtime: PhantomData<fn() -> R>,
}

pub fn gemini_jsonl_adapter<R: GeminiRuntime>() -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
    Arc::new(GeminiJsonlAdapter {
        source_identity_revision: GeminiSourceIdentityRevision::CurrentV2,
        _runtime: PhantomData,
    })
}

#[cfg(any(test, feature = "test-support"))]
pub fn gemini_legacy_v1_jsonl_adapter_for_test<R: GeminiRuntime>(
) -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
    Arc::new(GeminiJsonlAdapter {
        source_identity_revision: GeminiSourceIdentityRevision::LegacyV1,
        _runtime: PhantomData,
    })
}

impl<R: GeminiRuntime> JsonlFamilyAdapter for GeminiJsonlAdapter<R> {
    type Runtime = R;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Gemini
    }

    fn source_format(&self) -> &'static str {
        GEMINI_CLI_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        GEMINI_SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        self.source_identity_revision.parser_revision()
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::ProjectorPreflight(false)
    }

    fn discover(&self, root: &Path) -> GeminiResult<JsonlFamilyInventory<GeminiError>> {
        let metadata = match fs::symlink_metadata(root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        };
        let discovery = discover_gemini_transcripts(root)?;
        if !discovery.completed_inventory {
            return Err(GeminiError::InvalidPayload(
                "Gemini discovery did not produce a complete inventory".to_owned(),
            ));
        }
        let authority = shared_authority(root, &metadata, &discovery.transcripts)?;
        let mut recordings = Vec::with_capacity(discovery.transcripts.len());
        for transcript in discovery.transcripts {
            if transcript.authority.named_path() != authority.named_path()
                || transcript.authority.authority_fingerprint() != authority.authority_fingerprint()
            {
                return Err(GeminiError::SourceChangedDuringCapture);
            }
            let session = read_gemini_session_header(&transcript).map_err(capture_scan_error)?;
            let source = self
                .source_identity_revision
                .source_key(&session)
                .map_err(capture_error)?;
            recordings.push((transcript, session, source));
        }
        #[cfg(any(test, feature = "test-support"))]
        run_after_gemini_recording_discovery_hook();
        let mut descriptor_counts = BTreeMap::<[u8; 32], Vec<(SourceKey, usize)>>::new();
        for (_, _, source) in &recordings {
            let candidates = descriptor_counts
                .entry(source.exact_descriptor_digest())
                .or_default();
            if let Some((_, count)) = candidates
                .iter_mut()
                .find(|(candidate, _)| candidate.exact_descriptor_eq(source))
            {
                *count += 1;
            } else {
                candidates.push((source.clone(), 1));
            }
        }
        let mut canonical_sources = BTreeMap::<[u8; 32], Vec<(SourceKey, [u8; 32])>>::new();
        let mut exact_dependencies = Vec::new();
        let mut distinct_recordings = Vec::with_capacity(recordings.len());
        for (transcript, session, source) in recordings {
            let digest = source.exact_descriptor_digest();
            let alias_count = descriptor_counts
                .get(&digest)
                .and_then(|candidates| {
                    candidates
                        .iter()
                        .find(|(candidate, _)| candidate.exact_descriptor_eq(&source))
                })
                .map(|(_, count)| *count)
                .ok_or(GeminiError::SystemInvariant(
                    "Gemini recording descriptor count is missing",
                ))?;
            if alias_count == 1 {
                distinct_recordings.push((transcript, session, source));
                continue;
            }

            let opened = authority.open_file(&transcript.authority_relative_path)?;
            if opened.ordinary_file_token() != transcript.ordinary_file_token {
                return Err(GeminiError::SourceChangedDuringCapture);
            }
            let content_sha256 = opened_file_sha256(&opened)?;
            let matching_canonical = canonical_sources.get(&digest).and_then(|candidates| {
                candidates
                    .iter()
                    .find(|(canonical, _)| canonical.exact_descriptor_eq(&source))
            });
            if let Some((_, canonical_content_sha256)) = matching_canonical {
                if canonical_content_sha256 != &content_sha256 {
                    return Err(GeminiError::InvalidPayload(
                        "distinct Gemini recordings declared the same recording identity"
                            .to_owned(),
                    ));
                }
                exact_dependencies.push(JsonlFamilyTerminalProof::exact_opened_path(
                    transcript.path,
                    Arc::clone(&authority),
                    transcript.authority_relative_path,
                    &opened,
                )?);
                continue;
            }
            canonical_sources
                .entry(digest)
                .or_default()
                .push((source.clone(), content_sha256));
            distinct_recordings.push((transcript, session, source));
        }
        let mut leaves = Vec::with_capacity(distinct_recordings.len());
        for (transcript, session, source) in distinct_recordings {
            let binding = GeminiFamilyBinding {
                relative_path: transcript.relative_path.clone(),
                layout: transcript.layout.clone(),
                observation: transcript.observation.clone(),
                ordinary_file_token: transcript.ordinary_file_token,
                authority_relative_path: transcript.authority_relative_path.clone(),
                session,
            };
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                transcript.path,
                Arc::clone(&authority),
                transcript.authority_relative_path,
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract_error)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
            .map(|inventory| inventory.with_exact_dependencies(exact_dependencies))
    }

    fn owns(&self, source: &SourceKey) -> bool {
        owns_gemini_source(source)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf<GeminiError>,
        source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> GeminiResult<Box<dyn JsonlFamilyProjector<Runtime = R>>> {
        let binding = decode_binding(leaf)?;
        if source_file.ordinary_file_token() != binding.ordinary_file_token {
            return Err(GeminiError::SourceChangedDuringCapture);
        }
        let expected_source = self
            .source_identity_revision
            .source_key(&binding.session)
            .map_err(capture_error)?;
        if !expected_source.exact_descriptor_eq(leaf.source()) {
            return Err(GeminiError::SourceChangedDuringCapture);
        }
        let session_id = gemini_session_id(leaf.source(), &binding.session.native_session_id)
            .map_err(capture_error)?;
        // The child path carries only a provider-session parent hint. Gemini
        // recording identity now needs the parent's complete header anchor,
        // so manufacturing a source from the hint would conflate resumed
        // parent recordings. Preserve the child scope and abstain from a ctx
        // parent-session edge until Core has a typed unresolved native claim.
        let parent_session_id = None;
        let transcript = binding.transcript(leaf);
        Ok(Box::new(GeminiProjector {
            parser: GeminiBorrowedRecordParser::new(transcript.clone(), binding.session.clone()),
            source: leaf.source().clone(),
            session: binding.session,
            session_id,
            parent_session_id,
            parser_revision: self.source_identity_revision.parser_revision(),
            source_file,
            authority: Arc::clone(leaf.authority()),
            native_item_ids: GeminiSourceNativeItemIds::default(),
            emitted_event_digests: BTreeSet::new(),
            runtime: PhantomData,
        }))
    }
}

fn opened_file_sha256(opened: &OpenedProviderSourceFile) -> GeminiResult<[u8; 32]> {
    let expected_length = opened.len();
    let mut file = opened.reopen_same_object()?;
    let mut hasher = Sha256::new();
    let mut observed_length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        observed_length = observed_length
            .checked_add(u64::try_from(count).map_err(|_| {
                GeminiError::SystemInvariant("Gemini recording read length exceeds u64")
            })?)
            .ok_or(GeminiError::SystemInvariant(
                "Gemini recording length overflowed u64",
            ))?;
        hasher.update(&buffer[..count]);
    }
    if observed_length != expected_length
        || opened.current_ordinary_file_token()? != opened.ordinary_file_token()
    {
        return Err(GeminiError::SourceChangedDuringCapture);
    }
    opened.revalidate_leaf()?;
    Ok(hasher.finalize().into())
}

fn owns_gemini_source(source: &SourceKey) -> bool {
    source.provider() == CaptureProvider::Gemini.as_str()
        && source.source_format() == GEMINI_CLI_SOURCE_FORMAT
        && source.schema_variant() == GEMINI_SOURCE_SCHEMA_VARIANT
        && matches!(
            source.provider_identity_version(),
            1 | GEMINI_SOURCE_IDENTITY_VERSION
        )
}

struct GeminiProjector<R> {
    parser: GeminiBorrowedRecordParser,
    source: SourceKey,
    session: GeminiSession,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    parser_revision: &'static str,
    source_file: Arc<OpenedProviderSourceFile>,
    authority: Arc<ProviderSourceRoot>,
    native_item_ids: GeminiSourceNativeItemIds,
    emitted_event_digests: BTreeSet<[u8; 32]>,
    runtime: PhantomData<fn() -> R>,
}

#[derive(Debug, Default)]
pub(super) struct GeminiSourceNativeItemIds {
    header_seen: bool,
    ids: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiSourceNativeItemProbe {
    id: Option<String>,
    session_id: Option<String>,
}

impl GeminiSourceNativeItemIds {
    fn candidate(&mut self, payload: &[u8]) -> Option<String> {
        let Ok(probe) = serde_json::from_slice::<GeminiSourceNativeItemProbe>(payload) else {
            return None;
        };
        if probe
            .session_id
            .as_deref()
            .is_some_and(|session_id| !session_id.trim().is_empty())
        {
            self.header_seen = true;
            return None;
        }
        if !self.header_seen {
            return None;
        }
        probe.id.filter(|id| !id.trim().is_empty())
    }

    fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    fn remember(&mut self, id: Option<String>) {
        if let Some(id) = id {
            self.ids.insert(id);
        }
    }

    #[cfg(test)]
    pub(super) fn admit(&mut self, payload: &[u8]) -> bool {
        let candidate = self.candidate(payload);
        if candidate.as_deref().is_some_and(|id| self.contains(id)) {
            return false;
        }
        self.remember(candidate);
        true
    }
}

impl<R: GeminiRuntime> JsonlFamilyProjector for GeminiProjector<R> {
    type Runtime = R;

    fn preflight(
        &mut self,
        reader: &mut JsonlReader<GeminiError>,
        _certified_prefix_end: Option<u64>,
    ) -> GeminiResult<bool> {
        consume_neutral_preflight(reader)?;
        Ok(false)
    }

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<R>,
        emit: &mut dyn FnMut(CoreRecord) -> GeminiResult<()>,
    ) -> GeminiResult<()> {
        let native_item_id = self.native_item_ids.candidate(record.bytes());
        if native_item_id
            .as_deref()
            .is_some_and(|id| self.native_item_ids.contains(id))
        {
            return Ok(());
        }
        let evidence = record.evidence();
        let events = self
            .parser
            .project(
                record.bytes(),
                evidence.physical_ordinal(),
                evidence.byte_start(),
                evidence.byte_end_exclusive(),
                evidence.record_digest(),
            )
            .map_err(capture_scan_error)?;
        if !events.is_empty() {
            self.native_item_ids.remember(native_item_id);
        }
        let mut retained = Vec::new();
        for event in events {
            let event_id =
                gemini_event_id(&self.source, self.session_id, &event).map_err(capture_error)?;
            if !self.emitted_event_digests.insert(event_id.digest()) {
                continue;
            }
            let annotation =
                gemini_annotation_for_event(&self.session, &event).map_err(capture_error)?;
            retained.push((event, annotation));
        }
        for (event, annotation) in retained {
            emit(
                project_event(
                    &self.source,
                    self.session_id,
                    self.parent_session_id,
                    self.parser_revision,
                    &self.session,
                    event,
                    projection::GeminiProjectedContent { annotation },
                )
                .map_err(capture_error)?,
            )?;
        }
        Ok(())
    }

    fn finish(&mut self) -> GeminiResult<()> {
        self.parser.finish().map_err(capture_scan_error)?;
        self.source_file.revalidate_leaf()?;
        self.authority.revalidate()
    }
}

fn consume_neutral_preflight(reader: &mut JsonlReader<GeminiError>) -> GeminiResult<()> {
    while reader
        .visit_page(&mut |_record| -> GeminiResult<()> { Ok(()) })?
        .is_some()
    {}
    Ok(())
}

fn gemini_annotation_for_event(
    session: &GeminiSession,
    event: &super::GeminiRetainedEvent,
) -> GeminiSourceBackedResult<CoreRecordAnnotation> {
    let mut facts = Vec::new();
    if let Some(cwd) = session
        .cwd
        .as_deref()
        .filter(|cwd| !cwd.is_empty() && cwd.len() <= MAX_CORE_CONTENT_BYTES)
    {
        facts.push(provider_fact(LiteralFactKind::SessionCwd, cwd));
    }

    let occurred_at_unix_ms = event
        .occurred_at
        .or(session.started_at)
        .map(|timestamp| timestamp.timestamp_millis());
    let mut provider_call_id = None;
    let mut invocation = None;
    let mut result = None;

    match &event.body {
        GeminiEventBody::ToolCall { calls } => {
            if let [call] = calls.as_slice() {
                extend_exact_facts(&mut facts, &call.literal_facts);
                if let (Some(call_id), Some(tool)) = (
                    bounded_nonempty(call.id.as_deref()),
                    bounded_nonempty(call.name.as_deref()),
                ) {
                    provider_call_id = Some(TypedKey::utf8(call_id)?);
                    let (protocol, server, tool) = exact_gemini_tool_identity(
                        tool,
                        call.protocol.as_deref(),
                        call.server.as_deref(),
                        call.explicit_tool.as_deref(),
                        call.mcp_identity_unavailable,
                    );
                    invocation = Some(ActivityInvocation {
                        protocol,
                        server,
                        tool,
                        arguments: json_capture(call.args.as_ref(), call.arguments_unavailable),
                        started_at_unix_ms: occurred_at_unix_ms,
                    });
                }
            }
        }
        GeminiEventBody::ToolResult {
            result: provider_result,
            call_id,
            call_id_unavailable,
            result_unavailable,
            literal_facts,
            ..
        } => {
            extend_exact_facts(&mut facts, literal_facts);
            if !call_id_unavailable {
                let Some(call_id) = bounded_nonempty(call_id.as_deref()) else {
                    return Ok(CoreRecordAnnotation {
                        activity: (!facts.is_empty()).then_some(CoreActivity {
                            revision: CORE_ACTIVITY_REVISION,
                            provider_call_id: None,
                            invocation: None,
                            result: None,
                            facts,
                        }),
                        structured_content: gemini_structured_content(event),
                    });
                };
                provider_call_id = Some(TypedKey::utf8(call_id)?);
                result = Some(ActivityResult {
                    status: None,
                    completed_at_unix_ms: occurred_at_unix_ms,
                    duration_ns: None,
                    text: if *result_unavailable {
                        ActivityTextCapture::Unavailable
                    } else {
                        match provider_result {
                            Some(serde_json::Value::String(value)) => {
                                ActivityTextCapture::Present {
                                    value: value.clone(),
                                }
                            }
                            Some(_) | None => ActivityTextCapture::Absent,
                        }
                    },
                    structured_content: json_capture(provider_result.as_ref(), *result_unavailable),
                });
            }
        }
        GeminiEventBody::Message { .. }
        | GeminiEventBody::StateNotice { .. }
        | GeminiEventBody::RewindNotice { .. } => {}
    }

    let activity =
        (invocation.is_some() || result.is_some() || !facts.is_empty()).then_some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id,
            invocation,
            result,
            facts,
        });
    Ok(CoreRecordAnnotation {
        activity,
        structured_content: gemini_structured_content(event),
    })
}

fn gemini_structured_content(event: &super::GeminiRetainedEvent) -> Option<serde_json::Value> {
    match &event.body {
        GeminiEventBody::ToolCall { calls } => calls
            .as_slice()
            .first()
            .filter(|call| !call.native_content_unavailable)
            .map(|call| call.native_content.clone()),
        GeminiEventBody::ToolResult {
            native_content,
            native_content_unavailable,
            ..
        } => (!native_content_unavailable).then_some(native_content.clone()),
        _ => serde_json::to_value(&event.body).ok(),
    }
}

fn exact_gemini_tool_identity(
    native_name: &str,
    protocol: Option<&str>,
    server: Option<&str>,
    explicit_tool: Option<&str>,
    unavailable: bool,
) -> (Option<String>, Option<String>, String) {
    if !unavailable {
        if let (Some("mcp"), Some(server), Some(tool)) = (protocol, server, explicit_tool) {
            return (
                Some("mcp".to_owned()),
                Some(server.to_owned()),
                tool.to_owned(),
            );
        }
    }
    (None, None, native_name.to_owned())
}

fn bounded_nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty() && value.len() <= MAX_GEMINI_ACTIVITY_FIELD_BYTES)
}

fn provider_fact(kind: LiteralFactKind, value: &str) -> ProviderDeclaredFact {
    ProviderDeclaredFact {
        kind,
        value: value.to_owned(),
    }
}

fn json_capture(value: Option<&serde_json::Value>, unavailable: bool) -> ActivityJsonCapture {
    if unavailable {
        ActivityJsonCapture::Unavailable
    } else {
        value.cloned().map_or(ActivityJsonCapture::Absent, |value| {
            ActivityJsonCapture::Present { value }
        })
    }
}

fn extend_exact_facts(facts: &mut Vec<ProviderDeclaredFact>, event_facts: &[ProviderDeclaredFact]) {
    if facts
        .len()
        .checked_add(event_facts.len())
        .is_some_and(|count| count <= ctx_history_core::MAX_PROVIDER_DECLARED_FACTS)
    {
        facts.extend(event_facts.iter().cloned());
    }
}

fn shared_authority(
    root: &Path,
    metadata: &fs::Metadata,
    transcripts: &[GeminiTranscriptSource],
) -> GeminiResult<Arc<ProviderSourceRoot>> {
    if let Some(transcript) = transcripts.first() {
        return Ok(Arc::new(transcript.authority.clone()));
    }
    let authority_path = if metadata.is_file() {
        root.parent()
            .ok_or(GeminiError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "Gemini transcript file has no parent authority",
            })?
    } else {
        root
    };
    Ok(Arc::new(ProviderSourceRoot::open(authority_path)?))
}

fn decode_binding(leaf: &JsonlFamilyLeaf<GeminiError>) -> GeminiResult<GeminiFamilyBinding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(GeminiError::InvalidPayload(
            "Gemini family leaf binding is malformed".to_owned(),
        ));
    };
    Ok(serde_json::from_slice(bytes)?)
}

#[cfg(test)]
pub(super) fn project_gemini_test_events(
    source: &GeminiTranscriptSource,
    events: Vec<super::GeminiRetainedEvent>,
) -> GeminiSourceBackedResult<Vec<CoreRecord>> {
    let session = read_gemini_session_header(source)?;
    let source_key = gemini_source_key(&session)?;
    let session_id = gemini_session_id(&source_key, &session.native_session_id)?;
    // This single-source helper has no complete inventory with which to prove
    // one exact parent recording occurrence. Keep the provider hint in the
    // parsed session and abstain from fabricating a parent source identity.
    let parent_session_id = None;
    let mut emitted_event_digests = BTreeSet::new();
    let mut records = Vec::new();
    for event in events {
        let event_id = gemini_event_id(&source_key, session_id, &event)?;
        if !emitted_event_digests.insert(event_id.digest()) {
            continue;
        }
        let annotation = gemini_annotation_for_event(&session, &event)?;
        records.push(project_event(
            &source_key,
            session_id,
            parent_session_id,
            GEMINI_SOURCE_BACKED_PARSER_REVISION,
            &session,
            event,
            projection::GeminiProjectedContent { annotation },
        )?);
    }
    Ok(records)
}

fn capture_scan_error(error: GeminiScanError) -> GeminiError {
    GeminiError::InvalidPayload(error.to_string())
}

fn capture_error(error: impl std::fmt::Display) -> GeminiError {
    GeminiError::InvalidPayload(error.to_string())
}

fn contract_error(error: impl std::fmt::Display) -> GeminiError {
    GeminiError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod neutral_preflight_tests {
    use super::*;
    use ctx_history_jsonl::JsonlSourceIdentity;

    #[test]
    fn neutral_preflight_consumes_complete_framing_without_semantic_output() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("neutral-preflight.jsonl");
        let bytes = b"{\"message\":\"first\"}\nnot-json\n{\"message\":\"last\"}\n";
        std::fs::write(&path, bytes).unwrap();
        let source = Arc::new(OpenedProviderSourceFile::open(&path).unwrap());
        let identity = JsonlSourceIdentity::new(
            "neutral-test",
            "neutral-preflight-v1",
            "physical-only-v1",
            [2; 32],
            path,
        );
        let mut reader = JsonlReader::open(identity, source, None, None).unwrap();

        consume_neutral_preflight(&mut reader).unwrap();

        let checkpoint = reader.outcome().unwrap().checkpoint();
        assert!(checkpoint.terminal());
        assert_eq!(checkpoint.next_physical_ordinal(), 3);
        assert_eq!(checkpoint.complete_prefix_end(), bytes.len() as u64);
    }
}

#[cfg(test)]
mod recording_identity_tests {
    use super::*;
    use ctx_history_core::{AgentScope, SourceAnchor};

    fn session(start_time: Option<&str>, project_hash: Option<&str>) -> GeminiSession {
        GeminiSession {
            native_session_id: "shared-provider-session".to_owned(),
            native_start_time: start_time.map(str::to_owned),
            project_hash: project_hash.map(str::to_owned),
            parent_native_session_id: None,
            agent_scope: AgentScope::Primary,
            started_at: None,
            cwd: None,
            cwd_ambiguous: false,
            native_kind: Some("main".to_owned()),
        }
    }

    #[test]
    fn recording_anchor_uses_exact_header_identity_not_route_or_content() {
        let baseline = session(Some("2026-08-23T15:53:00Z"), Some("project-a"));
        let mut relocated_rewritten = baseline.clone();
        relocated_rewritten.cwd = Some("/different/route/content".to_owned());
        relocated_rewritten.parent_native_session_id = Some("unrelated-hint".to_owned());
        relocated_rewritten.agent_scope = AgentScope::Subagent;
        let baseline_source = gemini_source_key(&baseline).unwrap();
        let relocated_source = gemini_source_key(&relocated_rewritten).unwrap();

        assert!(baseline_source.exact_descriptor_eq(&relocated_source));
        assert_eq!(baseline_source.provider_identity_version(), 2);
        let SourceAnchor::ProviderNative { namespace, key } = baseline_source.anchor() else {
            panic!("Gemini recording source must use provider-native evidence");
        };
        assert_eq!(namespace, GEMINI_SOURCE_ANCHOR_NAMESPACE);
        assert_eq!(
            key,
            &TypedKey::composite(vec![
                TypedKey::utf8("shared-provider-session").unwrap(),
                TypedKey::utf8("2026-08-23T15:53:00Z").unwrap(),
                TypedKey::utf8("project-a").unwrap(),
                TypedKey::utf8("main").unwrap(),
            ])
            .unwrap()
        );
    }

    #[test]
    fn resumed_recordings_share_provider_metadata_but_not_source_identity() {
        let first = session(Some("2026-08-23T15:53:00Z"), Some("project-a"));
        let resumed = session(Some("2026-08-23T16:03:00Z"), Some("project-a"));

        assert_eq!(first.native_session_id, resumed.native_session_id);
        assert_ne!(
            gemini_source_key(&first).unwrap(),
            gemini_source_key(&resumed).unwrap()
        );
    }

    #[test]
    fn identity_migration_owns_legacy_and_current_gemini_sources_only() {
        let current =
            gemini_source_key(&session(Some("2026-08-23T15:53:00Z"), Some("project-a"))).unwrap();
        let legacy = gemini_legacy_v1_source_key("shared-provider-session").unwrap();

        assert_ne!(legacy, current);
        assert!(owns_gemini_source(&legacy));
        assert!(owns_gemini_source(&current));
    }
}
