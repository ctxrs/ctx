#[cfg(test)]
use std::fs;
use std::{
    collections::BTreeMap,
    fs::{File, Metadata},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
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
    common::io::{open_provider_source_file, OpenedProviderSourceFile},
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

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PromptHistoryIoMetrics {
    opens: usize,
    bytes_read: u64,
}

#[cfg(test)]
std::thread_local! {
    static PROMPT_HISTORY_IO_METRICS: std::cell::Cell<PromptHistoryIoMetrics> =
        const { std::cell::Cell::new(PromptHistoryIoMetrics {
            opens: 0,
            bytes_read: 0,
        }) };
}

#[inline]
fn open_prompt_history_source(source: &OpenedProviderSourceFile) -> Result<File> {
    let mut file = source.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    #[cfg(test)]
    PROMPT_HISTORY_IO_METRICS.with(|metrics| {
        let current = metrics.get();
        metrics.set(PromptHistoryIoMetrics {
            opens: current.opens.saturating_add(1),
            ..current
        });
    });
    Ok(file)
}

#[inline(always)]
fn note_prompt_history_bytes_read(bytes: usize) {
    #[cfg(test)]
    PROMPT_HISTORY_IO_METRICS.with(|metrics| {
        let current = metrics.get();
        metrics.set(PromptHistoryIoMetrics {
            bytes_read: current
                .bytes_read
                .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX)),
            ..current
        });
    });
    #[cfg(not(test))]
    let _ = bytes;
}

#[cfg(test)]
fn reset_prompt_history_io_metrics() {
    PROMPT_HISTORY_IO_METRICS.with(|metrics| metrics.set(PromptHistoryIoMetrics::default()));
}

#[cfg(test)]
fn prompt_history_io_metrics() -> PromptHistoryIoMetrics {
    PROMPT_HISTORY_IO_METRICS.with(std::cell::Cell::get)
}

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

    fn revalidate(&self, source: &OpenedProviderSourceFile) -> Result<bool> {
        if source.revalidate().is_err() {
            return Ok(false);
        }
        Ok(Self::from_metadata(&source.file().metadata()?)? == *self)
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
    #[serde(default)]
    event_identity_raw_source_path: Option<String>,
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
            || cursor
                .event_identity_raw_source_path
                .as_deref()
                .is_some_and(str::is_empty)
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
            || self.canonical_source_identity != authority.canonical_source_identity
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

// The native cursor is intentionally inline so cursor ownership remains explicit.
#[allow(clippy::large_enum_variant)]
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
    canonical_source_identity: String,
    opened: Option<Arc<OpenedProviderSourceFile>>,
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
        let canonical_source_identity = proposed_source_identity.clone();
        let authority_path = std::path::absolute(path)?;
        let opened = match open_provider_source_file(&authority_path) {
            Ok(opened) => Some(Arc::new(opened)),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotADirectory => {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "linked provider transcript path components are rejected",
                });
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            physical_path: path.to_path_buf(),
            machine_id: machine_id.to_owned(),
            raw_source_path,
            route_identity,
            locator_identity,
            cursor_stream,
            proposed_source_identity,
            canonical_source_identity,
            opened,
        })
    }

    fn admit_canonical_source_identity(&mut self, canonical_source_identity: String) {
        self.canonical_source_identity = canonical_source_identity;
    }

    fn shared_source_id(&self, canonical_source_identity: &str) -> Uuid {
        stable_capture_uuid(
            &format!("codex-prompt-history:{canonical_source_identity}"),
            "native-source",
        )
    }

    fn opened(&self) -> Result<&OpenedProviderSourceFile> {
        self.opened.as_deref().ok_or_else(|| {
            CaptureError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Codex prompt-history source is unavailable",
            ))
        })
    }

    fn is_missing(&self) -> bool {
        self.opened.is_none()
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
    outcome: PromptHistoryPageOutcome,
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
    let mut authority = SourceAuthority::new(path, logical_path, &options.machine_id)?;
    let mut stored = load_cursor(store, &options.machine_id, &authority.cursor_stream)?;
    if let StoredCursor::Native { cursor } = &stored {
        authority.admit_canonical_source_identity(cursor.canonical_source_identity.clone());
        cursor.validate_route(&authority)?;
    }

    if options.import_profile.is_replay_only() {
        replay_no_outputs(store, &authority, &options)?;
        return Ok(ProviderImportSummary::default());
    }
    if authority.is_missing() {
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
        StoredCursor::Native { cursor } => Some(cursor),
        _ => None,
    };
    let had_durable_authority = !matches!(&stored, StoredCursor::None);
    let digest = digest_source(
        &authority,
        prior_native.map(|cursor| cursor.observation.len),
        options.inventory_observation_token.as_deref(),
    )?;
    let admitted = store.plan_provider_source_locator(&locator_observation_for_revision(
        &authority,
        &authority.proposed_source_identity,
        &digest.revision,
        options.imported_at,
    ))?;
    authority.admit_canonical_source_identity(admitted.canonical_source_identity);
    if let Some(cursor) = prior_native {
        cursor.validate_route(&authority)?;
    }
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

    let event_identity_raw_source_path =
        event_identity_raw_source_path(store, &authority, &authority.canonical_source_identity)?;
    let mut cursor = plan_cursor(&authority, stored, &digest, event_identity_raw_source_path)?;
    let guard = store.begin_event_search_bulk_mode()?;
    let import = import_core_and_retire(
        store,
        &guard,
        &authority,
        &digest,
        &options,
        &mut cursor,
        had_durable_authority,
    );
    let finish = store.finish_event_search_bulk_mode(&guard);
    let (mut summary, published_authority) = import?;
    finish?;
    if published_authority && !summary.work_remaining {
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
    summary.set_work_result(if published_authority {
        ProviderImportWorkResult::Changed
    } else {
        ProviderImportWorkResult::NoOp
    });
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
    ensure_active_journal(store)?;
    let guard = store.begin_event_search_bulk_mode()?;
    let import = publish_cursor_advance(
        store,
        &guard,
        authority,
        options,
        &mut cursor,
        CursorPhase::Complete { missing: true },
        true,
    );
    let finish = store.finish_event_search_bulk_mode(&guard);
    import?;
    finish?;
    let mut summary = ProviderImportSummary::default();
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
    had_durable_authority: bool,
) -> Result<(ProviderImportSummary, bool)> {
    let mut summary = ProviderImportSummary::default();
    let mut scanner = None;
    let mut published_authority = had_durable_authority;
    loop {
        match cursor.phase.clone() {
            CursorPhase::Core {
                next_offset,
                next_ordinal,
                prefix_sha256,
            } => {
                if scanner.is_none() {
                    scanner = Some(PromptHistoryScanner::open(
                        authority,
                        digest,
                        next_offset,
                        next_ordinal,
                        prefix_sha256,
                    )?);
                }
                let scanner = scanner.as_mut().ok_or(CaptureError::SystemInvariant(
                    "Codex prompt-history Drain scanner disappeared",
                ))?;
                scanner.validate_frontier(next_offset, next_ordinal, prefix_sha256)?;
                let page = prepare_page(scanner, digest, cursor)?;
                if !published_authority && !page.outcome.has_retained_content() {
                    let terminal = absorb_fresh_page_without_authority(cursor, page, &mut summary);
                    if terminal {
                        break;
                    }
                    continue;
                }
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
                published_authority = true;
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
                published_authority = true;
            }
            CursorPhase::Complete { .. } => break,
        }
        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
            summary.work_remaining = !cursor.terminal();
            break;
        }
    }
    Ok((summary, published_authority))
}

mod cursor;
mod identity;
mod output;
mod page;
mod rows;
mod source_backed;
#[cfg(test)]
mod tests;

use cursor::*;
use identity::*;
use output::*;
use page::*;
use rows::*;
pub(crate) use source_backed::{
    observe_codex_prompt_history_source_backed_explicit_v0,
    scan_codex_prompt_history_source_backed_v0, CodexPromptHistorySourceBackedDispositionV0,
    CodexPromptHistorySourceBackedInputV0, CodexPromptHistorySourceBackedResolverV0,
};
