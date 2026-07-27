use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
    time::UNIX_EPOCH,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    compute_payload_hash, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
    CaptureSourceKind, EventRole, EventType, Fidelity, ProviderSourceTrust, Session, SessionStatus,
    SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, NativePathRetainedSourceEntities,
    NativePathSourceEntityFrontier, NativePathSourceEntityKind, NativePathSourceGenerationKey,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementReason, Store, StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    common::io::ensure_regular_provider_transcript_file,
    provider::{
        codex::events::{codex_canonical_event, CodexNativeEvent},
        importer::{
            avoid_provider_source_event_seq_collision, provider_path_identity,
            provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
            provider_source_event_import_identity, provider_source_identity,
            provider_source_session_uuid, provider_sync_metadata, timestamps,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
            NATIVE_INGESTION_PAGE_MAX_BYTES,
        },
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, CodexHistoryImportOptions, ImportProfile,
    OutputSourceIdentity, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProOutputSourceDisposition, ProviderImportFailure, ProviderImportSummary,
    ProviderImportWorkResult, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};

const SOURCE_FORMAT: &str = "codex_history_jsonl";
const CURSOR_VERSION: u32 = 1;
const PARSER_REVISION: &str = "codex-prompt-history-nativepath-jsonl-v1";
const POLICY_REVISION: &str = "codex-prompt-history-core-user-prompts-v1";
const OUTPUT_FRONTIER_VERSION: u32 = 1;
const OUTPUT_PARSER_REVISION: &str = "codex-prompt-history-no-output-v1";
const MAX_PAGE_RECORDS: usize = 60;
const MAX_PAGE_BYTES: usize = NATIVE_INGESTION_PAGE_MAX_BYTES - 512 * 1024;
const RETIREMENT_PAGE_LIMIT: usize = 256;
const PAGE_OVERHEAD_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
struct PromptLine {
    session_id: String,
    ts: i64,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileObservation {
    len: u64,
    modified_side: i8,
    modified_secs: u64,
    modified_nanos: u32,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl FileObservation {
    fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        Self::from_metadata(&fs::symlink_metadata(path)?)
    }

    fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);
        let (modified_side, modified_secs, modified_nanos) =
            match metadata.modified()?.duration_since(UNIX_EPOCH) {
                Ok(value) => (1, value.as_secs(), value.subsec_nanos()),
                Err(error) => {
                    let value = error.duration();
                    (-1, value.as_secs(), value.subsec_nanos())
                }
            };
        Ok(Self {
            len: metadata.len(),
            modified_side,
            modified_secs,
            modified_nanos,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    fn same_file(&self, other: &Self) -> bool {
        match (self.device, self.inode, other.device, other.inode) {
            (Some(left_device), Some(left_inode), Some(right_device), Some(right_inode)) => {
                left_device == right_device && left_inode == right_inode
            }
            _ => true,
        }
    }

    fn revalidate(&self, path: &Path) -> Result<bool> {
        match Self::read(path) {
            Ok(current) => Ok(current == *self),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug)]
struct SourceDigest {
    observation: FileObservation,
    revision: String,
    prefix_at_prior_len: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Lifecycle {
    Fresh,
    Append,
    Rewrite,
    Truncation,
    Replacement,
    Migration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RetirementKind {
    Session,
    SessionEdge,
    Run,
    Event,
    FileTouch,
}

impl RetirementKind {
    fn from_store(kind: NativePathSourceEntityKind) -> Result<Self> {
        match kind {
            NativePathSourceEntityKind::Session => Ok(Self::Session),
            NativePathSourceEntityKind::SessionEdge => Ok(Self::SessionEdge),
            NativePathSourceEntityKind::Run => Ok(Self::Run),
            NativePathSourceEntityKind::Event => Ok(Self::Event),
            NativePathSourceEntityKind::FileTouch => Ok(Self::FileTouch),
        }
    }

    const fn to_store(&self) -> NativePathSourceEntityKind {
        match self {
            Self::Session => NativePathSourceEntityKind::Session,
            Self::SessionEdge => NativePathSourceEntityKind::SessionEdge,
            Self::Run => NativePathSourceEntityKind::Run,
            Self::Event => NativePathSourceEntityKind::Event,
            Self::FileTouch => NativePathSourceEntityKind::FileTouch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetirementFrontier {
    kind: RetirementKind,
    id: Uuid,
}

impl RetirementFrontier {
    fn from_store(frontier: NativePathSourceEntityFrontier) -> Result<Self> {
        Ok(Self {
            kind: RetirementKind::from_store(frontier.kind)?,
            id: frontier.id,
        })
    }

    fn to_store(&self) -> NativePathSourceEntityFrontier {
        NativePathSourceEntityFrontier {
            kind: self.kind.to_store(),
            id: self.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum CursorPhase {
    Core {
        next_offset: u64,
        next_ordinal: u64,
        prefix_sha256: [u8; 32],
    },
    Retiring {
        after: Option<RetirementFrontier>,
        missing: bool,
    },
    Complete {
        missing: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptHistoryCursor {
    version: u32,
    parser_revision: String,
    policy_revision: String,
    route_identity: String,
    locator_identity: String,
    cursor_stream: String,
    canonical_source_identity: String,
    capture_source_id: Uuid,
    source_revision: String,
    generation: u64,
    generation_id: String,
    observation: FileObservation,
    lifecycle: Lifecycle,
    accepted_events: u64,
    session_runs: u64,
    rejected_records: u64,
    ignored_records: u64,
    last_session_hash: Option<[u8; 32]>,
    phase: CursorPhase,
}

impl PromptHistoryCursor {
    fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded).map_err(|_| {
            CaptureError::InvalidPayload(
                "Codex prompt-history NativePath cursor is corrupt".to_owned(),
            )
        })?;
        if cursor.version != CURSOR_VERSION
            || cursor.parser_revision != PARSER_REVISION
            || cursor.policy_revision != POLICY_REVISION
            || cursor.route_identity.is_empty()
            || cursor.locator_identity.is_empty()
            || cursor.cursor_stream.is_empty()
            || cursor.canonical_source_identity.is_empty()
            || cursor.source_revision.is_empty()
            || cursor.generation_id.is_empty()
        {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history NativePath cursor authority is incomplete".to_owned(),
            ));
        }
        Ok(cursor)
    }

    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::from)
    }

    fn validate_route(&self, authority: &SourceAuthority) -> Result<()> {
        if self.route_identity != authority.route_identity
            || self.locator_identity != authority.locator_identity
            || self.cursor_stream != authority.cursor_stream
            || self.canonical_source_identity != authority.proposed_source_identity
            || self.capture_source_id != authority.shared_source_id(&self.canonical_source_identity)
        {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history cursor belongs to a different source route".to_owned(),
            ));
        }
        revision_bytes(&self.source_revision)?;
        let missing = matches!(
            self.phase,
            CursorPhase::Retiring { missing: true, .. } | CursorPhase::Complete { missing: true }
        );
        if self.generation_id != generation_id(self.generation, &self.source_revision, missing) {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history cursor generation authority is invalid".to_owned(),
            ));
        }
        Ok(())
    }

    fn terminal(&self) -> bool {
        matches!(self.phase, CursorPhase::Complete { .. })
    }

    fn terminal_live(&self) -> bool {
        matches!(self.phase, CursorPhase::Complete { missing: false })
    }
}

#[derive(Debug)]
enum StoredCursor {
    None,
    Released,
    Native { cursor: PromptHistoryCursor },
}

#[derive(Debug, Clone)]
struct SourceAuthority {
    physical_path: std::path::PathBuf,
    machine_id: String,
    raw_source_path: String,
    route_identity: String,
    locator_identity: String,
    cursor_stream: String,
    proposed_source_identity: String,
}

impl SourceAuthority {
    fn new(path: &Path, logical_path: &Path, machine_id: &str) -> Result<Self> {
        let raw_source_path = logical_path.display().to_string();
        let route_identity = provider_path_identity(logical_path)?;
        let locator_identity = provider_path_identity(path)?;
        let cursor_stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Codex,
            SOURCE_FORMAT,
            &route_identity,
        );
        let proposed_source_identity = provider_source_identity(
            CaptureProvider::Codex,
            SOURCE_FORMAT,
            Some(&raw_source_path),
            Some(&raw_source_path),
            None,
            &Value::Null,
        )
        .ok_or(CaptureError::SystemInvariant(
            "Codex prompt-history canonical source identity is unavailable",
        ))?;
        Ok(Self {
            physical_path: path.to_path_buf(),
            machine_id: machine_id.to_owned(),
            raw_source_path,
            route_identity,
            locator_identity,
            cursor_stream,
            proposed_source_identity,
        })
    }

    fn shared_source_id(&self, canonical_source_identity: &str) -> Uuid {
        stable_capture_uuid(
            &format!("codex-prompt-history:{canonical_source_identity}"),
            "native-source",
        )
    }
}

#[derive(Debug)]
struct RawRecord {
    bytes: Vec<u8>,
    observed_bytes: usize,
    terminated: bool,
}

#[derive(Debug)]
struct PromptRow {
    session_id: String,
    event: CodexNativeEvent,
    event_hash: String,
}

#[derive(Debug)]
struct PreparedPage {
    rows: Vec<PromptRow>,
    failures: Vec<ProviderImportFailure>,
    retained_bytes: usize,
    next_offset: u64,
    next_ordinal: u64,
    prefix_sha256: [u8; 32],
    accepted_events: u64,
    session_runs: u64,
    rejected_records: u64,
    ignored_records: u64,
    last_session_hash: Option<[u8; 32]>,
    terminal: bool,
}

pub(crate) fn import_codex_native_prompt_history(
    path: &Path,
    store: &mut Store,
    options: CodexHistoryImportOptions,
) -> Result<ProviderImportSummary> {
    let logical_path = options.source_path.as_deref().unwrap_or(path);
    let authority = SourceAuthority::new(path, logical_path, &options.machine_id)?;
    let mut stored = load_cursor(store, &options.machine_id, &authority.cursor_stream)?;

    if options.import_profile.is_replay_only() {
        replay_no_outputs(store, &authority, &options)?;
        return Ok(ProviderImportSummary::default());
    }
    if !path.exists() {
        return retire_disappeared_source(store, &authority, stored, &options);
    }
    if matches!(
        &stored,
        StoredCursor::Native {
            cursor: PromptHistoryCursor {
                phase: CursorPhase::Retiring { missing: true, .. },
                ..
            }
        }
    ) {
        let mut summary = finish_pending_missing_retirement(store, &authority, &options, &stored)?;
        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
            summary.work_remaining = true;
            return Ok(summary);
        }
        stored = load_cursor(store, &options.machine_id, &authority.cursor_stream)?;
    }

    ensure_active_journal(store)?;
    let prior_native = match &stored {
        StoredCursor::Native { cursor } => {
            cursor.validate_route(&authority)?;
            Some(cursor)
        }
        _ => None,
    };
    let digest = digest_source(
        path,
        prior_native.map(|cursor| cursor.observation.len),
        options.inventory_observation_token.as_deref(),
    )?;
    if let Some(cursor) = prior_native {
        if cursor.source_revision == digest.revision
            && cursor.observation.same_file(&digest.observation)
            && cursor.terminal_live()
        {
            let mut summary = replay_summary(cursor);
            if let ImportProfile::CoreAndPro(sink) = &options.import_profile {
                replay_empty_output_or_mark_behind(
                    store,
                    &authority,
                    &digest.revision,
                    cursor,
                    sink.as_ref(),
                );
            }
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
    }

    let mut cursor = plan_cursor(&authority, stored, &digest)?;
    let guard = store.begin_event_search_bulk_mode()?;
    let import = import_core_and_retire(store, &guard, &authority, &digest, &options, &mut cursor);
    let finish = store.finish_event_search_bulk_mode(&guard);
    let mut summary = import?;
    finish?;
    if !summary.work_remaining {
        if let ImportProfile::CoreAndPro(sink) = &options.import_profile {
            replay_empty_output_or_mark_behind(
                store,
                &authority,
                &digest.revision,
                &cursor,
                sink.as_ref(),
            );
        }
    }
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn finish_pending_missing_retirement(
    store: &Store,
    authority: &SourceAuthority,
    options: &CodexHistoryImportOptions,
    stored: &StoredCursor,
) -> Result<ProviderImportSummary> {
    let StoredCursor::Native { cursor } = stored else {
        return Err(CaptureError::SystemInvariant(
            "Codex prompt-history missing retirement lost NativePath authority",
        ));
    };
    let mut cursor = cursor.clone();
    cursor.validate_route(authority)?;
    let digest = SourceDigest {
        observation: cursor.observation.clone(),
        revision: cursor.source_revision.clone(),
        prefix_at_prior_len: None,
    };
    ensure_active_journal(store)?;
    let guard = store.begin_event_search_bulk_mode()?;
    let import = import_core_and_retire(store, &guard, authority, &digest, options, &mut cursor);
    let finish = store.finish_event_search_bulk_mode(&guard);
    let mut summary = import?;
    finish?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn import_core_and_retire(
    store: &Store,
    guard: &ctx_history_store::EventSearchBulkGuard,
    authority: &SourceAuthority,
    digest: &SourceDigest,
    options: &CodexHistoryImportOptions,
    cursor: &mut PromptHistoryCursor,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    loop {
        match cursor.phase.clone() {
            CursorPhase::Core {
                next_offset,
                next_ordinal,
                prefix_sha256,
            } => {
                let page = prepare_page(
                    authority,
                    digest,
                    cursor,
                    next_offset,
                    next_ordinal,
                    prefix_sha256,
                )?;
                publish_core_page(
                    store,
                    guard,
                    authority,
                    digest,
                    options,
                    cursor,
                    page,
                    &mut summary,
                )?;
            }
            CursorPhase::Retiring { after, missing } => {
                let page = publish_retirement_page(
                    store,
                    guard,
                    authority,
                    options,
                    cursor,
                    after.as_ref(),
                )?;
                let next_after = page
                    .next_after
                    .map(RetirementFrontier::from_store)
                    .transpose()?;
                let next_phase = if page.done {
                    CursorPhase::Complete { missing }
                } else {
                    CursorPhase::Retiring {
                        after: next_after,
                        missing,
                    }
                };
                publish_cursor_advance(
                    store,
                    guard,
                    authority,
                    options,
                    cursor,
                    next_phase,
                    page.done && missing,
                )?;
            }
            CursorPhase::Complete { .. } => break,
        }
        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
            summary.work_remaining = !cursor.terminal();
            break;
        }
    }
    Ok(summary)
}

fn prepare_page(
    authority: &SourceAuthority,
    digest: &SourceDigest,
    cursor: &PromptHistoryCursor,
    start_offset: u64,
    start_ordinal: u64,
    expected_prefix: [u8; 32],
) -> Result<PreparedPage> {
    let file = File::open(&authority.physical_path)?;
    if FileObservation::from_metadata(&file.metadata()?)? != digest.observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut reader = BufReader::new(file);
    let mut prefix = Sha256::new();
    hash_prefix_and_seek(&mut reader, &mut prefix, start_offset)?;
    let actual_prefix: [u8; 32] = prefix.clone().finalize().into();
    if actual_prefix != expected_prefix {
        return Err(CaptureError::InvalidPayload(
            "Codex prompt-history committed prefix no longer matches the source".to_owned(),
        ));
    }
    let mut offset = start_offset;
    let mut ordinal = start_ordinal;
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut accepted_events = cursor.accepted_events;
    let mut session_runs = cursor.session_runs;
    let mut rejected_records = cursor.rejected_records;
    let mut ignored_records = cursor.ignored_records;
    let mut last_session_hash = cursor.last_session_hash;

    while ordinal.saturating_sub(start_ordinal) < MAX_PAGE_RECORDS as u64
        && retained_bytes.saturating_add(PAGE_OVERHEAD_BYTES) < MAX_PAGE_BYTES
    {
        let record_start = offset;
        let prefix_before = prefix.clone();
        let Some(record) = read_record(&mut reader, &mut prefix)? else {
            break;
        };
        offset = offset
            .checked_add(u64::try_from(record.observed_bytes).map_err(|_| {
                CaptureError::SystemInvariant("Codex prompt-history record length exceeds u64")
            })?)
            .ok_or(CaptureError::SystemInvariant(
                "Codex prompt-history source offset overflowed",
            ))?;
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Codex prompt-history line number exceeds platform limits",
            ))?;
        if record.observed_bytes > MAX_PROVIDER_JSONL_LINE_BYTES {
            reject(
                &mut failures,
                &mut rejected_records,
                line_number,
                format!(
                    "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit (observed {} bytes)",
                    record.observed_bytes
                ),
            )?;
            ordinal = next_ordinal(ordinal)?;
            continue;
        }
        if record.bytes.iter().all(u8::is_ascii_whitespace) {
            ignored_records =
                ignored_records
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex prompt-history ignored count overflowed",
                    ))?;
            ordinal = next_ordinal(ordinal)?;
            continue;
        }
        let parsed = match serde_json::from_slice::<PromptLine>(&record.bytes) {
            Ok(parsed) => parsed,
            Err(error) => {
                reject(
                    &mut failures,
                    &mut rejected_records,
                    line_number,
                    format!(
                        "malformed Codex prompt-history JSON{}: {error}",
                        if record.terminated { "" } else { " at EOF" }
                    ),
                )?;
                ordinal = next_ordinal(ordinal)?;
                continue;
            }
        };
        if parsed.session_id.trim().is_empty() {
            reject(
                &mut failures,
                &mut rejected_records,
                line_number,
                "codex history line has empty session_id".to_owned(),
            )?;
            ordinal = next_ordinal(ordinal)?;
            continue;
        }
        let Some(occurred_at) = DateTime::from_timestamp(parsed.ts, 0) else {
            reject(
                &mut failures,
                &mut rejected_records,
                line_number,
                format!(
                    "codex history line has invalid unix timestamp {}",
                    parsed.ts
                ),
            )?;
            ordinal = next_ordinal(ordinal)?;
            continue;
        };
        let event = prompt_event(ordinal, line_number, occurred_at, parsed.text);
        let event_hash = compute_payload_hash(&event.payload)?;
        let conservative_bytes = serde_json::to_vec(&event)?.len().saturating_add(2048);
        if conservative_bytes > MAX_PAGE_BYTES {
            reject(
                &mut failures,
                &mut rejected_records,
                line_number,
                "Codex prompt-history event exceeds the bounded Core page".to_owned(),
            )?;
            ordinal = next_ordinal(ordinal)?;
            continue;
        }
        if !rows.is_empty()
            && retained_bytes
                .saturating_add(conservative_bytes)
                .saturating_add(PAGE_OVERHEAD_BYTES)
                > MAX_PAGE_BYTES
        {
            reader.seek(SeekFrom::Start(record_start))?;
            prefix = prefix_before;
            offset = record_start;
            break;
        }
        let session_hash: [u8; 32] = Sha256::digest(parsed.session_id.as_bytes()).into();
        if last_session_hash != Some(session_hash) {
            session_runs = session_runs
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Codex prompt-history session-run count overflowed",
                ))?;
            last_session_hash = Some(session_hash);
        }
        accepted_events = accepted_events
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Codex prompt-history accepted count overflowed",
            ))?;
        retained_bytes = retained_bytes.saturating_add(conservative_bytes);
        rows.push(PromptRow {
            session_id: parsed.session_id,
            event,
            event_hash,
        });
        ordinal = next_ordinal(ordinal)?;
    }
    let terminal = reader.fill_buf()?.is_empty();
    if rows.is_empty() && failures.is_empty() && ordinal == start_ordinal && !terminal {
        return Err(CaptureError::SystemInvariant(
            "Codex prompt-history page reader made no progress",
        ));
    }
    let prefix_sha256: [u8; 32] = prefix.finalize().into();
    if terminal && prefix_sha256 != revision_bytes(&digest.revision)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(PreparedPage {
        rows,
        failures,
        retained_bytes,
        next_offset: offset,
        next_ordinal: ordinal,
        prefix_sha256,
        accepted_events,
        session_runs,
        rejected_records,
        ignored_records,
        last_session_hash,
        terminal,
    })
}

fn publish_core_page(
    store: &Store,
    guard: &ctx_history_store::EventSearchBulkGuard,
    authority: &SourceAuthority,
    digest: &SourceDigest,
    options: &CodexHistoryImportOptions,
    cursor: &mut PromptHistoryCursor,
    page: PreparedPage,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let expected = store
        .get_sync_cursor(None, &options.machine_id, &authority.cursor_stream)?
        .map(|value| value.cursor);
    let next_phase = if page.terminal {
        CursorPhase::Retiring {
            after: None,
            missing: false,
        }
    } else {
        CursorPhase::Core {
            next_offset: page.next_offset,
            next_ordinal: page.next_ordinal,
            prefix_sha256: page.prefix_sha256,
        }
    };
    let mut next_cursor = cursor.clone();
    next_cursor.phase = next_phase;
    next_cursor.accepted_events = page.accepted_events;
    next_cursor.session_runs = page.session_runs;
    next_cursor.rejected_records = page.rejected_records;
    next_cursor.ignored_records = page.ignored_records;
    next_cursor.last_session_hash = page.last_session_hash;
    let next = sync_cursor(
        options,
        authority.cursor_stream.clone(),
        next_cursor.encode()?,
    );
    let transition = NativePathCursorTransition::new(expected.clone(), next);
    let publication_id = publication_id(cursor, &transition, "core");
    let retained_bytes = page
        .retained_bytes
        .saturating_add(PAGE_OVERHEAD_BYTES)
        .min(NATIVE_INGESTION_PAGE_MAX_BYTES);
    let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let classification =
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;
    if classification == NativePathCursorSetClassification::AllExpected {
        let locator = locator_observation(authority, cursor, options.imported_at);
        let resolution = group.reconcile_provider_source_locator(&locator)?;
        if resolution.canonical_source_identity != cursor.canonical_source_identity {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history relocation requires a fresh explicit route".to_owned(),
            ));
        }
        let source = capture_source(
            authority,
            cursor,
            options,
            page.rows.iter().map(|row| row.event.occurred_at).min(),
        );
        group.upsert_capture_source(&source)?;
        group.bind_capture_source_provider_route(
            cursor.capture_source_id,
            &resolution.route_binding(),
        )?;

        let mut sessions = BTreeMap::<String, DateTime<Utc>>::new();
        for row in &page.rows {
            sessions
                .entry(row.session_id.clone())
                .and_modify(|started| *started = (*started).min(row.event.occurred_at))
                .or_insert(row.event.occurred_at);
        }
        let new_session_count = sessions
            .keys()
            .map(|native_id| {
                let session_id =
                    provider_source_session_uuid(&cursor.canonical_source_identity, native_id);
                match store.get_session(session_id) {
                    Ok(_) => Ok(0_usize),
                    Err(StoreError::NotFound(_)) => Ok(1_usize),
                    Err(error) => Err(CaptureError::from(error)),
                }
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .sum::<usize>();
        let mut retained = NativePathRetainedSourceEntities {
            capture_source_ids: vec![cursor.capture_source_id],
            ..NativePathRetainedSourceEntities::default()
        };
        for (native_id, started_at) in sessions {
            let session_id =
                provider_source_session_uuid(&cursor.canonical_source_identity, &native_id);
            group.upsert_session(&session(
                cursor.capture_source_id,
                session_id,
                &native_id,
                started_at,
                options,
            ))?;
            retained.session_ids.push(session_id);
        }
        let mut imported_events = 0_usize;
        for row in &page.rows {
            let session_id =
                provider_source_session_uuid(&cursor.canonical_source_identity, &row.session_id);
            let ordinal = row.event.provider_event_index;
            let line_number = ordinal
                .checked_add(1)
                .and_then(|line| usize::try_from(line).ok())
                .ok_or(CaptureError::SystemInvariant(
                    "Codex prompt-history line number overflowed",
                ))?;
            // Keep the released provider-scoped event identity exactly stable.
            let event_identity_source_id = provider_scoped_source_uuid(
                CaptureProvider::Codex,
                &row.session_id,
                SOURCE_FORMAT,
                Some(&authority.raw_source_path),
            );
            let mut identity = provider_source_event_import_identity(
                event_identity_source_id,
                ordinal,
                &row.event_hash,
            );
            identity = avoid_provider_source_event_seq_collision(
                store,
                identity,
                event_identity_source_id,
                ordinal,
                ordinal,
            )?;
            let (event, run) = codex_canonical_event(
                &row.session_id,
                SOURCE_FORMAT,
                ProviderSourceTrust::ProviderExport,
                options.imported_at,
                options.history_record_id,
                cursor.capture_source_id,
                session_id,
                line_number,
                &row.event,
                &row.event_hash,
                ProviderEventHashAuthority::NormalizedPayloadFallback,
                &identity,
            )?;
            if run.is_some() {
                return Err(CaptureError::SystemInvariant(
                    "Codex prompt-history user prompt unexpectedly produced a run",
                ));
            }
            if group.reconcile_provider_event(
                &event,
                ProviderEventHashAuthority::NormalizedPayloadFallback,
            )? {
                imported_events = imported_events.saturating_add(1);
            }
            retained.event_ids.push(event.id);
        }
        group.stage_source_generation_page(&generation_key(authority, cursor), &retained)?;
        group.prepare_journal_checkpoint()?;
        if !digest.observation.revalidate(&authority.physical_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        group.publish_cursor_set()?;
        summary.imported_events = summary.imported_events.saturating_add(imported_events);
        summary.imported = summary.imported.saturating_add(imported_events);
        summary.accepted_content_records = summary
            .accepted_content_records
            .saturating_add(page.rows.len());
        summary.imported_sessions = summary.imported_sessions.saturating_add(new_session_count);
        summary.imported = summary.imported.saturating_add(new_session_count);
        for failure in page.failures {
            summary.record_failure(failure);
        }
    } else if !digest.observation.revalidate(&authority.physical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.commit()?;
    *cursor = next_cursor;
    Ok(())
}

fn publish_retirement_page(
    store: &Store,
    guard: &ctx_history_store::EventSearchBulkGuard,
    authority: &SourceAuthority,
    options: &CodexHistoryImportOptions,
    cursor: &PromptHistoryCursor,
    after: Option<&RetirementFrontier>,
) -> Result<ctx_history_store::NativePathSourceRetirementPage> {
    let expected = store
        .get_sync_cursor(None, &options.machine_id, &authority.cursor_stream)?
        .ok_or(CaptureError::SystemInvariant(
            "Codex prompt-history retirement lost its cursor",
        ))?;
    let next = sync_cursor(options, authority.cursor_stream.clone(), cursor.encode()?);
    let transition = NativePathCursorTransition::new(Some(expected.cursor.clone()), next);
    let publication_id = publication_id(cursor, &transition, "retire-page");
    let accounting = NativePathGroupAccounting::new(1, 1, PAGE_OVERHEAD_BYTES)?;
    let admission = store.admit_event_search_bulk_group(guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let classification =
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;
    let store_after = after.map(RetirementFrontier::to_store);
    let page = if classification == NativePathCursorSetClassification::AllExpected {
        let page = group.retire_source_generation_page(
            &generation_key(authority, cursor),
            store_after.as_ref(),
            RETIREMENT_PAGE_LIMIT,
            options.imported_at.timestamp_millis(),
        )?;
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
        page
    } else {
        group.commit()?;
        return publish_retirement_page(store, guard, authority, options, cursor, after);
    };
    group.commit()?;
    Ok(page)
}

#[allow(clippy::too_many_arguments)]
fn publish_cursor_advance(
    store: &Store,
    guard: &ctx_history_store::EventSearchBulkGuard,
    authority: &SourceAuthority,
    options: &CodexHistoryImportOptions,
    cursor: &mut PromptHistoryCursor,
    phase: CursorPhase,
    retire_route: bool,
) -> Result<()> {
    let expected = store
        .get_sync_cursor(None, &options.machine_id, &authority.cursor_stream)?
        .ok_or(CaptureError::SystemInvariant(
            "Codex prompt-history retirement lost its cursor",
        ))?;
    let mut next_cursor = cursor.clone();
    next_cursor.phase = phase;
    let next = sync_cursor(
        options,
        authority.cursor_stream.clone(),
        next_cursor.encode()?,
    );
    let transition = NativePathCursorTransition::new(Some(expected.cursor.clone()), next);
    let publication_id = publication_id(cursor, &transition, "retire");
    let accounting = NativePathGroupAccounting::new(1, 1, PAGE_OVERHEAD_BYTES)?;
    let admission = store.admit_event_search_bulk_group(guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let classification =
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;
    if classification == NativePathCursorSetClassification::AllExpected {
        if retire_route {
            group.retire_provider_source_route(&route_retirement(
                authority,
                cursor,
                options.imported_at,
            ))?;
        }
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    *cursor = next_cursor;
    Ok(())
}

fn retire_disappeared_source(
    store: &Store,
    authority: &SourceAuthority,
    stored: StoredCursor,
    options: &CodexHistoryImportOptions,
) -> Result<ProviderImportSummary> {
    let StoredCursor::Native { mut cursor } = stored else {
        return Err(CaptureError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Codex prompt-history source does not exist: {}",
                authority.physical_path.display()
            ),
        )));
    };
    cursor.validate_route(authority)?;
    if matches!(cursor.phase, CursorPhase::Complete { missing: true }) {
        return Ok(replay_summary(&cursor));
    }
    ensure_active_journal(store)?;
    let guard = store.begin_event_search_bulk_mode()?;
    let result = (|| -> Result<ProviderImportSummary> {
        let mut summary = ProviderImportSummary::default();
        let digest = SourceDigest {
            observation: cursor.observation.clone(),
            revision: cursor.source_revision.clone(),
            prefix_at_prior_len: None,
        };

        if matches!(cursor.phase, CursorPhase::Core { .. }) {
            publish_cursor_advance(
                store,
                &guard,
                authority,
                options,
                &mut cursor,
                CursorPhase::Retiring {
                    after: None,
                    missing: false,
                },
                false,
            )?;
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }

        if matches!(cursor.phase, CursorPhase::Retiring { missing: false, .. }) {
            summary.merge_from(import_core_and_retire(
                store,
                &guard,
                authority,
                &digest,
                options,
                &mut cursor,
            )?);
            if !matches!(cursor.phase, CursorPhase::Complete { missing: false }) {
                summary.work_remaining = true;
                return Ok(summary);
            }
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }

        if matches!(cursor.phase, CursorPhase::Complete { missing: false }) {
            cursor.generation =
                cursor
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex prompt-history generation exhausted",
                    ))?;
            cursor.generation_id = generation_id(cursor.generation, &cursor.source_revision, true);
            cursor.lifecycle = Lifecycle::Replacement;
            cursor.phase = CursorPhase::Retiring {
                after: None,
                missing: true,
            };
            seed_missing_generation(store, &guard, authority, options, &cursor)?;
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }

        summary.merge_from(import_core_and_retire(
            store,
            &guard,
            authority,
            &digest,
            options,
            &mut cursor,
        )?);
        Ok(summary)
    })();
    let finish = store.finish_event_search_bulk_mode(&guard);
    let mut summary = result?;
    finish?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn seed_missing_generation(
    store: &Store,
    guard: &ctx_history_store::EventSearchBulkGuard,
    authority: &SourceAuthority,
    options: &CodexHistoryImportOptions,
    cursor: &PromptHistoryCursor,
) -> Result<()> {
    let expected = store
        .get_sync_cursor(None, &options.machine_id, &authority.cursor_stream)?
        .ok_or(CaptureError::SystemInvariant(
            "Codex prompt-history missing source lost its cursor",
        ))?;
    let next = sync_cursor(options, authority.cursor_stream.clone(), cursor.encode()?);
    let transition = NativePathCursorTransition::new(Some(expected.cursor.clone()), next);
    let publication_id = publication_id(cursor, &transition, "missing");
    let accounting = NativePathGroupAccounting::new(1, 1, PAGE_OVERHEAD_BYTES)?;
    let admission = store.admit_event_search_bulk_group(guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let classification =
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;
    if classification == NativePathCursorSetClassification::AllExpected {
        group.stage_source_generation_page(
            &generation_key(authority, cursor),
            &NativePathRetainedSourceEntities {
                capture_source_ids: vec![cursor.capture_source_id],
                ..NativePathRetainedSourceEntities::default()
            },
        )?;
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    Ok(())
}

fn plan_cursor(
    authority: &SourceAuthority,
    stored: StoredCursor,
    digest: &SourceDigest,
) -> Result<PromptHistoryCursor> {
    let (prior, migration) = match stored {
        StoredCursor::None => (None, false),
        StoredCursor::Released => (None, true),
        StoredCursor::Native { cursor } => (Some(cursor), false),
    };
    if let Some(prior) = &prior {
        prior.validate_route(authority)?;
    }
    let lifecycle = match prior.as_ref() {
        None if migration => Lifecycle::Migration,
        None => Lifecycle::Fresh,
        Some(previous) if !previous.observation.same_file(&digest.observation) => {
            Lifecycle::Replacement
        }
        Some(previous) if digest.observation.len < previous.observation.len => {
            Lifecycle::Truncation
        }
        Some(previous)
            if digest.observation.len > previous.observation.len
                && revision_inventory_authority(&digest.revision)
                    == revision_inventory_authority(&previous.source_revision)
                && digest.prefix_at_prior_len
                    == Some(revision_bytes(&previous.source_revision)?) =>
        {
            Lifecycle::Append
        }
        Some(_) => Lifecycle::Rewrite,
    };
    let resume = prior.as_ref().is_some_and(|previous| {
        previous.source_revision == digest.revision
            && matches!(
                previous.phase,
                CursorPhase::Core { .. } | CursorPhase::Retiring { missing: false, .. }
            )
    });
    if resume {
        return prior.ok_or(CaptureError::SystemInvariant(
            "Codex prompt-history resume cursor disappeared",
        ));
    }
    let generation = match prior.as_ref() {
        Some(previous) => {
            previous
                .generation
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Codex prompt-history generation exhausted",
                ))?
        }
        None => 0,
    };
    let canonical_source_identity = prior
        .as_ref()
        .map(|previous| previous.canonical_source_identity.clone())
        .unwrap_or_else(|| authority.proposed_source_identity.clone());
    let capture_source_id = authority.shared_source_id(&canonical_source_identity);
    Ok(PromptHistoryCursor {
        version: CURSOR_VERSION,
        parser_revision: PARSER_REVISION.to_owned(),
        policy_revision: POLICY_REVISION.to_owned(),
        route_identity: authority.route_identity.clone(),
        locator_identity: authority.locator_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        canonical_source_identity,
        capture_source_id,
        source_revision: digest.revision.clone(),
        generation,
        generation_id: generation_id(generation, &digest.revision, false),
        observation: digest.observation.clone(),
        lifecycle,
        accepted_events: 0,
        session_runs: 0,
        rejected_records: 0,
        ignored_records: 0,
        last_session_hash: None,
        phase: CursorPhase::Core {
            next_offset: 0,
            next_ordinal: 0,
            prefix_sha256: Sha256::digest([]).into(),
        },
    })
}

fn load_cursor(store: &Store, machine_id: &str, stream: &str) -> Result<StoredCursor> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(StoredCursor::None);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        return Ok(StoredCursor::Native {
            cursor: PromptHistoryCursor::decode(committed.provider_cursor())?,
        });
    }
    if crate::provider::importer::CertifiedProviderCursor::decode_if_certified(&stored.cursor)?
        .is_some()
    {
        return Ok(StoredCursor::Released);
    }
    Err(CaptureError::InvalidPayload(
        "Codex prompt-history cursor is neither NativePath nor a released migration cursor"
            .to_owned(),
    ))
}

fn digest_source(
    path: &Path,
    prior_len: Option<u64>,
    inventory_observation_token: Option<&str>,
) -> Result<SourceDigest> {
    let observation = FileObservation::read(path)?;
    let file = File::open(path)?;
    if FileObservation::from_metadata(&file.metadata()?)? != observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut reader = BufReader::new(file);
    let mut full = Sha256::new();
    let mut prefix = Sha256::new();
    let mut read = 0_u64;
    let mut bytes = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        full.update(&bytes[..count]);
        if let Some(target) = prior_len {
            let remaining = target.saturating_sub(read);
            let take = count.min(usize::try_from(remaining).unwrap_or(usize::MAX));
            prefix.update(&bytes[..take]);
        }
        read = read
            .checked_add(u64::try_from(count).map_err(|_| {
                CaptureError::SystemInvariant("Codex prompt-history source length exceeds u64")
            })?)
            .ok_or(CaptureError::SystemInvariant(
                "Codex prompt-history source length overflowed",
            ))?;
    }
    if read != observation.len || !observation.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let hash: [u8; 32] = full.finalize().into();
    Ok(SourceDigest {
        observation,
        revision: revision_string(&hash, inventory_observation_token),
        prefix_at_prior_len: prior_len.map(|_| prefix.finalize().into()),
    })
}

fn read_record(reader: &mut BufReader<File>, hasher: &mut Sha256) -> Result<Option<RawRecord>> {
    let mut bytes = Vec::new();
    let mut observed = 0_usize;
    let mut saw_any = false;
    let mut terminated = false;
    while !terminated {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        saw_any = true;
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        hasher.update(chunk);
        observed = observed.saturating_add(chunk.len());
        if bytes.len() <= MAX_PROVIDER_JSONL_LINE_BYTES {
            let remaining = MAX_PROVIDER_JSONL_LINE_BYTES
                .saturating_add(1)
                .saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }
        terminated = chunk.last() == Some(&b'\n');
        reader.consume(take);
    }
    if !saw_any {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    Ok(Some(RawRecord {
        bytes,
        observed_bytes: observed,
        terminated,
    }))
}

fn hash_prefix_and_seek(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    target: u64,
) -> Result<()> {
    let mut remaining = target;
    let mut bytes = [0_u8; 64 * 1024];
    while remaining > 0 {
        let take = bytes
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let count = reader.read(&mut bytes[..take])?;
        if count == 0 {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history cursor exceeds its source".to_owned(),
            ));
        }
        hasher.update(&bytes[..count]);
        remaining = remaining.saturating_sub(u64::try_from(count).unwrap_or(u64::MAX));
    }
    Ok(())
}

fn prompt_event(
    ordinal: u64,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    text: String,
) -> CodexNativeEvent {
    CodexNativeEvent {
        provider_event_index: ordinal,
        provider_event_hash: None,
        cursor: Some(format!("line:{line_number}")),
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at,
        fidelity: Fidelity::SummaryOnly,
        idempotency_key: Some(format!("provider-event:codex:prompt-history:{ordinal}")),
        artifacts: Vec::new(),
        payload: json!({
            "text": text,
            "source_format": SOURCE_FORMAT,
            "nativepath_schema": 1,
        }),
        metadata: json!({
            "source": "codex_history",
            "source_format": SOURCE_FORMAT,
            "source_fidelity": "prompt_log_only",
        }),
    }
}

fn capture_source(
    authority: &SourceAuthority,
    cursor: &PromptHistoryCursor,
    options: &CodexHistoryImportOptions,
    started_at: Option<DateTime<Utc>>,
) -> CaptureSource {
    CaptureSource {
        id: cursor.capture_source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: options.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(authority.raw_source_path.clone()),
            source_format: Some(SOURCE_FORMAT.to_owned()),
            source_root: Some(authority.raw_source_path.clone()),
            source_identity: Some(cursor.canonical_source_identity.clone()),
            external_session_id: None,
        },
        started_at: started_at.unwrap_or(options.imported_at),
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::SummaryOnly,
            json!({
                "source_format": SOURCE_FORMAT,
                "source_trust": "provider_export",
                "imported_at": options.imported_at,
                "source_identity": cursor.canonical_source_identity,
                "source_root": authority.raw_source_path,
                "source_revision": cursor.source_revision,
                "nativepath_publication": "codex-prompt-history-v1",
            }),
        ),
    }
}

fn session(
    source_id: Uuid,
    id: Uuid,
    native_id: &str,
    started_at: DateTime<Utc>,
    options: &CodexHistoryImportOptions,
) -> Session {
    Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Codex,
        external_session_id: Some(native_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at: None,
        timestamps: timestamps(options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::SummaryOnly,
            json!({
                "provider_session_id": native_id,
                "source_format": SOURCE_FORMAT,
                "source_trust": "provider_export",
                "imported_at": options.imported_at,
                "session_idempotency_key": format!("provider-session:codex:{native_id}"),
                "metadata": {
                    "source_format": SOURCE_FORMAT,
                    "source_fidelity": "prompt_log_only",
                    "limitations": [
                        "user prompts only",
                        "no assistant responses",
                        "no tool calls",
                        "no command output",
                        "no child session relationships"
                    ],
                },
            }),
        ),
    }
}

fn locator_observation(
    authority: &SourceAuthority,
    cursor: &PromptHistoryCursor,
    observed_at: DateTime<Utc>,
) -> ProviderSourceLocatorObservation {
    ProviderSourceLocatorObservation {
        provider: CaptureProvider::Codex,
        source_format: SOURCE_FORMAT.to_owned(),
        machine_id: authority.machine_id.clone(),
        locator_identity: authority.locator_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        proposed_source_identity: cursor.canonical_source_identity.clone(),
        raw_source_path: Some(authority.physical_path.display().to_string()),
        source_revision: cursor.source_revision.clone(),
        observed_at_ms: observed_at.timestamp_millis(),
    }
}

fn generation_key(
    authority: &SourceAuthority,
    cursor: &PromptHistoryCursor,
) -> NativePathSourceGenerationKey {
    NativePathSourceGenerationKey {
        provider: CaptureProvider::Codex,
        source_format: SOURCE_FORMAT.to_owned(),
        machine_id: authority.machine_id.clone(),
        canonical_source_identity: cursor.canonical_source_identity.clone(),
        locator_identity: authority.locator_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        source_revision: cursor.source_revision.clone(),
        generation_id: cursor.generation_id.clone(),
    }
}

fn route_retirement(
    authority: &SourceAuthority,
    cursor: &PromptHistoryCursor,
    retired_at: DateTime<Utc>,
) -> ProviderSourceRouteRetirement {
    ProviderSourceRouteRetirement {
        provider: CaptureProvider::Codex,
        source_format: SOURCE_FORMAT.to_owned(),
        machine_id: authority.machine_id.clone(),
        locator_identity: authority.locator_identity.clone(),
        cursor_stream: authority.cursor_stream.clone(),
        expected_canonical_source_identity: cursor.canonical_source_identity.clone(),
        expected_source_revision: cursor.source_revision.clone(),
        retired_at_ms: retired_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::SourceMissing,
    }
}

fn sync_cursor(options: &CodexHistoryImportOptions, stream: String, cursor: String) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!("provider-cursor:codex:{}:{stream}", options.machine_id),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream,
        cursor,
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    }
}

fn replay_summary(cursor: &PromptHistoryCursor) -> ProviderImportSummary {
    let skipped_events = usize::try_from(cursor.accepted_events).unwrap_or(usize::MAX);
    let skipped_sessions = usize::try_from(cursor.session_runs).unwrap_or(usize::MAX);
    ProviderImportSummary {
        skipped: skipped_events.saturating_add(skipped_sessions),
        failed: usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX),
        skipped_sessions,
        skipped_events,
        accepted_content_records: skipped_events,
        ..ProviderImportSummary::default()
    }
}

fn replay_no_outputs(
    store: &Store,
    authority: &SourceAuthority,
    options: &CodexHistoryImportOptions,
) -> Result<()> {
    let StoredCursor::Native { cursor } =
        load_cursor(store, &options.machine_id, &authority.cursor_stream)?
    else {
        return Err(CaptureError::InvalidPayload(
            "Codex prompt-history Pro replay requires committed NativePath Core".to_owned(),
        ));
    };
    if !cursor.terminal() {
        return Err(CaptureError::InvalidPayload(
            "Codex prompt-history Pro replay requires terminal Core authority".to_owned(),
        ));
    }
    let digest = digest_source(
        &authority.physical_path,
        None,
        options.inventory_observation_token.as_deref(),
    )?;
    if digest.revision != cursor.source_revision {
        return Err(CaptureError::InvalidPayload(
            "Codex prompt-history Pro replay source changed after Core commit".to_owned(),
        ));
    }
    if let ImportProfile::ProReplayOnly(sink) = &options.import_profile {
        replay_empty_output_or_mark_behind(
            store,
            authority,
            &digest.revision,
            &cursor,
            sink.as_ref(),
        );
    }
    Ok(())
}

fn replay_empty_output_or_mark_behind(
    store: &Store,
    authority: &SourceAuthority,
    revision: &str,
    cursor: &PromptHistoryCursor,
    sink: &dyn ProOutputSink,
) {
    if let Err(error) = replay_empty_output(store, authority, revision, cursor, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "codex_prompt_history_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_empty_output(
    _store: &Store,
    authority: &SourceAuthority,
    revision: &str,
    cursor: &PromptHistoryCursor,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let source = OutputSourceIdentity {
        provider: CaptureProvider::Codex.as_str().to_owned(),
        namespace_id: authority.cursor_stream.clone(),
        source_id: cursor.canonical_source_identity.clone(),
    };
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let frontier = output_frontier(revision)?;
    if progress.as_ref().is_some_and(|progress| {
        progress.terminal
            && progress.parser_revision == OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress.observed_revision == revision
            && progress.cursor.as_ref().is_some_and(|committed| {
                committed.version == frontier.version && committed.payload == frontier.bytes
            })
    }) {
        return Ok(());
    }
    let state = output_state(progress, revision, sink.materializer_revision())?;
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source,
        source_epoch: state.source_epoch,
        observed_revision: revision.to_owned(),
        parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_frontier,
        observations: Vec::new(),
    };
    let page = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(
            CaptureProvider::Codex.as_str(),
            &cursor.canonical_source_identity,
        ),
        frontier.clone(),
        frontier,
        true,
        NativePageAccounting {
            logical_units: 1,
            conservative_serialized_bytes: 1024,
        },
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if process_pro_replay_only(page, sink).is_err() {
        // The NativePath output coordinator already marked the sink behind.
        return Ok(());
    }
    Ok(())
}

struct OutputState {
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

fn output_state(
    progress: Option<ProOutputProgress>,
    revision: &str,
    materializer_revision: &str,
) -> Result<OutputState> {
    let Some(progress) = progress else {
        return Ok(OutputState {
            source_epoch: 0,
            expected_source_epoch: None,
            expected_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
        });
    };
    let can_resume = progress.parser_revision == OUTPUT_PARSER_REVISION
        && progress.materializer_revision == materializer_revision
        && progress.observed_revision == revision;
    let expected_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(OutputState {
        source_epoch: if can_resume {
            progress.source_epoch
        } else {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Codex prompt-history output epoch exhausted",
                ))?
        },
        expected_source_epoch: Some(progress.source_epoch),
        expected_frontier,
        disposition: if can_resume {
            ProOutputSourceDisposition::AppendOrResume
        } else {
            ProOutputSourceDisposition::Rewrite
        },
    })
}

fn output_frontier(revision: &str) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&json!({
            "version": OUTPUT_FRONTIER_VERSION,
            "source_revision": revision,
            "next_output": 0,
        }))?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn reject(
    failures: &mut Vec<ProviderImportFailure>,
    count: &mut u64,
    line: usize,
    error: String,
) -> Result<()> {
    *count = count.checked_add(1).ok_or(CaptureError::SystemInvariant(
        "Codex prompt-history rejection count overflowed",
    ))?;
    if failures.len() < crate::summaries::MAX_RETAINED_PROVIDER_FAILURES {
        failures.push(ProviderImportFailure { line, error });
    }
    Ok(())
}

fn next_ordinal(current: u64) -> Result<u64> {
    current.checked_add(1).ok_or(CaptureError::SystemInvariant(
        "Codex prompt-history ordinal overflowed",
    ))
}

fn generation_id(generation: u64, revision: &str, missing: bool) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/codex-prompt-history/generation/v1\0");
    digest.update(generation.to_be_bytes());
    digest.update(revision.as_bytes());
    digest.update([u8::from(missing)]);
    format!("codex-prompt-history-generation-v1:{:x}", digest.finalize())
}

fn publication_id(
    cursor: &PromptHistoryCursor,
    transition: &NativePathCursorTransition,
    phase: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/codex-prompt-history/publication/v1\0");
    digest.update(cursor.route_identity.as_bytes());
    digest.update(cursor.generation_id.as_bytes());
    digest.update(phase.as_bytes());
    if let Some(expected) = transition.expected_cursor() {
        digest.update(expected.as_bytes());
    }
    digest.update(transition.next().cursor.as_bytes());
    format!("codex-prompt-history-nativepath-v1:{:x}", digest.finalize())
}

fn revision_string(hash: &[u8; 32], inventory_observation_token: Option<&str>) -> String {
    let mut revision = format!("codex-prompt-history-sha256-v1:{}", hex(hash));
    if let Some(token) = inventory_observation_token {
        let inventory_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        revision.push_str(":inventory-sha256:");
        revision.push_str(&hex(&inventory_hash));
    }
    revision
}

fn revision_bytes(revision: &str) -> Result<[u8; 32]> {
    let Some(encoded) = revision.strip_prefix("codex-prompt-history-sha256-v1:") else {
        return Err(CaptureError::InvalidPayload(
            "Codex prompt-history source revision is malformed".to_owned(),
        ));
    };
    let mut parts = encoded.split(':');
    let file_hash = parts.next().unwrap_or_default();
    match (parts.next(), parts.next(), parts.next()) {
        (None, None, None) => {}
        (Some("inventory-sha256"), Some(inventory_hash), None) => {
            decode_hex_hash(inventory_hash)?;
        }
        _ => {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history source revision is malformed".to_owned(),
            ));
        }
    }
    decode_hex_hash(file_hash)
}

fn decode_hex_hash(encoded: &str) -> Result<[u8; 32]> {
    if encoded.len() != 64 {
        return Err(CaptureError::InvalidPayload(
            "Codex prompt-history source revision is malformed".to_owned(),
        ));
    }
    let mut bytes = [0_u8; 32];
    for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Ok(bytes)
}

fn revision_inventory_authority(revision: &str) -> Option<&str> {
    revision
        .split_once(":inventory-sha256:")
        .map(|(_, authority)| authority)
}

fn hex_value(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(CaptureError::InvalidPayload(
            "Codex prompt-history source revision is malformed".to_owned(),
        )),
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn ensure_active_journal(store: &Store) -> Result<()> {
    match store.projection_journal_snapshot(None) {
        Ok(_) => Ok(()),
        Err(StoreError::ProjectionJournalInactive) => {
            store.activate_projection_journal(ctx_pro_host_protocol::PROTOCOL_FINGERPRINT)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}
