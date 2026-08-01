//! Provider-local source-backed extraction and direct Core projection for Deep Agents.
//!
//! This module deliberately stops at the provider boundary. It emits bounded
//! complete Core records and a certified SQLite snapshot, but does not choose
//! publication, replacement, deletion, or retry policy.

mod replacement;

use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource, CoreRecord,
    CoreRecordError, EventIdentityInput, NativeItemKey, NativeSessionKey, PositionStability,
    ProjectionContractError, ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey,
    StableEntityId, SubrecordSelector, TypedKey,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::super::{
    message::{
        core_eligible, deepagents_event_type, deepagents_messages_from_blob, DeepAgentsMessage,
    },
    record_evidence::deepagents_write_record_digest,
    source::{
        deepagents_checkpoint_contexts, deepagents_logical_fingerprint, deepagents_validate_schema,
        deepagents_write_candidate_page, DeepAgentsThreadSummary, DeepAgentsWriteCandidate,
        DeepAgentsWriteKey,
    },
};
use crate::{
    common::io::ProviderSourceRoot,
    provider::sqlite::sqlite_schema_fingerprint,
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteLogicalSnapshot, SqliteSourceAccessError, SqliteSourceEvidence,
        SqliteSourceReadSnapshot,
    },
    CaptureError, ProviderAdapterContext, DEEPAGENTS_SQLITE_SOURCE_FORMAT,
    MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

const DEEPAGENTS_SOURCE_ANCHOR_NAMESPACE: &str = "deepagents.sessions";
const DEEPAGENTS_SOURCE_ANCHOR_KEY: &str = "selected-sessions-db";
const DEEPAGENTS_SOURCE_SCHEMA_VARIANT: &str = "deepagents-sqlite-write-messages-v0";
const DEEPAGENTS_SOURCE_PARSER_REVISION: &str = "deepagents-source-backed-v0";
const DEEPAGENTS_NATIVE_SESSION_NAMESPACE: &str = "deepagents.thread";
const DEEPAGENTS_NATIVE_MESSAGE_NAMESPACE: &str = "deepagents.message";
const DEEPAGENTS_NATIVE_WRITE_NAMESPACE: &str = "deepagents.write";
const DEEPAGENTS_MESSAGE_OFFSET_KIND: &str = "deepagents.write-message-offset";
const DEEPAGENTS_LOGICAL_SESSION_KIND: &str = "deepagents-thread";
const DEEPAGENTS_LOGICAL_EVENT_KIND: &str = "deepagents-message";
const DEEPAGENTS_PAGE_MAX_DOCUMENTS: usize = 64;
const DEEPAGENTS_SOURCE_DIGEST_DOMAIN: &[u8] = b"ctx-deepagents-source-backed-v0\0";
const DEEPAGENTS_REJECTED_RECORD_DOMAIN: &[u8] = b"ctx-deepagents-rejected-record-v0\0";
const SQLITE_SOURCE_INVALID_REASON: &str =
    "Deep Agents SQLite source must have an authorized parent and database leaf";

#[derive(Debug, Error)]
pub(crate) enum DeepAgentsSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Deep Agents source-backed scanner must be exhausted before certification")]
    ScannerNotExhausted,
    #[error("Deep Agents source-backed count overflow")]
    CountOverflow,
}

pub(crate) type DeepAgentsSourceBackedResultV0<T> = Result<T, DeepAgentsSourceBackedErrorV0>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeepAgentsDatabaseRouteV0 {
    // Current/legacy remain explicit to preserve the no-fallback selection
    // contract exercised by platform-independent route checks.
    #[allow(dead_code)]
    Current,
    #[allow(dead_code)]
    Legacy,
    Explicit,
}

impl DeepAgentsDatabaseRouteV0 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Legacy => "legacy",
            Self::Explicit => "explicit",
        }
    }
}

/// The one database selected by Deep Agents' current-over-legacy rule.
///
/// An existing but unsafe current path remains selected and is rejected when
/// opened. It never causes a silent fallback to stale legacy history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeepAgentsDatabaseSelectionV0 {
    path: PathBuf,
    data_root: PathBuf,
    route: DeepAgentsDatabaseRouteV0,
}

impl DeepAgentsDatabaseSelectionV0 {
    // Home selection is retained as the authoritative current-over-legacy,
    // fail-closed route policy even when release capture supplies an explicit path.
    #[cfg(test)]
    pub(crate) fn from_home(data_root: &Path, home: &Path) -> Self {
        let current = home.join(".deepagents/.state/sessions.db");
        let legacy = home.join(".deepagents/sessions.db");
        match fs::symlink_metadata(&current) {
            Ok(_) => Self {
                path: current,
                data_root: data_root.to_path_buf(),
                route: DeepAgentsDatabaseRouteV0::Current,
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if fs::symlink_metadata(&legacy).is_ok() {
                    Self {
                        path: legacy,
                        data_root: data_root.to_path_buf(),
                        route: DeepAgentsDatabaseRouteV0::Legacy,
                    }
                } else {
                    Self {
                        path: current,
                        data_root: data_root.to_path_buf(),
                        route: DeepAgentsDatabaseRouteV0::Current,
                    }
                }
            }
            Err(_) => Self {
                path: current,
                data_root: data_root.to_path_buf(),
                route: DeepAgentsDatabaseRouteV0::Current,
            },
        }
    }

    pub(crate) fn explicit(data_root: &Path, path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            data_root: data_root.to_path_buf(),
            route: DeepAgentsDatabaseRouteV0::Explicit,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    // Route identity is retained with the selected path as provenance evidence.
    #[allow(dead_code)]
    pub(crate) fn route(&self) -> DeepAgentsDatabaseRouteV0 {
        self.route
    }
}

#[derive(Debug)]
struct PendingWriteV0 {
    key: DeepAgentsWriteKey,
    record_digest: [u8; 32],
    session_id: StableEntityId,
    occurred_at: DateTime<Utc>,
    cwd: Option<String>,
    branch: Option<String>,
    messages: Vec<DeepAgentsMessage>,
    next_message_offset: usize,
    first_event_sequence: u64,
}

#[derive(Debug)]
pub(crate) struct DeepAgentsSourceBackedScanV0 {
    pub(crate) source: SourceKey,
    pub(crate) certificate: CertifiedSource,
    #[cfg(test)]
    pub(crate) selected_path: PathBuf,
    #[cfg(test)]
    pub(crate) selected_route: DeepAgentsDatabaseRouteV0,
    pub(crate) terminal_fence: DeepAgentsSourceTerminalFence,
    pub(crate) row_decode_passes: u64,
    pub(crate) decoded_rows: u64,
    pub(crate) peak_buffered_documents: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct DeepAgentsSourceTerminalFence {
    evidence: SqliteSourceEvidence,
}

/// Bounded scanner for one immutable observation of the selected sessions DB.
pub(crate) struct DeepAgentsSourceBackedScannerV0 {
    #[cfg(test)]
    selection: DeepAgentsDatabaseSelectionV0,
    evidence: SqliteSourceEvidence,
    source_root: Option<ProviderSourceRoot>,
    sqlite_snapshot: Option<SqliteSourceReadSnapshot>,
    source: SourceKey,
    schema_evidence: Vec<u8>,
    logical_fingerprint: [u8; 32],
    context: ProviderAdapterContext,
    source_path: String,
    after_rowid: Option<i64>,
    pending_candidates: VecDeque<DeepAgentsWriteCandidate>,
    checkpoint_times: BTreeMap<(String, String), DateTime<Utc>>,
    thread_summaries: BTreeMap<String, DeepAgentsThreadSummary>,
    pending: Option<PendingWriteV0>,
    current_thread: Option<DeepAgentsThreadSummary>,
    next_event_sequence: u64,
    counts: ScannedSourceCounts,
    content_digest: Sha256,
    exhausted: bool,
    decoded_rows: u64,
    peak_buffered_documents: u64,
}

impl DeepAgentsSourceBackedScannerV0 {
    pub(crate) fn open(
        selection: DeepAgentsDatabaseSelectionV0,
        imported_at: DateTime<Utc>,
    ) -> DeepAgentsSourceBackedResultV0<Self> {
        let (source_root, sqlite_snapshot) =
            open_root_authorized_snapshot(&selection.data_root, selection.path())?;
        let evidence = sqlite_snapshot.evidence().clone();
        let conn = sqlite_snapshot.connection()?;
        deepagents_validate_schema(conn, selection.path())?;
        let schema_fingerprint = sqlite_schema_fingerprint(conn)?;
        let source = deepagents_source_key()?;
        let schema_evidence = deepagents_schema_evidence(&selection, &schema_fingerprint)?;
        let logical_fingerprint = deepagents_logical_fingerprint(conn, &schema_evidence)?;
        let mut content_digest = Sha256::new();
        content_digest.update(DEEPAGENTS_SOURCE_DIGEST_DOMAIN);
        content_digest.update(source.exact_descriptor_digest());
        content_digest.update((schema_fingerprint.len() as u64).to_be_bytes());
        content_digest.update(schema_fingerprint.as_bytes());
        let context = ProviderAdapterContext {
            machine_id: "deepagents-source-backed".to_owned(),
            source_path: Some(selection.path().to_path_buf()),
            source_root: None,
            imported_at,
        };
        let source_path = selection.path().display().to_string();
        Ok(Self {
            #[cfg(test)]
            selection,
            evidence,
            source_root: Some(source_root),
            sqlite_snapshot: Some(sqlite_snapshot),
            source,
            schema_evidence,
            logical_fingerprint,
            context,
            source_path,
            after_rowid: None,
            pending_candidates: VecDeque::new(),
            checkpoint_times: BTreeMap::new(),
            thread_summaries: BTreeMap::new(),
            pending: None,
            current_thread: None,
            next_event_sequence: 1,
            counts: ScannedSourceCounts::default(),
            content_digest,
            exhausted: false,
            decoded_rows: 0,
            peak_buffered_documents: 0,
        })
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn logical_fingerprint(&self) -> [u8; 32] {
        self.logical_fingerprint
    }

    pub(crate) fn terminal_revalidator(
        &self,
    ) -> Box<dyn Fn() -> Result<(), SqliteSourceAccessError> + Send + Sync + 'static> {
        self.sqlite_snapshot
            .as_ref()
            .map(SqliteSourceReadSnapshot::terminal_revalidator)
            .unwrap_or_else(|| Box::new(|| Err(SqliteSourceAccessError::SnapshotNotActive)))
    }

    /// Returns at most 64 complete Core records directly from the pinned snapshot.
    pub(crate) fn next_page(&mut self) -> DeepAgentsSourceBackedResultV0<Option<Vec<CoreRecord>>> {
        if self.exhausted {
            return Ok(None);
        }
        let mut page = Vec::with_capacity(DEEPAGENTS_PAGE_MAX_DOCUMENTS);
        while page.len() < DEEPAGENTS_PAGE_MAX_DOCUMENTS {
            if self.pending.is_none() && !self.prepare_next_write()? {
                self.exhausted = true;
                break;
            }
            let Some(pending) = self.pending.as_mut() else {
                continue;
            };
            while page.len() < DEEPAGENTS_PAGE_MAX_DOCUMENTS
                && pending.next_message_offset < pending.messages.len()
            {
                let offset = pending.next_message_offset;
                pending.next_message_offset = pending
                    .next_message_offset
                    .checked_add(1)
                    .ok_or(DeepAgentsSourceBackedErrorV0::CountOverflow)?;
                let message = &pending.messages[offset];
                if !core_eligible(message) {
                    continue;
                }
                let offset_sequence = u64::try_from(offset)
                    .map_err(|_| DeepAgentsSourceBackedErrorV0::CountOverflow)?;
                let event_sequence = pending
                    .first_event_sequence
                    .checked_add(offset_sequence)
                    .ok_or(DeepAgentsSourceBackedErrorV0::CountOverflow)?;
                page.push(deepagents_core_record(
                    &self.source,
                    &pending.key,
                    pending.session_id,
                    event_sequence,
                    offset,
                    pending.occurred_at,
                    pending.cwd.as_deref(),
                    pending.branch.as_deref(),
                    message,
                )?);
                self.counts.indexed_documents = checked_add(self.counts.indexed_documents, 1)?;
            }
            if pending.next_message_offset == pending.messages.len() {
                self.pending = None;
            }
        }
        self.peak_buffered_documents = self.peak_buffered_documents.max(
            u64::try_from(page.len()).map_err(|_| DeepAgentsSourceBackedErrorV0::CountOverflow)?,
        );
        if page.is_empty() && self.exhausted {
            Ok(None)
        } else {
            Ok(Some(page))
        }
    }

    pub(crate) fn finish(mut self) -> DeepAgentsSourceBackedResultV0<DeepAgentsSourceBackedScanV0> {
        if !self.exhausted || self.pending.is_some() {
            return Err(DeepAgentsSourceBackedErrorV0::ScannerNotExhausted);
        }
        let content_digest = self.content_digest.finalize().into();
        let logical_snapshot = SqliteLogicalSnapshot::new(
            DEEPAGENTS_SOURCE_PARSER_REVISION,
            &self.schema_evidence,
            content_digest,
            self.counts,
        );
        let sqlite_snapshot = self
            .sqlite_snapshot
            .take()
            .ok_or(DeepAgentsSourceBackedErrorV0::ScannerNotExhausted)?;
        let closing_evidence = sqlite_snapshot.finish()?;
        if closing_evidence != self.evidence {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        self.source_root
            .take()
            .ok_or(DeepAgentsSourceBackedErrorV0::ScannerNotExhausted)?
            .revalidate()?;
        let certificate = logical_snapshot.certify(self.source.clone())?;
        Ok(DeepAgentsSourceBackedScanV0 {
            source: self.source,
            certificate,
            #[cfg(test)]
            selected_path: self.selection.path,
            #[cfg(test)]
            selected_route: self.selection.route,
            terminal_fence: DeepAgentsSourceTerminalFence {
                evidence: closing_evidence,
            },
            row_decode_passes: 1,
            decoded_rows: self.decoded_rows,
            peak_buffered_documents: self.peak_buffered_documents,
        })
    }

    pub(crate) fn seal_unscanned(
        mut self,
    ) -> DeepAgentsSourceBackedResultV0<DeepAgentsSourceTerminalFence> {
        let sqlite_snapshot = self
            .sqlite_snapshot
            .take()
            .ok_or(DeepAgentsSourceBackedErrorV0::ScannerNotExhausted)?;
        let closing_evidence = sqlite_snapshot.finish()?;
        if closing_evidence != self.evidence {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        self.source_root
            .take()
            .ok_or(DeepAgentsSourceBackedErrorV0::ScannerNotExhausted)?
            .revalidate()?;
        Ok(DeepAgentsSourceTerminalFence {
            evidence: closing_evidence,
        })
    }

    fn connection(&self) -> DeepAgentsSourceBackedResultV0<&rusqlite::Connection> {
        self.sqlite_snapshot
            .as_ref()
            .ok_or(DeepAgentsSourceBackedErrorV0::ScannerNotExhausted)?
            .connection()
            .map_err(Into::into)
    }

    fn prepare_next_write(&mut self) -> DeepAgentsSourceBackedResultV0<bool> {
        loop {
            if self.pending_candidates.is_empty() {
                let candidates =
                    deepagents_write_candidate_page(self.connection()?, self.after_rowid, 64)?;
                if candidates.is_empty() {
                    return Ok(false);
                }
                let contexts =
                    deepagents_checkpoint_contexts(self.connection()?, &self.context, &candidates)?;
                self.checkpoint_times = contexts.checkpoint_times;
                self.thread_summaries = contexts.threads;
                self.pending_candidates.extend(candidates);
            }
            let Some(candidate) = self.pending_candidates.pop_front() else {
                return Ok(false);
            };
            self.after_rowid = Some(candidate.rowid);
            self.observe_candidate_bytes(&candidate)?;
            let Some(key) = candidate.key.clone() else {
                self.observe_rejected_candidate(&candidate);
                self.add_rejected_records(1)?;
                continue;
            };
            self.refresh_thread(&key.thread_id)?;
            let Some(occurred_at) = self
                .checkpoint_times
                .get(&(key.thread_id.clone(), key.checkpoint_id.clone()))
                .copied()
            else {
                self.observe_rejected_candidate(&candidate);
                self.add_rejected_records(1)?;
                continue;
            };
            let value_type = candidate.value_type;
            let value = candidate.value.ok_or_else(|| {
                DeepAgentsSourceBackedErrorV0::Capture(CaptureError::SourceChangedDuringCapture)
            })?;
            self.decoded_rows = checked_add(self.decoded_rows, 1)?;
            let record_digest = digest_bytes(&deepagents_write_record_digest(
                &key,
                value_type.as_deref(),
                &value,
            ))
            .ok_or(CaptureError::SystemInvariant(
                "Deep Agents logical record digest is not canonical SHA-256",
            ))?;
            self.content_digest.update(record_digest);
            let decoded = match deepagents_messages_from_blob(value_type.as_deref(), &value) {
                Ok(decoded) => decoded,
                Err(_) => {
                    self.add_rejected_records(1)?;
                    continue;
                }
            };
            let accepted = u64::try_from(decoded.messages.len())
                .map_err(|_| DeepAgentsSourceBackedErrorV0::CountOverflow)?;
            let complete = accepted
                .checked_add(decoded.rejected_entries)
                .and_then(|count| count.checked_add(decoded.ignored_entries))
                .ok_or(DeepAgentsSourceBackedErrorV0::CountOverflow)?;
            self.counts.complete_records = checked_add(self.counts.complete_records, complete)?;
            self.counts.retained_records = checked_add(self.counts.retained_records, accepted)?;
            self.counts.rejected_records =
                checked_add(self.counts.rejected_records, decoded.rejected_entries)?;
            self.counts.ignored_records =
                checked_add(self.counts.ignored_records, decoded.ignored_entries)?;
            if decoded.messages.is_empty() {
                continue;
            }
            let session_id = deepagents_session_id(&self.source, &key.thread_id)?;
            let first_event_sequence = self.next_event_sequence;
            self.next_event_sequence = self
                .next_event_sequence
                .checked_add(accepted)
                .ok_or(DeepAgentsSourceBackedErrorV0::CountOverflow)?;
            self.pending = Some(PendingWriteV0 {
                key,
                record_digest,
                session_id,
                occurred_at,
                cwd: self
                    .current_thread
                    .as_ref()
                    .and_then(|summary| summary.thread.cwd.clone()),
                branch: self
                    .current_thread
                    .as_ref()
                    .and_then(|summary| summary.thread.git_branch.clone()),
                messages: decoded.messages,
                next_message_offset: 0,
                first_event_sequence,
            });
            return Ok(true);
        }
    }

    fn refresh_thread(&mut self, thread_id: &str) -> DeepAgentsSourceBackedResultV0<()> {
        if self
            .current_thread
            .as_ref()
            .is_some_and(|summary| summary.thread.thread_id == thread_id)
        {
            return Ok(());
        }
        self.current_thread = self.thread_summaries.get(thread_id).cloned();
        self.next_event_sequence = 1;
        Ok(())
    }

    fn observe_candidate_bytes(
        &mut self,
        candidate: &DeepAgentsWriteCandidate,
    ) -> DeepAgentsSourceBackedResultV0<()> {
        self.counts.certified_bytes = self
            .counts
            .certified_bytes
            .checked_add(candidate.observed_bytes()?)
            .ok_or(DeepAgentsSourceBackedErrorV0::CountOverflow)?;
        Ok(())
    }

    fn observe_rejected_candidate(&mut self, candidate: &DeepAgentsWriteCandidate) {
        self.content_digest
            .update(DEEPAGENTS_REJECTED_RECORD_DOMAIN);
        self.content_digest
            .update(candidate.retained_bytes.to_be_bytes());
        if let Some(reason) = &candidate.rejection_reason {
            self.content_digest
                .update((reason.len() as u64).to_be_bytes());
            self.content_digest.update(reason.as_bytes());
        }
    }

    fn add_rejected_records(&mut self, rejected: u64) -> DeepAgentsSourceBackedResultV0<()> {
        self.counts.complete_records = checked_add(self.counts.complete_records, rejected)?;
        self.counts.rejected_records = checked_add(self.counts.rejected_records, rejected)?;
        Ok(())
    }
}

fn open_root_authorized_snapshot(
    data_root: &Path,
    path: &Path,
) -> DeepAgentsSourceBackedResultV0<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook(data_root, path, || {})
}

fn open_root_authorized_snapshot_with_hook(
    data_root: &Path,
    path: &Path,
    after_authorize: impl FnOnce(),
) -> DeepAgentsSourceBackedResultV0<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let database_leaf =
        path.file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: SQLITE_SOURCE_INVALID_REASON,
            })?;
    let source_root = ProviderSourceRoot::open(parent)?;
    let source_directory = source_root.directory()?;
    let parent_handle = source_directory
        .try_clone_authority_handle()
        .map_err(CaptureError::from)?;
    let sqlite_authority =
        retain_sqlite_source_directory_authority(data_root, &parent_handle, parent)?;
    let sqlite_snapshot =
        open_root_handle_sqlite_source_snapshot(&sqlite_authority, database_leaf)?;
    after_authorize();
    sqlite_snapshot.revalidate()?;
    source_directory.revalidate()?;
    source_root.revalidate()?;
    let connection = sqlite_snapshot.connection()?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| DeepAgentsSourceBackedErrorV0::CountOverflow)?;
    connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(CaptureError::from)?;
    Ok((source_root, sqlite_snapshot))
}

fn deepagents_source_key() -> DeepAgentsSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        DEEPAGENTS_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(DEEPAGENTS_SOURCE_ANCHOR_KEY)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::DeepAgents.as_str(),
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        DEEPAGENTS_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn deepagents_schema_evidence(
    selection: &DeepAgentsDatabaseSelectionV0,
    schema_fingerprint: &str,
) -> DeepAgentsSourceBackedResultV0<Vec<u8>> {
    Ok(serde_json::to_vec(&serde_json::json!({
        "route": selection.route.as_str(),
        "schema_fingerprint": schema_fingerprint,
    }))?)
}

fn deepagents_session_id(
    source: &SourceKey,
    thread_id: &str,
) -> DeepAgentsSourceBackedResultV0<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        DEEPAGENTS_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(thread_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: DEEPAGENTS_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

#[allow(clippy::too_many_arguments)]
fn deepagents_core_record(
    source: &SourceKey,
    key: &DeepAgentsWriteKey,
    session_id: StableEntityId,
    event_sequence: u64,
    message_offset: usize,
    occurred_at: DateTime<Utc>,
    cwd: Option<&str>,
    branch: Option<&str>,
    message: &DeepAgentsMessage,
) -> DeepAgentsSourceBackedResultV0<CoreRecord> {
    let write_key = vec![
        TypedKey::utf8(&key.checkpoint_id)?,
        TypedKey::utf8(&key.task_id)?,
        TypedKey::I64(key.idx),
    ];
    let fallback_item_key = NativeItemKey::composite(DEEPAGENTS_NATIVE_WRITE_NAMESPACE, write_key)?;
    let native_item_key = match message.message_id.as_deref() {
        Some(message_id) => NativeItemKey::native_id(
            DEEPAGENTS_NATIVE_MESSAGE_NAMESPACE,
            TypedKey::utf8(message_id)?,
        )?,
        None => fallback_item_key,
    };
    let fallback_selector = SubrecordSelector::certified_position(
        DEEPAGENTS_MESSAGE_OFFSET_KIND,
        TypedKey::U64(
            u64::try_from(message_offset)
                .map_err(|_| DeepAgentsSourceBackedErrorV0::CountOverflow)?,
        ),
        PositionStability::StableSlot,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: DEEPAGENTS_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: message.message_id.is_none().then_some(&fallback_selector),
    })?;
    let primary_key = TypedKey::composite(vec![
        TypedKey::utf8(&key.thread_id)?,
        TypedKey::utf8(&key.checkpoint_id)?,
        TypedKey::utf8(&key.task_id)?,
        TypedKey::I64(key.idx),
        TypedKey::U64(
            u64::try_from(message_offset)
                .map_err(|_| DeepAgentsSourceBackedErrorV0::CountOverflow)?,
        ),
    ])?;
    let body = message.text.clone();
    let event_type = deepagents_event_type(message);
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        event_sequence,
        event_type.as_str(),
        AgentType::Primary.as_str(),
        true,
        DEEPAGENTS_SOURCE_PARSER_REVISION,
        body,
    )?;
    record.provider_session_id = Some(key.thread_id.clone());
    record.native_event_id = Some(primary_key);
    record.occurred_at_unix_ms = Some(occurred_at.timestamp_millis());
    record.role = Some(message.role.as_str().to_owned());
    record.branch = branch.map(str::to_owned);
    record.cwd = cwd.map(str::to_owned);
    record.content.structured_content = Some(serde_json::json!({
        "provider_native_message": {
            "message_type": message.message_type,
            "message_class": message.message_class,
            "message_id": message.message_id,
            "tool_call_id": message.tool_call_id,
            "status": message.status,
            "exit_code": message.exit_code,
            "duration_ms": message.duration_ms,
            "timed_out": message.timed_out,
            "is_error": message.is_error,
            "success": message.success,
        }
    }));
    record.validate_contract()?;
    Ok(record)
}

fn digest_bytes(digest: &crate::record_evidence::RecordDigest) -> Option<[u8; 32]> {
    let raw = digest.as_str().as_bytes();
    if raw.len() != 64 {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in raw.chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        decoded[index] = (high << 4) | low;
    }
    Some(decoded)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn checked_add(left: u64, right: u64) -> DeepAgentsSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(DeepAgentsSourceBackedErrorV0::CountOverflow)
}

#[cfg(test)]
mod tests;
