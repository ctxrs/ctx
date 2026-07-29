//! Provider-local source-backed extraction and exact hydration for Deep Agents.
//!
//! This module deliberately stops at the provider boundary. It emits bounded
//! lexical documents and a certified SQLite snapshot, but does not choose
//! publication, replacement, deletion, or retry policy.

use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, PositionStability, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, SubrecordSelector, TypedKey,
};
use ctx_history_index::{LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::super::{
    complete_content::{
        deepagents_write_record_digest, resolve_deepagents_content,
        validate_deepagents_content_schema, DeepAgentsContentAddress,
    },
    message::{deepagents_messages_from_blob, DeepAgentsMessage},
    source::{
        deepagents_checkpoint_time, deepagents_hydrate_write, deepagents_next_write_candidate,
        deepagents_thread_summary, deepagents_validate_schema, DeepAgentsThreadSummary,
        DeepAgentsWriteCandidate, DeepAgentsWriteKey,
    },
};
use super::core_eligible;
use crate::{
    common::io::ProviderSourceRoot,
    provider::{normalization::capped_text, sqlite::sqlite_schema_fingerprint},
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceEvidence, SqliteSourceReadSnapshot,
    },
    CaptureError, ProviderAdapterContext, DEEPAGENTS_SQLITE_SOURCE_FORMAT,
    MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

const DEEPAGENTS_SOURCE_ANCHOR_NAMESPACE: &str = "deepagents.sessions";
const DEEPAGENTS_SOURCE_ANCHOR_KEY: &str = "selected-sessions-db";
const DEEPAGENTS_SOURCE_SCHEMA_VARIANT: &str = "deepagents-sqlite-write-messages-v0";
const DEEPAGENTS_SOURCE_REVISION_KIND: &str = "deepagents-sqlite-snapshot-v0";
const DEEPAGENTS_SOURCE_PARSER_REVISION: &str = "deepagents-source-backed-v0";
const DEEPAGENTS_NATIVE_SESSION_NAMESPACE: &str = "deepagents.thread";
const DEEPAGENTS_NATIVE_MESSAGE_NAMESPACE: &str = "deepagents.message";
const DEEPAGENTS_NATIVE_WRITE_NAMESPACE: &str = "deepagents.write";
const DEEPAGENTS_MESSAGE_OFFSET_KIND: &str = "deepagents.write-message-offset";
const DEEPAGENTS_LOGICAL_SESSION_KIND: &str = "deepagents-thread";
const DEEPAGENTS_LOGICAL_EVENT_KIND: &str = "deepagents-message";
const DEEPAGENTS_LOGICAL_RELATION: &str = "writes.messages";
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
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Deep Agents source-backed scanner must be exhausted before certification")]
    ScannerNotExhausted,
    #[error("Deep Agents source-backed count overflow")]
    CountOverflow,
    #[error("locator is not a Deep Agents write-message coordinate")]
    InvalidLocator,
    #[error("Deep Agents SQLite snapshot evidence no longer matches")]
    StaleSourceEvidence,
    #[error("Deep Agents write-message record evidence no longer matches")]
    StaleRecordEvidence,
    #[error("Deep Agents write-message row or subrecord no longer exists")]
    MissingRecord,
}

pub(crate) type DeepAgentsSourceBackedResultV0<T> = Result<T, DeepAgentsSourceBackedErrorV0>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeepAgentsDatabaseRouteV0 {
    Current,
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
    route: DeepAgentsDatabaseRouteV0,
}

impl DeepAgentsDatabaseSelectionV0 {
    pub(crate) fn from_home(home: &Path) -> Self {
        let current = home.join(".deepagents/.state/sessions.db");
        let legacy = home.join(".deepagents/sessions.db");
        match fs::symlink_metadata(&current) {
            Ok(_) => Self {
                path: current,
                route: DeepAgentsDatabaseRouteV0::Current,
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if fs::symlink_metadata(&legacy).is_ok() {
                    Self {
                        path: legacy,
                        route: DeepAgentsDatabaseRouteV0::Legacy,
                    }
                } else {
                    Self {
                        path: current,
                        route: DeepAgentsDatabaseRouteV0::Current,
                    }
                }
            }
            Err(_) => Self {
                path: current,
                route: DeepAgentsDatabaseRouteV0::Current,
            },
        }
    }

    pub(crate) fn explicit(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            route: DeepAgentsDatabaseRouteV0::Explicit,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

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
    pub(crate) selected_path: PathBuf,
    pub(crate) selected_route: DeepAgentsDatabaseRouteV0,
}

/// Bounded scanner for one immutable observation of the selected sessions DB.
pub(crate) struct DeepAgentsSourceBackedScannerV0 {
    selection: DeepAgentsDatabaseSelectionV0,
    evidence: SqliteSourceEvidence,
    source_root: Option<ProviderSourceRoot>,
    sqlite_snapshot: Option<SqliteSourceReadSnapshot>,
    source: SourceKey,
    observation: SourceObservation,
    source_revision_digest: [u8; 32],
    context: ProviderAdapterContext,
    source_path: String,
    after_rowid: Option<i64>,
    pending: Option<PendingWriteV0>,
    current_thread: Option<DeepAgentsThreadSummary>,
    next_event_sequence: u64,
    counts: ScannedSourceCounts,
    content_digest: Sha256,
    exhausted: bool,
    source_validated: bool,
    validated_pages: VecDeque<Vec<LexicalDocument>>,
}

impl DeepAgentsSourceBackedScannerV0 {
    pub(crate) fn open(
        selection: DeepAgentsDatabaseSelectionV0,
        imported_at: DateTime<Utc>,
    ) -> DeepAgentsSourceBackedResultV0<Self> {
        let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(selection.path())?;
        let evidence = sqlite_snapshot.evidence().clone();
        let conn = sqlite_snapshot.connection()?;
        deepagents_validate_schema(conn, selection.path())?;
        let schema_fingerprint = sqlite_schema_fingerprint(conn)?;
        let source = deepagents_source_key()?;
        let revision = deepagents_snapshot_revision(&selection, &evidence, &schema_fingerprint)?;
        let source_revision_digest = Sha256::digest(&revision).into();
        let observation =
            SourceObservation::new(source.clone(), DEEPAGENTS_SOURCE_REVISION_KIND, revision)?;
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
            selection,
            evidence,
            source_root: Some(source_root),
            sqlite_snapshot: Some(sqlite_snapshot),
            source,
            observation,
            source_revision_digest,
            context,
            source_path,
            after_rowid: None,
            pending: None,
            current_thread: None,
            next_event_sequence: 1,
            counts: ScannedSourceCounts::default(),
            content_digest,
            exhausted: false,
            source_validated: false,
            validated_pages: VecDeque::new(),
        })
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn source_revision_digest(&self) -> &[u8; 32] {
        &self.source_revision_digest
    }

    /// Returns at most 64 bounded lexical records after the complete source
    /// read has finished and passed terminal revalidation.
    pub(crate) fn next_page(
        &mut self,
    ) -> DeepAgentsSourceBackedResultV0<Option<Vec<LexicalDocument>>> {
        if !self.source_validated {
            self.stage_and_validate_pages()?;
        }
        Ok(self.validated_pages.pop_front())
    }

    fn next_unvalidated_page(
        &mut self,
    ) -> DeepAgentsSourceBackedResultV0<Option<Vec<LexicalDocument>>> {
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
                page.push(deepagents_lexical_document(
                    &self.source,
                    self.source_revision_digest,
                    &pending.key,
                    pending.record_digest,
                    pending.session_id,
                    event_sequence,
                    offset,
                    pending.occurred_at,
                    pending.cwd.as_deref(),
                    pending.branch.as_deref(),
                    &self.source_path,
                    message,
                )?);
                self.counts.indexed_documents = checked_add(self.counts.indexed_documents, 1)?;
            }
            if pending.next_message_offset == pending.messages.len() {
                self.pending = None;
            }
        }
        if page.is_empty() && self.exhausted {
            Ok(None)
        } else {
            Ok(Some(page))
        }
    }

    pub(crate) fn finish(self) -> DeepAgentsSourceBackedResultV0<DeepAgentsSourceBackedScanV0> {
        if !self.source_validated
            || !self.validated_pages.is_empty()
            || !self.exhausted
            || self.pending.is_some()
        {
            return Err(DeepAgentsSourceBackedErrorV0::ScannerNotExhausted);
        }
        let content_digest = self.content_digest.finalize().into();
        let certificate = CertifiedSource::certify(
            self.observation.clone(),
            self.observation,
            DEEPAGENTS_SOURCE_PARSER_REVISION,
            content_digest,
            self.counts,
        )?;
        Ok(DeepAgentsSourceBackedScanV0 {
            source: self.source,
            certificate,
            selected_path: self.selection.path,
            selected_route: self.selection.route,
        })
    }

    fn stage_and_validate_pages(&mut self) -> DeepAgentsSourceBackedResultV0<()> {
        while let Some(page) = self.next_unvalidated_page()? {
            self.validated_pages.push_back(page);
        }
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
        self.source_validated = true;
        Ok(())
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
            let Some(candidate) =
                deepagents_next_write_candidate(self.connection()?, self.after_rowid)?
            else {
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
            let Some(occurred_at) = deepagents_checkpoint_time(
                self.connection()?,
                &self.context,
                &key.thread_id,
                &key.checkpoint_id,
            )?
            else {
                self.observe_rejected_candidate(&candidate);
                self.add_rejected_records(1)?;
                continue;
            };
            let (value_type, value) =
                deepagents_hydrate_write(self.connection()?, candidate.rowid)?;
            let record_digest = digest_bytes(&deepagents_write_record_digest(
                &key,
                value_type.as_deref(),
                &value,
            ))
            .ok_or(DeepAgentsSourceBackedErrorV0::StaleRecordEvidence)?;
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
        self.current_thread =
            deepagents_thread_summary(self.connection()?, &self.context, thread_id, None)?;
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
        self.content_digest.update(candidate.rowid.to_be_bytes());
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeepAgentsHydratedMessageV0 {
    pub(crate) text: String,
    pub(crate) record_digest: [u8; 32],
}

/// One-invocation resolver for exact Deep Agents row/subrecord locators.
#[derive(Debug, Clone)]
pub(crate) struct DeepAgentsLocatorResolverV0 {
    selection: DeepAgentsDatabaseSelectionV0,
}

impl DeepAgentsLocatorResolverV0 {
    pub(crate) fn from_home(home: &Path) -> Self {
        Self {
            selection: DeepAgentsDatabaseSelectionV0::from_home(home),
        }
    }

    pub(crate) fn explicit(path: impl Into<PathBuf>) -> Self {
        Self {
            selection: DeepAgentsDatabaseSelectionV0::explicit(path),
        }
    }

    pub(crate) fn hydrate(
        &self,
        locator: &SourceRecordLocator,
    ) -> DeepAgentsSourceBackedResultV0<DeepAgentsHydratedMessageV0> {
        locator.validate_contract()?;
        let expected_source = deepagents_source_key()?;
        if !expected_source.exact_descriptor_eq(locator.source())
            || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
        {
            return Err(DeepAgentsSourceBackedErrorV0::InvalidLocator);
        }
        let (address, row_version) = decode_deepagents_locator(locator)?;
        if &row_version != locator.record_digest() {
            return Err(DeepAgentsSourceBackedErrorV0::InvalidLocator);
        }

        let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(self.selection.path())?;
        let evidence = sqlite_snapshot.evidence().clone();
        let conn = sqlite_snapshot.connection()?;
        validate_deepagents_content_schema(conn)?;
        let schema_fingerprint = sqlite_schema_fingerprint(conn)?;
        let revision =
            deepagents_snapshot_revision(&self.selection, &evidence, &schema_fingerprint)?;
        let revision_digest: [u8; 32] = Sha256::digest(&revision).into();
        if locator.certified_source_revision_digest() != Some(&revision_digest) {
            return Err(DeepAgentsSourceBackedErrorV0::StaleSourceEvidence);
        }
        let resolved = resolve_deepagents_content(conn, &address)?
            .ok_or(DeepAgentsSourceBackedErrorV0::MissingRecord)?;
        let record_digest = digest_bytes(&resolved.record_digest)
            .ok_or(DeepAgentsSourceBackedErrorV0::StaleRecordEvidence)?;
        if record_digest != row_version {
            return Err(DeepAgentsSourceBackedErrorV0::StaleRecordEvidence);
        }
        let closing_evidence = sqlite_snapshot.finish()?;
        if closing_evidence != evidence {
            return Err(DeepAgentsSourceBackedErrorV0::StaleSourceEvidence);
        }
        source_root.revalidate()?;
        Ok(DeepAgentsHydratedMessageV0 {
            text: resolved.text,
            record_digest,
        })
    }
}

fn open_root_authorized_snapshot(
    path: &Path,
) -> DeepAgentsSourceBackedResultV0<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook(path, || {})
}

fn open_root_authorized_snapshot_with_hook(
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
    let sqlite_authority = retain_sqlite_source_directory_authority(&parent_handle)?;
    let sqlite_snapshot =
        open_root_handle_sqlite_source_snapshot(&sqlite_authority, database_leaf)?;
    after_authorize();
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

fn deepagents_snapshot_revision(
    selection: &DeepAgentsDatabaseSelectionV0,
    evidence: &SqliteSourceEvidence,
    schema_fingerprint: &str,
) -> DeepAgentsSourceBackedResultV0<Vec<u8>> {
    Ok(serde_json::to_vec(&serde_json::json!({
        "route": selection.route.as_str(),
        "snapshot": sqlite_revision_component(evidence),
        "schema_fingerprint": schema_fingerprint,
    }))?)
}

fn sqlite_revision_component(evidence: &SqliteSourceEvidence) -> String {
    format!(
        "identity={};length={};revision={}",
        hex_digest(evidence.identity()),
        evidence.length(),
        hex_digest(evidence.revision()),
    )
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
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
fn deepagents_lexical_document(
    source: &SourceKey,
    source_revision_digest: [u8; 32],
    key: &DeepAgentsWriteKey,
    record_digest: [u8; 32],
    session_id: StableEntityId,
    event_sequence: u64,
    message_offset: usize,
    occurred_at: DateTime<Utc>,
    cwd: Option<&str>,
    branch: Option<&str>,
    source_path: &str,
    message: &DeepAgentsMessage,
) -> DeepAgentsSourceBackedResultV0<LexicalDocument> {
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
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: DEEPAGENTS_LOGICAL_RELATION.to_owned(),
            primary_key,
            row_version: Some(TypedKey::bytes(record_digest.to_vec())?),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(source_revision_digest),
        record_digest,
    )?;
    let (body, _) = capped_text(&message.text, MAX_BODY_PREVIEW_CHARS);
    let event_type = if message.role == ctx_history_core::EventRole::Tool {
        ctx_history_core::EventType::ToolOutput
    } else {
        ctx_history_core::EventType::Message
    };
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(key.thread_id.clone()),
        branch: branch.map(str::to_owned),
        source_path: Some(source_path.to_owned()),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence,
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        event_type: event_type.as_str().to_owned(),
        role: Some(message.role.as_str().to_owned()),
        body,
        workspace: None,
        cwd: cwd.map(str::to_owned),
        touched_files: Vec::new(),
    })
}

fn decode_deepagents_locator(
    locator: &SourceRecordLocator,
) -> DeepAgentsSourceBackedResultV0<(DeepAgentsContentAddress, [u8; 32])> {
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(DeepAgentsSourceBackedErrorV0::InvalidLocator);
    };
    if logical_relation != DEEPAGENTS_LOGICAL_RELATION {
        return Err(DeepAgentsSourceBackedErrorV0::InvalidLocator);
    }
    let TypedKey::Composite(parts) = primary_key else {
        return Err(DeepAgentsSourceBackedErrorV0::InvalidLocator);
    };
    let [TypedKey::Utf8(thread_id), TypedKey::Utf8(checkpoint_id), TypedKey::Utf8(task_id), TypedKey::I64(write_idx), TypedKey::U64(message_offset)] =
        parts.as_slice()
    else {
        return Err(DeepAgentsSourceBackedErrorV0::InvalidLocator);
    };
    let Some(TypedKey::Bytes(row_version)) = row_version else {
        return Err(DeepAgentsSourceBackedErrorV0::InvalidLocator);
    };
    let row_version: [u8; 32] = row_version
        .as_slice()
        .try_into()
        .map_err(|_| DeepAgentsSourceBackedErrorV0::InvalidLocator)?;
    Ok((
        DeepAgentsContentAddress {
            thread_id: thread_id.clone(),
            checkpoint_id: checkpoint_id.clone(),
            task_id: task_id.clone(),
            write_idx: *write_idx,
            message_offset: u32::try_from(*message_offset)
                .map_err(|_| DeepAgentsSourceBackedErrorV0::InvalidLocator)?,
        },
        row_version,
    ))
}

fn digest_bytes(digest: &crate::complete_content::CompleteContentBodyDigest) -> Option<[u8; 32]> {
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
