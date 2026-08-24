use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap},
    fs, io,
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
    JsonlFamilyProjector, JsonlFamilyWorkerContext, JsonlReader, JsonlRecordRef,
};

use crate::{GeminiError, GeminiResult, GeminiRuntime, GEMINI_CLI_SOURCE_FORMAT};
use projection::{gemini_event_id, gemini_session_id, gemini_source_key, project_event};

const GEMINI_SOURCE_ANCHOR_NAMESPACE: &str = "gemini.session";
const GEMINI_NATIVE_SESSION_NAMESPACE: &str = "gemini.session";
const GEMINI_NATIVE_EVENT_NAMESPACE: &str = "gemini.event";
const GEMINI_LOGICAL_SESSION_KIND: &str = "gemini-session";
const GEMINI_LOGICAL_EVENT_KIND: &str = "gemini-event";
const GEMINI_SOURCE_SCHEMA_VARIANT: &str = "gemini-nativepath-jsonl-v0";
const GEMINI_SOURCE_BACKED_PARSER_REVISION: &str = "gemini-nativepath-core-activity-v1";
const MAX_GEMINI_ACTIVITY_FIELD_BYTES: usize = 64 * 1024;

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

#[derive(Debug)]
struct GeminiJsonlAdapter<R>(PhantomData<fn() -> R>);

pub fn gemini_jsonl_adapter<R: GeminiRuntime>() -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
    Arc::new(GeminiJsonlAdapter(PhantomData))
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
        GEMINI_SOURCE_BACKED_PARSER_REVISION
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
        // Read each transcript's native session header and validate that every
        // discovered file shares this inventory's root authority before mapping
        // files onto source identities.
        let mut observed = Vec::with_capacity(discovery.transcripts.len());
        for transcript in discovery.transcripts {
            if transcript.authority.named_path() != authority.named_path()
                || transcript.authority.authority_fingerprint() != authority.authority_fingerprint()
            {
                return Err(GeminiError::SourceChangedDuringCapture);
            }
            let session = read_gemini_session_header(&transcript).map_err(capture_scan_error)?;
            observed.push((transcript, session));
        }
        // Gemini reuses a single `sessionId` across multiple transcript files
        // when a session is resumed: the resumed snapshot is written to a new
        // file that keeps the same native session id. Each logical session must
        // therefore map to exactly one source identity, otherwise the
        // source-backed refresh would try to replace the same source twice and
        // fail with "source replacement has already started". Collapse files
        // that share a session id into a single leaf, keeping the most complete
        // file so the canonical snapshot retains every event.
        let collapsed = gemini_collapse_session_leaves(observed)?;
        let mut leaves = Vec::with_capacity(collapsed.len());
        for (source, transcript, session) in collapsed {
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
        let expected_source =
            gemini_source_key(&binding.session.native_session_id).map_err(capture_error)?;
        if !expected_source.exact_descriptor_eq(leaf.source()) {
            return Err(GeminiError::SourceChangedDuringCapture);
        }
        let session_id = gemini_session_id(leaf.source(), &binding.session.native_session_id)
            .map_err(capture_error)?;
        let parent_session_id = binding
            .session
            .parent_native_session_id
            .as_deref()
            .map(|parent_native_session_id| {
                let parent_source =
                    gemini_source_key(parent_native_session_id).map_err(capture_error)?;
                gemini_session_id(&parent_source, parent_native_session_id).map_err(capture_error)
            })
            .transpose()?;
        let transcript = binding.transcript(leaf);
        Ok(Box::new(GeminiProjector {
            parser: GeminiBorrowedRecordParser::new(transcript.clone(), binding.session.clone()),
            source: leaf.source().clone(),
            session: binding.session,
            session_id,
            parent_session_id,
            source_file,
            authority: Arc::clone(leaf.authority()),
            native_item_ids: GeminiSourceNativeItemIds::default(),
            emitted_event_digests: BTreeSet::new(),
            runtime: PhantomData,
        }))
    }
}

/// Collapses the discovered `(transcript, session)` pairs into one entry per
/// native `sessionId`.
///
/// When two transcript files share a `sessionId` (Gemini resuming an existing
/// session), both would otherwise derive the same `SourceKey` and the
/// source-backed refresh would attempt a duplicate source replacement. We keep
/// a single authoritative transcript per session id; the chosen file is the one
/// most likely to contain the complete conversation (the resumed snapshot
/// re-dumps the prior turns), so no events are dropped.
fn gemini_collapse_session_leaves(
    observed: Vec<(GeminiTranscriptSource, GeminiSession)>,
) -> GeminiResult<Vec<(SourceKey, GeminiTranscriptSource, GeminiSession)>> {
    let mut collapsed: Vec<(SourceKey, GeminiTranscriptSource, GeminiSession)> = Vec::new();
    let mut index: HashMap<SourceKey, usize> = HashMap::new();
    for (transcript, session) in observed {
        let source = gemini_source_key(&session.native_session_id).map_err(capture_error)?;
        match index.get(&source).copied() {
            Some(existing) => {
                if gemini_transcript_is_authoritative(&transcript, &collapsed[existing].1) {
                    collapsed[existing] = (source, transcript, session);
                }
            }
            None => {
                index.insert(source.clone(), collapsed.len());
                collapsed.push((source, transcript, session));
            }
        }
    }
    Ok(collapsed)
}

/// Returns `true` when `candidate` is strictly more complete than `current` and
/// should therefore become the authoritative transcript for a shared
/// `sessionId`. Completeness is proxied by file size, then by modification time;
/// identical files are left unchanged so the decision is deterministic and never
/// discards events that the other copy already holds.
fn gemini_transcript_is_authoritative(
    candidate: &GeminiTranscriptSource,
    current: &GeminiTranscriptSource,
) -> bool {
    match candidate
        .observation
        .length
        .cmp(&current.observation.length)
    {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => matches!(
            candidate.observation.modified.cmp(&current.observation.modified),
            Ordering::Greater
        ),
    }
}

struct GeminiProjector<R> {
    parser: GeminiBorrowedRecordParser,
    source: SourceKey,
    session: GeminiSession,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
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
    let source_key = gemini_source_key(&session.native_session_id)?;
    let session_id = gemini_session_id(&source_key, &session.native_session_id)?;
    let parent_session_id = session
        .parent_native_session_id
        .as_deref()
        .map(|parent_native_session_id| {
            let parent_source = gemini_source_key(parent_native_session_id)?;
            gemini_session_id(&parent_source, parent_native_session_id)
        })
        .transpose()?;
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
mod duplicate_session_tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use serde_json::{json, Value};
    use tempfile::TempDir;

    use super::{
        discover_gemini_transcripts, gemini_collapse_session_leaves, project_gemini_test_events,
        read_gemini_session_header,
    };
    use crate::nativepath::dto::{GeminiRetainedEvent, GeminiScanOutcome, GeminiTranscriptSource};
    use crate::nativepath::parser::read_gemini_transcript_pages;

    fn header(session_id: &str) -> Value {
        json!({
            "sessionId": session_id,
            "startTime": "2026-01-01T00:00:00.000Z",
            "lastUpdated": "2026-01-01T00:00:00.000Z",
            "kind": "main",
            "directories": ["/workspace/project"]
        })
    }

    fn jsonl(values: &[Value]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in values {
            serde_json::to_writer(&mut bytes, value).unwrap();
            bytes.push(b'\n');
        }
        bytes
    }

    fn write_chat(root: &Path, name: &str, values: &[Value]) -> PathBuf {
        let path = root.join("tmp/project/chats").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, jsonl(values)).unwrap();
        path
    }

    fn scan_collect(
        source: &GeminiTranscriptSource,
    ) -> (GeminiScanOutcome, Vec<GeminiRetainedEvent>) {
        let mut reader = read_gemini_transcript_pages(source, None).unwrap();
        let mut rows = Vec::new();
        while let Some(page) = reader.next_page().unwrap() {
            rows.extend(page.events);
        }
        let outcome = reader.outcome().cloned().unwrap();
        (outcome, rows)
    }

    /// Reproduces #659: two Gemini chat files in one directory that share a
    /// `sessionId` (a resumed session) must collapse to a single source identity
    /// so the source-backed refresh does not attempt a duplicate replacement.
    #[test]
    fn gemini_resumed_session_files_collapse_into_one_source() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join(".gemini");

        // Original session file: fewer events.
        let _original = write_chat(
            &root,
            "session-original.jsonl",
            &[
                header("shared-session"),
                json!({
                    "id": "user-1",
                    "timestamp": "2026-01-01T00:00:01.000Z",
                    "type": "user",
                    "content": "alpha"
                }),
            ],
        );
        // Resumed session file: re-dumps the prior turn and adds a new one, so it
        // is the more complete (larger) snapshot of the same logical session.
        write_chat(
            &root,
            "session-resumed.jsonl",
            &[
                header("shared-session"),
                json!({
                    "id": "user-1",
                    "timestamp": "2026-01-01T00:00:01.000Z",
                    "type": "user",
                    "content": "alpha"
                }),
                json!({
                    "id": "assistant-2",
                    "timestamp": "2026-01-01T00:00:02.000Z",
                    "type": "gemini",
                    "content": "beta"
                }),
            ],
        );

        let discovery = discover_gemini_transcripts(&root).unwrap();
        assert_eq!(
            discovery.transcripts.len(),
            2,
            "both files should be discovered"
        );

        let observed: Vec<_> = discovery
            .transcripts
            .iter()
            .map(|transcript| {
                let session = read_gemini_session_header(transcript).unwrap();
                (transcript.clone(), session)
            })
            .collect();
        let collapsed = gemini_collapse_session_leaves(observed).unwrap();

        // The duplicate sessionId must collapse to a single source identity.
        assert_eq!(
            collapsed.len(),
            1,
            "two files sharing a sessionId must map to one source identity"
        );
        // The most complete transcript is authoritative and retains every event.
        assert_eq!(
            collapsed[0]
                .1
                .path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("session-resumed.jsonl"),
            "the richer resumed file must be the authoritative transcript"
        );

        // Scanning the authoritative leaf yields the full union of events.
        let (_outcome, rows) = scan_collect(&collapsed[0].1);
        let records = project_gemini_test_events(&collapsed[0].1, rows).unwrap();
        assert_eq!(
            records.len(),
            2,
            "no events lost when collapsing duplicates"
        );
    }

    /// Each file on its own still imports to exactly one source.
    #[test]
    fn gemini_single_session_file_imports_to_one_source() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join(".gemini");
        write_chat(
            &root,
            "only.jsonl",
            &[
                header("solo-session"),
                json!({
                    "id": "user-1",
                    "timestamp": "2026-01-01T00:00:01.000Z",
                    "type": "user",
                    "content": "alpha"
                }),
            ],
        );

        let discovery = discover_gemini_transcripts(&root).unwrap();
        let observed: Vec<_> = discovery
            .transcripts
            .iter()
            .map(|transcript| {
                let session = read_gemini_session_header(transcript).unwrap();
                (transcript.clone(), session)
            })
            .collect();
        let collapsed = gemini_collapse_session_leaves(observed).unwrap();
        assert_eq!(collapsed.len(), 1);
    }

    /// Distinct session ids must not be collapsed together.
    #[test]
    fn gemini_distinct_session_files_keep_separate_sources() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join(".gemini");
        write_chat(
            &root,
            "a.jsonl",
            &[
                header("session-a"),
                json!({"id":"a-1","type":"user","content":"a"}),
            ],
        );
        write_chat(
            &root,
            "b.jsonl",
            &[
                header("session-b"),
                json!({"id":"b-1","type":"user","content":"b"}),
            ],
        );

        let discovery = discover_gemini_transcripts(&root).unwrap();
        let observed: Vec<_> = discovery
            .transcripts
            .iter()
            .map(|transcript| {
                let session = read_gemini_session_header(transcript).unwrap();
                (transcript.clone(), session)
            })
            .collect();
        let collapsed = gemini_collapse_session_leaves(observed).unwrap();
        assert_eq!(collapsed.len(), 2);
    }
}
