use std::{
    collections::BTreeSet,
    fs, io,
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::{
    derive_event_id, AgentScope, CaptureProvider, CoreActivity, CoreContentPolicyStatus,
    CoreRecord, EventIdentityInput, LiteralFactKind, NativeItemKey, ProviderDeclaredFact,
    SourceAnchorScope, SourceKey, StableEntityId, TypedKey, CORE_ACTIVITY_REVISION,
};
use ctx_history_native_jsonl_parsers::deepseek_harness::{
    agent_scope, exact_file_references, is_session_leaf, is_zstd_session_leaf, parse_row,
    sequence_span, session_identity, source_key_scoped, visit_storage_rows, ParsedRow,
    SemanticEvent, SequenceSpan, SessionHeader, StorageRowsError, SOURCE_SCHEMA_VARIANT,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    common::io::{
        OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderSourceDirectory,
        ProviderSourceRoot, PROVIDER_JSONL_INVENTORY_MAX_DEPTH,
        PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES, PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
    },
    provider::source_backed::{
        family::jsonl::{
            observe_opened_file, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory,
            JsonlFamilyLeaf, JsonlFamilyProjectionMode, JsonlFamilySemanticExecutor,
            JsonlFamilySemanticPage, JsonlFamilySemanticPreflight, JsonlFamilySemanticSummary,
            JsonlFamilyWorkerContext, JsonlPhysicalDigest, JsonlPhysicalStream, JsonlRecordFraming,
            JsonlResumableSha256,
        },
        IndexBaseEventLookup,
    },
    CaptureError, JsonlProviderRuntime, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};
use ctx_history_capture_runtime::{
    SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDraft,
    SourceBackedRecordRejectionDrafts,
};
use ctx_history_jsonl::{JsonlFamilyExecutionIo, JsonlPhysicalEncoding};

pub(crate) const TREE_SOURCE_FORMAT: &str = "deepseek_harness_session_jsonl_tree";
pub(crate) const EXPLICIT_SOURCE_FORMAT: &str = "deepseek_harness_session_jsonl";

const PARSER_REVISION: &str = "deepseek-harness-native-jsonl-v3-selected-core-activity";
const EVENT_IDENTITY_REVISION: &str = "deepseek-harness-sequence-v1";

#[derive(Debug, Clone, Copy)]
pub(crate) struct DeepSeekHarnessJsonlAdapter<R> {
    source_anchor_scope: SourceAnchorScope,
    runtime: PhantomData<fn() -> R>,
}

pub(crate) fn jsonl_adapter<R: JsonlProviderRuntime>(
    source_format: &'static str,
) -> Result<Arc<dyn JsonlFamilyAdapter<Runtime = R>>> {
    jsonl_adapter_with_source_root_lineage(source_format, None)
}

pub(crate) fn jsonl_adapter_with_source_root_lineage<R: JsonlProviderRuntime>(
    source_format: &'static str,
    source_root_lineage: Option<[u8; 32]>,
) -> Result<Arc<dyn JsonlFamilyAdapter<Runtime = R>>> {
    if !matches!(source_format, TREE_SOURCE_FORMAT | EXPLICIT_SOURCE_FORMAT) {
        return Err(CaptureError::InvalidPayload(format!(
            "unknown DeepSeek Harness source format {source_format:?}"
        )));
    }
    Ok(Arc::new(DeepSeekHarnessJsonlAdapter {
        source_anchor_scope: source_root_lineage
            .map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
        runtime: PhantomData,
    }))
}

impl<R: JsonlProviderRuntime> JsonlFamilyAdapter for DeepSeekHarnessJsonlAdapter<R> {
    type Runtime = R;
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::DeepSeekHarness
    }

    fn source_format(&self) -> &'static str {
        EXPLICIT_SOURCE_FORMAT
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

    fn bind_admitted_eof(&self) -> bool {
        true
    }

    fn physical_encoding(&self, leaf: &JsonlFamilyLeaf) -> JsonlPhysicalEncoding {
        encoding_for_path(leaf.source_path())
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        match fs::symlink_metadata(root) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        }
        let selected = fs::canonicalize(root)?;
        let root_is_file = fs::symlink_metadata(root)?.is_file();
        let authority_path = if root_is_file {
            selected
                .parent()
                .ok_or(CaptureError::InvalidProviderTranscriptPath {
                    path: selected.clone(),
                    reason: "selected DeepSeek Harness transcript has no authority directory",
                })?
                .to_path_buf()
        } else {
            selected.clone()
        };
        let authority = Arc::new(ProviderSourceRoot::open(&authority_path)?);
        let paths = if root_is_file {
            if !is_session_leaf(&selected) {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: selected,
                    reason:
                        "DeepSeek Harness transcript must be session.jsonl or session.jsonl.zstd",
                });
            }
            vec![selected]
        } else {
            discover_session_leaves(&authority)?
        };
        let mut session_ids = BTreeSet::new();
        let mut inventory_encoding = None;
        let mut leaves = Vec::with_capacity(paths.len());
        for path in paths {
            let relative = path
                .strip_prefix(authority.named_path())
                .map(Path::to_path_buf)
                .map_err(|_| CaptureError::SourceChangedDuringCapture)?;
            let opened = authority.open_file(&relative)?;
            let encoding = encoding_for_path(&path);
            if inventory_encoding
                .replace(encoding)
                .is_some_and(|seen| seen != encoding)
            {
                return Err(CaptureError::InvalidPayload(
                    "DeepSeek Harness inventory mixes raw and Zstandard session artifacts"
                        .to_owned(),
                ));
            }
            let binding = read_header_binding(&path, &opened, encoding)?;
            if !session_ids.insert(binding.id.clone()) {
                return Err(CaptureError::InvalidPayload(
                    "DeepSeek Harness inventory repeats a native session identity".to_owned(),
                ));
            }
            let source = source_key_scoped(
                EXPLICIT_SOURCE_FORMAT,
                &binding.id,
                self.source_anchor_scope,
            )
            .map_err(contract)?;
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                path,
                Arc::clone(&authority),
                relative,
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn semantic_executor(
        &self,
        leaf: &JsonlFamilyLeaf,
        checkpoint: Option<&TypedKey>,
        _base_event_lookup: Option<IndexBaseEventLookup<R>>,
        _mode: JsonlFamilyProjectionMode,
    ) -> Result<Option<Box<dyn JsonlFamilySemanticExecutor<Runtime = R>>>> {
        let expected_sequence = match checkpoint {
            None => 0,
            Some(TypedKey::U64(sequence)) => *sequence,
            Some(_) => {
                return Err(CaptureError::InvalidPayload(
                    "DeepSeek Harness provider checkpoint is malformed".to_owned(),
                ));
            }
        };
        let binding = decode_binding(leaf)?;
        Ok(Some(Box::new(DeepSeekHarnessSemanticExecutor::<R>::new(
            leaf.source().clone(),
            self.source_anchor_scope,
            leaf.source_path().to_path_buf(),
            binding,
            encoding_for_path(leaf.source_path()),
            expected_sequence,
        )?)))
    }
}

struct DeepSeekHarnessSemanticExecutor<R> {
    source: SourceKey,
    source_anchor_scope: SourceAnchorScope,
    source_selector: PathBuf,
    binding: SessionHeader,
    encoding: JsonlPhysicalEncoding,
    session_id: StableEntityId,
    agent_scope: Option<AgentScope>,
    expected_sequence: u64,
    represented_frames: u64,
    rejected_physical_frames: u64,
    logical_complete_rows: u64,
    rejected_rows: u64,
    record_rejections: SourceBackedRecordRejectionDrafts,
    runtime: PhantomData<fn() -> R>,
}

struct FrameProjection {
    records: Vec<CoreRecord>,
    logical_complete_rows: u64,
    rejected_rows: u64,
    represented: bool,
}

impl<R> DeepSeekHarnessSemanticExecutor<R> {
    fn new(
        source: SourceKey,
        source_anchor_scope: SourceAnchorScope,
        source_selector: PathBuf,
        binding: SessionHeader,
        encoding: JsonlPhysicalEncoding,
        expected_sequence: u64,
    ) -> Result<Self> {
        let session_id = session_identity(&source, &binding.id).map_err(contract)?;
        let agent_scope = Some(agent_scope(&binding));
        Ok(Self {
            source,
            source_anchor_scope,
            source_selector,
            binding,
            encoding,
            session_id,
            agent_scope,
            expected_sequence,
            represented_frames: 0,
            rejected_physical_frames: 0,
            logical_complete_rows: 0,
            rejected_rows: 0,
            record_rejections: Default::default(),
            runtime: PhantomData,
        })
    }

    fn validate_frame(
        &mut self,
        bytes: &[u8],
        ordinal: u64,
        project: bool,
    ) -> Result<FrameProjection> {
        let mut records = Vec::new();
        let mut logical_complete_rows = 0_u64;
        let mut rejected_rows = 0_u64;
        let mut represented = false;
        visit_frame_rows(bytes, self.encoding, |row_index, row| {
            if !project && (ordinal != 0 || row_index != 0) {
                return Ok(());
            }
            let parsed = match parse_row(row) {
                Ok(parsed) => parsed,
                Err(error)
                    if error.starts_with(
                        "unsupported required DeepSeek Harness semantic event type",
                    ) =>
                {
                    return Err(CaptureError::InvalidPayload(error));
                }
                Err(error) if project && (ordinal != 0 || row_index != 0) => {
                    let span = sequence_span(row);
                    let rejected = span.map_or(1, |span| span.len);
                    let line_number = self
                        .logical_complete_rows
                        .saturating_add(logical_complete_rows)
                        .saturating_add(1);
                    if let Some(span) = span {
                        self.validate_sequence(span)?;
                    }
                    logical_complete_rows = checked_add_rows(logical_complete_rows, rejected)?;
                    rejected_rows = checked_add_rows(rejected_rows, rejected)?;
                    self.record_rejections
                        .record(SourceBackedRecordRejectionDraft {
                            source: self.source.clone(),
                            provider: CaptureProvider::DeepSeekHarness,
                            source_selector: self.source_selector.display().to_string(),
                            line_number,
                            payload_type: serde_json::from_slice::<Value>(row)
                                .ok()
                                .and_then(|value| value.get("type")?.as_str().map(str::to_owned)),
                            class: SourceBackedRecordRejectionClass::MalformedRecord,
                            detail: error,
                        });
                    if rejected > 1 {
                        self.record_rejections
                            .record_omitted(usize::try_from(rejected - 1).unwrap_or(usize::MAX));
                    }
                    return Ok(());
                }
                Err(error) => return Err(CaptureError::InvalidPayload(error)),
            };
            match parsed {
                ParsedRow::Header(header) => {
                    if ordinal != 0 || row_index != 0 {
                        return Err(CaptureError::InvalidPayload(
                            "DeepSeek Harness session header is not the first row".to_owned(),
                        ));
                    }
                    if header != self.binding {
                        return Err(CaptureError::InvalidPayload(
                            "DeepSeek Harness session header changed".to_owned(),
                        ));
                    }
                    if project {
                        logical_complete_rows = checked_add_rows(logical_complete_rows, 1)?;
                        represented = true;
                    }
                }
                ParsedRow::Semantic(event) if project => {
                    self.validate_sequence(SequenceSpan {
                        first: event.seq,
                        len: 1,
                    })?;
                    logical_complete_rows = checked_add_rows(logical_complete_rows, 1)?;
                    represented = true;
                    records.extend(self.project_event(event)?);
                }
                ParsedRow::Ignored(span) if project => {
                    self.validate_sequence(span)?;
                    logical_complete_rows = checked_add_rows(logical_complete_rows, span.len)?;
                    represented = true;
                }
                ParsedRow::Semantic(_) | ParsedRow::Ignored(_) => {}
            }
            Ok(())
        })?;
        Ok(FrameProjection {
            records,
            logical_complete_rows,
            rejected_rows,
            represented,
        })
    }

    fn validate_sequence(&mut self, span: SequenceSpan) -> Result<()> {
        if span.first != self.expected_sequence {
            return Err(CaptureError::InvalidPayload(format!(
                "corrupt DeepSeek Harness session sequence: expected {}, got {}",
                self.expected_sequence, span.first
            )));
        }
        self.expected_sequence =
            self.expected_sequence
                .checked_add(span.len)
                .ok_or(CaptureError::SystemInvariant(
                    "DeepSeek Harness sequence overflowed",
                ))?;
        Ok(())
    }

    fn project_event(&self, event: SemanticEvent) -> Result<Option<CoreRecord>> {
        let native_key = TypedKey::U64(event.seq);
        let native_item_key =
            NativeItemKey::native_id("deepseek-harness-event", native_key.clone())
                .map_err(contract)?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: "deepseek-harness-event",
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .map_err(contract)?;
        // `new_selected` validates eagerly. Omitted records use a transient,
        // never-persisted body until policy status is applied below.
        let constructor_body = if event.text.trim().is_empty() {
            event.native_kind
        } else {
            event.text.as_str()
        };
        let mut record = CoreRecord::new_selected(
            event_id,
            self.session_id,
            self.source.clone(),
            event.seq,
            event.event_type.as_str(),
            PARSER_REVISION,
            constructor_body,
        )
        .map_err(contract)?;
        record.provider_session_id = Some(self.binding.id.clone());
        record.native_event_id = Some(native_key);
        record.occurred_at_unix_ms = Some(event.time_ms);
        record.role = Some(event.role.as_str().to_owned());
        record.agent_scope = self.agent_scope;
        if let Some(parent) = self.binding.parent_session.as_deref() {
            record.parent_session_id = Some(session_identity_for_native(
                parent,
                self.source_anchor_scope,
            )?);
        }
        if let Some(reason) = event.content_omission_reason {
            record.content.policy_status = CoreContentPolicyStatus::Omitted {
                reason: reason.to_owned(),
            };
            record.content.normalized_body = None;
            record.content.structured_content = None;
        } else {
            let mut facts = Vec::new();
            if let Some(cwd) = &self.binding.cwd {
                facts.push(ProviderDeclaredFact {
                    kind: LiteralFactKind::SessionCwd,
                    value: cwd.clone(),
                });
            }
            facts.extend(exact_file_references(&event.value).map_err(contract)?);
            if !facts.is_empty() {
                record.content.activity = Some(CoreActivity {
                    revision: CORE_ACTIVITY_REVISION,
                    provider_call_id: None,
                    invocation: None,
                    result: None,
                    facts,
                });
            }
            record.content.structured_content = Some(event.value);
            record
                .content
                .omit_structured_content_if_aggregate_exceeds_limit()
                .map_err(contract)?;
        }
        record.validate_contract().map_err(contract)?;
        Ok(Some(record))
    }
}

impl<R: JsonlProviderRuntime> JsonlFamilySemanticExecutor for DeepSeekHarnessSemanticExecutor<R> {
    type Runtime = R;
    fn preflight(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<R>,
    ) -> Result<JsonlFamilySemanticPreflight> {
        let requires_header = input.certified_prefix_end().is_none();
        let mut saw_header = false;
        while let Some(frame) = input.next_record()? {
            if !frame.complete() {
                break;
            }
            let bytes = input.record_bytes(frame)?;
            self.validate_frame(bytes, frame.physical_ordinal(), false)?;
            if frame.physical_ordinal() == 0 {
                saw_header = true;
            }
        }
        if requires_header && !saw_header {
            return Err(CaptureError::InvalidPayload(
                "DeepSeek Harness source has no complete session header frame".to_owned(),
            ));
        }
        Ok(JsonlFamilySemanticPreflight::Ready)
    }

    fn next_page(
        &mut self,
        input: &mut JsonlFamilyExecutionIo<R>,
        _worker: &mut JsonlFamilyWorkerContext<R>,
    ) -> Result<Option<JsonlFamilySemanticPage>> {
        let Some(frame) = input.next_record()? else {
            return Ok(None);
        };
        if !frame.complete() {
            return Ok(None);
        }
        let projection =
            self.validate_frame(input.record_bytes(frame)?, frame.physical_ordinal(), true)?;
        if projection.represented || self.encoding == JsonlPhysicalEncoding::ChecksummedZstdFrames {
            self.represented_frames =
                self.represented_frames
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "DeepSeek Harness represented-frame count overflowed",
                    ))?;
        } else {
            self.rejected_physical_frames = self.rejected_physical_frames.checked_add(1).ok_or(
                CaptureError::SystemInvariant("DeepSeek Harness rejected-frame count overflowed"),
            )?;
        }
        self.logical_complete_rows =
            checked_add_rows(self.logical_complete_rows, projection.logical_complete_rows)?;
        self.rejected_rows = self
            .rejected_rows
            .checked_add(projection.rejected_rows)
            .ok_or(CaptureError::SystemInvariant(
                "DeepSeek Harness rejected-row count overflowed",
            ))?;
        Ok(Some(JsonlFamilySemanticPage::new(projection.records)))
    }

    fn finish(self: Box<Self>) -> Result<JsonlFamilySemanticSummary> {
        Ok(JsonlFamilySemanticSummary::with_logical_counts(
            self.represented_frames,
            self.rejected_physical_frames,
            self.logical_complete_rows,
            self.rejected_rows,
            Some(TypedKey::U64(self.expected_sequence)),
        )
        .with_record_rejections(self.record_rejections))
    }
}

fn checked_add_rows(current: u64, additional: u64) -> Result<u64> {
    current
        .checked_add(additional)
        .ok_or(CaptureError::SystemInvariant(
            "DeepSeek Harness logical-row count overflowed",
        ))
}

fn visit_frame_rows(
    bytes: &[u8],
    encoding: JsonlPhysicalEncoding,
    visit: impl FnMut(usize, &[u8]) -> Result<()>,
) -> Result<()> {
    visit_storage_rows(
        bytes,
        encoding == JsonlPhysicalEncoding::ChecksummedZstdFrames,
        MAX_PROVIDER_JSONL_LINE_BYTES,
        visit,
    )
    .map_err(|error| match error {
        StorageRowsError::Invalid(detail) => CaptureError::InvalidPayload(detail),
        StorageRowsError::Visitor(error) => error,
    })
}

fn read_header_binding(
    path: &Path,
    opened: &OpenedProviderSourceFile,
    encoding: JsonlPhysicalEncoding,
) -> Result<SessionHeader> {
    let observation = observe_opened_file(path, opened)?;
    let mut stream = JsonlPhysicalStream::open_with_encoding(
        opened.file().try_clone()?,
        observation.length(),
        0,
        0,
        encoding,
        JsonlRecordFraming::ordinary(),
        JsonlPhysicalDigest::complete(JsonlResumableSha256::new()),
        || CaptureError::SourceChangedDuringCapture,
    )?;
    let frame = stream.next_record()?.ok_or_else(|| {
        CaptureError::InvalidPayload("DeepSeek Harness source is empty".to_owned())
    })?;
    if !frame.complete {
        return Err(CaptureError::InvalidPayload(
            "DeepSeek Harness session header frame is torn".to_owned(),
        ));
    }
    let mut header = None;
    visit_frame_rows(stream.record_bytes(frame), encoding, |row_index, row| {
        if row_index != 0 {
            return Err(CaptureError::InvalidPayload(
                "DeepSeek Harness header frame contains multiple JSONL rows".to_owned(),
            ));
        }
        let ParsedRow::Header(parsed) = parse_row(row).map_err(CaptureError::InvalidPayload)?
        else {
            return Err(CaptureError::InvalidPayload(
                "DeepSeek Harness source does not begin with a session header".to_owned(),
            ));
        };
        header = Some(parsed);
        Ok(())
    })?;
    opened.revalidate_same_object()?;
    header.ok_or_else(|| CaptureError::InvalidPayload("missing DeepSeek Harness header".to_owned()))
}

fn discover_session_leaves(authority: &Arc<ProviderSourceRoot>) -> Result<Vec<PathBuf>> {
    let mut state = DiscoveryState::default();
    visit_directory(&authority.directory()?, 0, &mut state)?;
    authority.revalidate()?;
    Ok(state.paths)
}

#[derive(Default)]
struct DiscoveryState {
    directories: usize,
    entries: usize,
    paths: Vec<PathBuf>,
}

fn visit_directory(
    directory: &ProviderSourceDirectory,
    depth: usize,
    state: &mut DiscoveryState,
) -> Result<()> {
    if depth > PROVIDER_JSONL_INVENTORY_MAX_DEPTH {
        return Err(CaptureError::InvalidPayload(
            "DeepSeek Harness inventory exceeds the directory-depth bound".to_owned(),
        ));
    }
    state.directories = state.directories.saturating_add(1);
    if state.directories > PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES {
        return Err(CaptureError::InvalidPayload(
            "DeepSeek Harness inventory exceeds the directory-count bound".to_owned(),
        ));
    }
    let remaining = PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES
        .checked_sub(state.entries)
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "DeepSeek Harness inventory exceeds the entry-count bound".to_owned(),
            )
        })?;
    let names = directory.entries(remaining)?;
    state.entries = state.entries.saturating_add(names.len());
    for name in names {
        let opened = directory.open_child(&name)?;
        match opened {
            OpenedProviderSourcePath::Directory(child) => {
                visit_directory(&child, depth.saturating_add(1), state)?;
            }
            OpenedProviderSourcePath::File(file) => {
                let path = directory
                    .authority_root()
                    .named_path()
                    .join(directory.relative_path())
                    .join(&name);
                if depth == 2 && is_session_leaf(&path) {
                    file.revalidate_same_object()?;
                    state.paths.push(path);
                }
            }
        }
    }
    directory.revalidate()?;
    Ok(())
}

fn encoding_for_path(path: &Path) -> JsonlPhysicalEncoding {
    if is_zstd_session_leaf(path) {
        JsonlPhysicalEncoding::ChecksummedZstdFrames
    } else {
        JsonlPhysicalEncoding::RawJsonl
    }
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> Result<SessionHeader> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(CaptureError::InvalidPayload(
            "DeepSeek Harness family binding is malformed".to_owned(),
        ));
    };
    serde_json::from_slice(bytes).map_err(Into::into)
}

fn session_identity_for_native(
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<StableEntityId> {
    let source = source_key_scoped(
        EXPLICIT_SOURCE_FORMAT,
        native_session_id,
        source_anchor_scope,
    )
    .map_err(contract)?;
    session_identity(&source, native_session_id).map_err(contract)
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARENT_NATIVE_ID: &str = "11111111-2222-4333-8444-555555555555";
    const CHILD_NATIVE_ID: &str = "66666666-7777-4888-8999-aaaaaaaaaaaa";

    fn session_header(native_session_id: &str, parent_session: Option<&str>) -> SessionHeader {
        let mut header = serde_json::json!({
            "type": "session",
            "version": 0,
            "id": native_session_id,
            "createdAt": 1,
            "delegationDepth": u64::from(parent_session.is_some()),
        });
        if let Some(parent_session) = parent_session {
            header["parentSession"] = Value::String(parent_session.to_owned());
            header["origin"] = Value::String("subagent".to_owned());
        }
        let encoded = serde_json::to_vec(&header).unwrap();
        let ParsedRow::Header(header) = parse_row(&encoded).unwrap() else {
            panic!("test session row did not parse as a header");
        };
        header
    }

    fn semantic_event(native_session_id: &str) -> SemanticEvent {
        let encoded = serde_json::to_vec(&serde_json::json!({
            "type": "user/message",
            "seq": 0,
            "time": 2,
            "data": {
                "content": [{"type": "text", "text": native_session_id}],
                "source": {"kind": "user"},
                "role": "user",
                "id": format!("{native_session_id}-event"),
            },
        }))
        .unwrap();
        let ParsedRow::Semantic(event) = parse_row(&encoded).unwrap() else {
            panic!("test event row did not parse as a semantic event");
        };
        event
    }

    fn project_session(
        native_session_id: &str,
        parent_session: Option<&str>,
        source_anchor_scope: SourceAnchorScope,
        source_root: &Path,
    ) -> CoreRecord {
        let source = source_key_scoped(
            EXPLICIT_SOURCE_FORMAT,
            native_session_id,
            source_anchor_scope,
        )
        .unwrap();
        DeepSeekHarnessSemanticExecutor::<()>::new(
            source,
            source_anchor_scope,
            source_root.join(native_session_id).join("session.jsonl"),
            session_header(native_session_id, parent_session),
            JsonlPhysicalEncoding::RawJsonl,
            0,
        )
        .unwrap()
        .project_event(semantic_event(native_session_id))
        .unwrap()
        .unwrap()
    }

    fn project_pair(
        source_anchor_scope: SourceAnchorScope,
        source_root: &Path,
    ) -> (CoreRecord, CoreRecord) {
        (
            project_session(PARENT_NATIVE_ID, None, source_anchor_scope, source_root),
            project_session(
                CHILD_NATIVE_ID,
                Some(PARENT_NATIVE_ID),
                source_anchor_scope,
                source_root,
            ),
        )
    }

    #[test]
    fn scoped_parent_child_projection_is_distinct_and_coherent_across_lineages() {
        let first = project_pair(SourceAnchorScope::Lineage([1; 32]), Path::new("first-root"));
        let second = project_pair(
            SourceAnchorScope::Lineage([2; 32]),
            Path::new("second-root"),
        );

        assert_eq!(first.0.parent_session_id, None);
        assert_eq!(first.1.parent_session_id, Some(first.0.session_id));
        assert_eq!(second.0.parent_session_id, None);
        assert_eq!(second.1.parent_session_id, Some(second.0.session_id));
        assert_ne!(first.0.session_id, second.0.session_id);
        assert_ne!(first.1.session_id, second.1.session_id);
        assert_ne!(
            first.1.parent_session_id,
            Some(session_identity(&first.1.source, PARENT_NATIVE_ID).unwrap())
        );
    }

    #[test]
    fn unqualified_projection_preserves_released_and_path_independent_identity() {
        let original = project_pair(SourceAnchorScope::Unqualified, Path::new("original-root"));
        let relocated = project_pair(SourceAnchorScope::Unqualified, Path::new("relocated-root"));
        let released_parent_source =
            ctx_history_native_jsonl_parsers::deepseek_harness::source_key(
                EXPLICIT_SOURCE_FORMAT,
                PARENT_NATIVE_ID,
            )
            .unwrap();
        let released_parent_session_id =
            session_identity(&released_parent_source, PARENT_NATIVE_ID).unwrap();

        assert!(original
            .0
            .source
            .exact_descriptor_eq(&released_parent_source));
        assert_eq!(original.0.session_id, released_parent_session_id);
        assert_eq!(
            original.1.parent_session_id,
            Some(released_parent_session_id)
        );
        assert_eq!(relocated.0.session_id, original.0.session_id);
        assert_eq!(relocated.1.session_id, original.1.session_id);
        assert_eq!(relocated.1.parent_session_id, original.1.parent_session_id);
    }
}
