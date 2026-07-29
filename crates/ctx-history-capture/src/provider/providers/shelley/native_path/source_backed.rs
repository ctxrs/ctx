//! Source-backed Shelley projection over the exact invocation CWD.
//!
//! Shelley does not own a historical-root catalog. Automatic authority is the
//! single `shelley.db` directly below the invocation CWD; callers must not feed
//! remembered project roots or manual paths into this discovery entrypoint.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CertifiedSource, EventIdentityInput,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    ProjectionContractError, ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey,
    SourceObservation, SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::ProviderSourceRoot,
    provider::sqlite::sqlite_schema_fingerprint,
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceEvidence, SqliteSourceReadSnapshot,
    },
    CaptureError, OutputOutcome, ProviderAdapterContext, MAX_PROVIDER_SQLITE_VALUE_BYTES,
    SHELLEY_SQLITE_SOURCE_FORMAT,
};

use super::{
    super::{
        normalization::{shelley_output_classification, shelley_timestamp},
        relationships::{
            shelley_event_role, shelley_event_type, shelley_logical_record_digest,
            shelley_message_body, shelley_message_complete_text, shelley_verified_record_values,
        },
        source::{
            shelley_conversation_columns, shelley_conversation_select_expressions,
            shelley_message_columns, shelley_message_select_expressions,
            shelley_require_message_index,
        },
        SHELLEY_CAPTURE_REVISION, SHELLEY_POLICY_REVISION,
    },
    scanner::next_message_unit,
    ShelleyMessage, ShelleyUnit, SHELLEY_PAGE_MAX_BYTES, SHELLEY_PAGE_MAX_UNITS,
};

const SHELLEY_SOURCE_ANCHOR_NAMESPACE: &str = "shelley.exact-cwd-slot";
const SHELLEY_SOURCE_ANCHOR_KEY: &str = "shelley.db";
const SHELLEY_SOURCE_SCHEMA_VARIANT: &str = "shelley-exact-cwd-sqlite-v1";
const SHELLEY_SOURCE_REVISION_KIND: &str = "shelley-sqlite-snapshot-v1";
const SHELLEY_SOURCE_PARSER_REVISION: &str = "shelley-source-backed-v1";
const SHELLEY_LOGICAL_SESSION_KIND: &str = "shelley-conversation";
const SHELLEY_NATIVE_SESSION_NAMESPACE: &str = "shelley.conversation";
const SHELLEY_LOGICAL_EVENT_KIND: &str = "shelley-message";
const SHELLEY_NATIVE_MESSAGE_NAMESPACE: &str = "shelley.message";
const SHELLEY_COMPOUND_LOCATOR_RELATION: &str = "shelley-compound-message-row-v1";
const SHELLEY_CERTIFIED_STREAM_DOMAIN: &[u8] = b"ctx-shelley-source-backed-stream-v1\0";
const SHELLEY_MAX_LINEAGE_DEPTH: usize = 256;
const SHELLEY_LINEAGE_LABEL_MAX_CHARS: usize = 256;
const SQLITE_SOURCE_INVALID_REASON: &str =
    "Shelley SQLite source must have an authorized parent and database leaf";

#[derive(Debug, Error)]
pub(crate) enum ShelleySourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error("Shelley source-backed scan was not drained to terminal certification")]
    ScanIncomplete,
    #[error("Shelley source-backed counts overflowed")]
    CountOverflow,
    #[error("Shelley source-backed locator does not address a compound message row")]
    InvalidLocator,
    #[error("Shelley source-backed locator no longer addresses a message row")]
    MissingRecord,
    #[error("Shelley source-backed locator record evidence is stale")]
    StaleRecordEvidence,
    #[error("Shelley source-backed projection produced no bounded lexical body")]
    MissingLexicalBody,
    #[error("Shelley source-backed conversation lineage is invalid: {0}")]
    InvalidLineage(String),
}

pub(crate) type ShelleySourceBackedResult<T> = Result<T, ShelleySourceBackedError>;

/// The one automatic Shelley source admitted for an invocation.
#[derive(Debug, Clone)]
pub(crate) struct ShelleySourceBackedAdapter {
    exact_cwd: PathBuf,
    database_path: PathBuf,
    source: SourceKey,
}

impl ShelleySourceBackedAdapter {
    pub(crate) fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn start_scan(&self) -> ShelleySourceBackedResult<ShelleySourceBackedScan> {
        let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(&self.database_path)?;
        let evidence = sqlite_snapshot.evidence().clone();
        let conn = sqlite_snapshot.connection()?;

        let sqlite_user_version = conn
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(CaptureError::from)?;
        let schema_fingerprint = sqlite_schema_fingerprint(conn)?;
        let conversation_columns = shelley_conversation_columns(conn)?;
        let message_columns = shelley_message_columns(conn)?;
        let has_message_sequence_id = message_columns.contains("sequence_id");
        shelley_require_message_index(conn, has_message_sequence_id)?;
        let conversation_select =
            shelley_conversation_select_expressions(&conversation_columns, "c");
        let message_select = shelley_message_select_expressions(&message_columns, "m");
        let revision = shelley_source_revision(&evidence, sqlite_user_version, &schema_fingerprint);
        let opening_observation = SourceObservation::new(
            self.source.clone(),
            SHELLEY_SOURCE_REVISION_KIND,
            revision.into_bytes(),
        )?;
        let context = ProviderAdapterContext {
            machine_id: "shelley-source-backed".to_owned(),
            source_path: Some(self.database_path.clone()),
            source_root: Some(self.exact_cwd.clone()),
            imported_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        };
        let mut content_digest = Sha256::new();
        content_digest.update(SHELLEY_CERTIFIED_STREAM_DOMAIN);

        Ok(ShelleySourceBackedScan {
            source: self.source.clone(),
            evidence,
            source_root: Some(source_root),
            sqlite_snapshot: Some(sqlite_snapshot),
            opening_observation,
            conversation_select,
            message_select,
            has_message_sequence_id,
            context,
            after_rowid: None,
            source_exhausted: false,
            content_digest,
            counts: ScannedSourceCounts::default(),
            session_lineages: HashMap::new(),
            validated_pages: VecDeque::new(),
            receipt: None,
        })
    }

    /// Reopens and verifies one exact compound message/conversation row.
    pub(crate) fn hydrate(
        &self,
        locator: &SourceRecordLocator,
    ) -> ShelleySourceBackedResult<ShelleyHydratedMessage> {
        locator.validate_contract()?;
        if !self.source.exact_descriptor_eq(locator.source())
            || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        {
            return Err(ShelleySourceBackedError::InvalidLocator);
        }
        let (parent_bearing, message_rowid, conversation_rowid) = decode_compound_locator(locator)?;

        let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(&self.database_path)?;
        let evidence = sqlite_snapshot.evidence().clone();
        let conn = sqlite_snapshot.connection()?;
        let conversation_columns = shelley_conversation_columns(conn)?;
        let message_columns = shelley_message_columns(conn)?;
        let has_message_sequence_id = message_columns.contains("sequence_id");
        shelley_require_message_index(conn, has_message_sequence_id)?;
        let conversation_select =
            shelley_conversation_select_expressions(&conversation_columns, "c");
        let message_select = shelley_message_select_expressions(&message_columns, "m");
        let after = message_rowid.checked_sub(1);
        let Some((unit, _)) = next_message_unit(
            conn,
            &message_select,
            &conversation_select,
            has_message_sequence_id,
            after,
            Some(message_rowid),
        )?
        else {
            return Err(ShelleySourceBackedError::MissingRecord);
        };
        let ShelleyUnit::Accepted { rowid, value, .. } = unit else {
            return Err(ShelleySourceBackedError::MissingRecord);
        };
        if rowid != message_rowid
            || value.conversation.rowid != conversation_rowid
            || value.parent_bearing != parent_bearing
        {
            return Err(ShelleySourceBackedError::MissingRecord);
        }
        let values = shelley_verified_record_values(
            &value.message,
            &value.conversation,
            value.parent_bearing,
        );
        let digest = shelley_logical_record_digest(&values);
        if &digest != locator.record_digest() {
            return Err(ShelleySourceBackedError::StaleRecordEvidence);
        }
        let closing_evidence = sqlite_snapshot.finish()?;
        if closing_evidence != evidence {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        source_root.revalidate()?;
        let text = shelley_message_complete_text(&value.message)
            .unwrap_or_else(|| format!("Shelley {} message", value.message.entry_type));
        Ok(ShelleyHydratedMessage {
            text,
            native_record_digest: digest,
        })
    }
}

/// Discovers exactly `<cwd>/shelley.db` and no remembered or recursive roots.
pub(crate) fn discover_shelley_source_backed_exact_cwd(
    cwd: &Path,
) -> ShelleySourceBackedResult<Option<ShelleySourceBackedAdapter>> {
    let exact_cwd = fs::canonicalize(cwd).map_err(CaptureError::from)?;
    let cwd_metadata = fs::symlink_metadata(&exact_cwd).map_err(CaptureError::from)?;
    if !cwd_metadata.file_type().is_dir() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: exact_cwd,
            reason: "Shelley exact CWD must be a directory",
        }
        .into());
    }
    let database_path = exact_cwd.join(SHELLEY_SOURCE_ANCHOR_KEY);
    match fs::symlink_metadata(&database_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(CaptureError::Io(error).into()),
        Ok(_) => {}
    }
    // This preflight rejects symlinks and non-files before a source is admitted.
    let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(&database_path)?;
    sqlite_snapshot.finish()?;
    source_root.revalidate()?;
    let anchor = SourceAnchor::provider_native(
        SHELLEY_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(SHELLEY_SOURCE_ANCHOR_KEY)?,
    )?;
    let source = SourceKey::derive(
        ctx_history_core::CaptureProvider::Shelley.as_str(),
        SHELLEY_SQLITE_SOURCE_FORMAT,
        SHELLEY_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?;
    Ok(Some(ShelleySourceBackedAdapter {
        exact_cwd,
        database_path,
        source,
    }))
}

pub(crate) struct ShelleySourceBackedScan {
    source: SourceKey,
    evidence: SqliteSourceEvidence,
    source_root: Option<ProviderSourceRoot>,
    sqlite_snapshot: Option<SqliteSourceReadSnapshot>,
    opening_observation: SourceObservation,
    conversation_select: Vec<String>,
    message_select: Vec<String>,
    has_message_sequence_id: bool,
    context: ProviderAdapterContext,
    after_rowid: Option<i64>,
    source_exhausted: bool,
    content_digest: Sha256,
    counts: ScannedSourceCounts,
    session_lineages: HashMap<String, ShelleyDocumentLineage>,
    validated_pages: VecDeque<ShelleySourceBackedPage>,
    receipt: Option<ShelleySourceBackedReceipt>,
}

impl ShelleySourceBackedScan {
    /// Returns at most 64 native records with each retained record's full
    /// lexical body after the complete source read has finished and passed
    /// terminal revalidation.
    pub(crate) fn next_page(
        &mut self,
    ) -> ShelleySourceBackedResult<Option<ShelleySourceBackedPage>> {
        if let Some(page) = self.validated_pages.pop_front() {
            return Ok(Some(page));
        }
        if self.receipt.is_some() {
            return Ok(None);
        }
        while let Some(page) = self.next_unvalidated_page()? {
            self.validated_pages.push_back(page);
        }
        self.finalize()?;
        Ok(self.validated_pages.pop_front())
    }

    fn next_unvalidated_page(
        &mut self,
    ) -> ShelleySourceBackedResult<Option<ShelleySourceBackedPage>> {
        if self.source_exhausted {
            return Ok(None);
        }

        let mut page = ShelleySourceBackedPage {
            documents: Vec::new(),
            rejections: Vec::new(),
            counts: ScannedSourceCounts::default(),
            retained_bytes: 0,
        };
        while page.counts.complete_records < SHELLEY_PAGE_MAX_UNITS as u64 {
            let Some((unit, scanner_digest)) = next_message_unit(
                self.connection()?,
                &self.message_select,
                &self.conversation_select,
                self.has_message_sequence_id,
                self.after_rowid,
                None,
            )?
            else {
                self.source_exhausted = true;
                break;
            };
            let unit_bytes = unit.retained_bytes();
            if page.counts.complete_records != 0
                && page.retained_bytes.saturating_add(unit_bytes) > SHELLEY_PAGE_MAX_BYTES
            {
                break;
            }
            self.after_rowid = Some(unit.rowid());
            page.retained_bytes = page.retained_bytes.saturating_add(unit_bytes);
            checked_add_count(&mut page.counts.complete_records, 1)?;
            checked_add_count(
                &mut page.counts.certified_bytes,
                u64::try_from(unit_bytes).map_err(|_| ShelleySourceBackedError::CountOverflow)?,
            )?;

            match unit {
                ShelleyUnit::Rejected { rowid, reason, .. } => {
                    self.hash_record(rowid, scanner_digest, None, RecordDisposition::Rejected);
                    checked_add_count(&mut page.counts.rejected_records, 1)?;
                    page.rejections
                        .push(ShelleySourceBackedRejection { rowid, reason });
                }
                ShelleyUnit::Accepted { rowid, value, .. } => {
                    let values = shelley_verified_record_values(
                        &value.message,
                        &value.conversation,
                        value.parent_bearing,
                    );
                    let record_digest = shelley_logical_record_digest(&values);
                    let lineage = self.resolve_document_lineage(&value.conversation);
                    match lineage.and_then(|lineage| {
                        build_document(&self.source, &self.context, &value, record_digest, &lineage)
                    }) {
                        Ok(Some(document)) => {
                            self.hash_record(
                                rowid,
                                scanner_digest,
                                Some(record_digest),
                                RecordDisposition::Retained,
                            );
                            checked_add_count(&mut page.counts.retained_records, 1)?;
                            checked_add_count(&mut page.counts.indexed_documents, 1)?;
                            page.documents.push(document);
                        }
                        Ok(None) => {
                            self.hash_record(
                                rowid,
                                scanner_digest,
                                Some(record_digest),
                                RecordDisposition::Ignored,
                            );
                            checked_add_count(&mut page.counts.ignored_records, 1)?;
                        }
                        Err(
                            error @ (ShelleySourceBackedError::Projection(_)
                            | ShelleySourceBackedError::Resolver(_)
                            | ShelleySourceBackedError::MissingLexicalBody
                            | ShelleySourceBackedError::InvalidLineage(_)),
                        ) => {
                            self.hash_record(
                                rowid,
                                scanner_digest,
                                Some(record_digest),
                                RecordDisposition::Rejected,
                            );
                            checked_add_count(&mut page.counts.rejected_records, 1)?;
                            page.rejections.push(ShelleySourceBackedRejection {
                                rowid,
                                reason: error.to_string(),
                            });
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }

        if page.counts.complete_records == 0 {
            return Ok(None);
        }
        merge_counts(&mut self.counts, page.counts)?;
        Ok(Some(page))
    }

    pub(crate) fn finish(self) -> ShelleySourceBackedResult<ShelleySourceBackedReceipt> {
        if !self.validated_pages.is_empty() {
            return Err(ShelleySourceBackedError::ScanIncomplete);
        }
        self.receipt.ok_or(ShelleySourceBackedError::ScanIncomplete)
    }

    fn hash_record(
        &mut self,
        rowid: i64,
        scanner_digest: [u8; 32],
        exact_digest: Option<[u8; 32]>,
        disposition: RecordDisposition,
    ) {
        self.content_digest.update(rowid.to_be_bytes());
        self.content_digest.update(scanner_digest);
        self.content_digest.update([disposition as u8]);
        match exact_digest {
            Some(digest) => {
                self.content_digest.update([1]);
                self.content_digest.update(digest);
            }
            None => self.content_digest.update([0]),
        }
    }

    fn resolve_document_lineage(
        &mut self,
        conversation: &super::super::relationships::ShelleyConversationRow,
    ) -> ShelleySourceBackedResult<ShelleyDocumentLineage> {
        if let Some(lineage) = self.session_lineages.get(&conversation.conversation_id) {
            return Ok(lineage.clone());
        }

        let session_id = shelley_session_identity(&self.source, &conversation.conversation_id)?;
        let parent_session_id = conversation
            .parent_conversation_id
            .as_deref()
            .map(|parent| shelley_session_identity(&self.source, parent))
            .transpose()?;
        let is_primary =
            conversation.parent_conversation_id.is_none() && conversation.user_initiated;
        let agent_type = if is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        };

        let mut seen = HashSet::from([conversation.conversation_id.clone()]);
        let mut next_parent = conversation.parent_conversation_id.clone();
        let mut root_provider_session_id = conversation.conversation_id.clone();
        let mut cached_root = None;
        for _ in 0..SHELLEY_MAX_LINEAGE_DEPTH {
            let Some(parent_provider_session_id) = next_parent.take() else {
                break;
            };
            // Validate every ancestor key before retaining or reporting it.
            let _ = shelley_session_identity(&self.source, &parent_provider_session_id)?;
            if !seen.insert(parent_provider_session_id.clone()) {
                return Err(ShelleySourceBackedError::InvalidLineage(format!(
                    "cycle containing {}",
                    shelley_lineage_label(&parent_provider_session_id)
                )));
            }
            if let Some(parent_lineage) = self.session_lineages.get(&parent_provider_session_id) {
                cached_root = Some(parent_lineage.root_session_id);
                break;
            }
            root_provider_session_id = parent_provider_session_id.clone();
            next_parent = self.load_parent_conversation_id(&parent_provider_session_id)?;
        }
        if next_parent.is_some() {
            return Err(ShelleySourceBackedError::InvalidLineage(format!(
                "conversation {} exceeds the {SHELLEY_MAX_LINEAGE_DEPTH}-ancestor limit",
                shelley_lineage_label(&conversation.conversation_id)
            )));
        }
        let root_session_id = cached_root.unwrap_or(shelley_session_identity(
            &self.source,
            &root_provider_session_id,
        )?);
        let lineage = ShelleyDocumentLineage {
            session_id,
            parent_session_id,
            root_session_id,
            agent_type: agent_type.as_str().to_owned(),
            is_primary,
        };
        self.session_lineages
            .insert(conversation.conversation_id.clone(), lineage.clone());
        Ok(lineage)
    }

    fn load_parent_conversation_id(
        &self,
        provider_session_id: &str,
    ) -> ShelleySourceBackedResult<Option<String>> {
        let sql = format!(
            "select {}, {}
             from conversations c
             where typeof(c.conversation_id) = 'text' and c.conversation_id = ?1
             order by c.rowid limit 2",
            self.conversation_select[1], self.conversation_select[8],
        );
        let mut statement = self
            .connection()?
            .prepare(&sql)
            .map_err(CaptureError::from)?;
        let candidates = statement
            .query_map([provider_session_id], |row| {
                Ok((
                    row.get::<_, rusqlite::types::Value>(0)?,
                    row.get::<_, rusqlite::types::Value>(1)?,
                ))
            })
            .map_err(CaptureError::from)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(CaptureError::from)?;
        let [(resolved_provider_session_id, parent_provider_session_id)] = candidates.as_slice()
        else {
            let issue = if candidates.is_empty() {
                "missing"
            } else {
                "duplicate"
            };
            return Err(ShelleySourceBackedError::InvalidLineage(format!(
                "{issue} ancestor conversation {}",
                shelley_lineage_label(provider_session_id)
            )));
        };
        let rusqlite::types::Value::Text(resolved_provider_session_id) =
            resolved_provider_session_id
        else {
            return Err(ShelleySourceBackedError::InvalidLineage(
                "ancestor conversation ID is not text".to_owned(),
            ));
        };
        if resolved_provider_session_id.as_str() != provider_session_id {
            return Err(ShelleySourceBackedError::InvalidLineage(format!(
                "ancestor conversation key mismatch for {}",
                shelley_lineage_label(provider_session_id)
            )));
        }
        match parent_provider_session_id {
            rusqlite::types::Value::Null => Ok(None),
            rusqlite::types::Value::Text(parent_provider_session_id) => {
                Ok(Some(parent_provider_session_id.clone()))
            }
            _ => Err(ShelleySourceBackedError::InvalidLineage(format!(
                "ancestor conversation {} has a non-text parent ID",
                shelley_lineage_label(provider_session_id)
            ))),
        }
    }

    fn finalize(&mut self) -> ShelleySourceBackedResult<()> {
        let sqlite_snapshot = self
            .sqlite_snapshot
            .take()
            .ok_or(ShelleySourceBackedError::ScanIncomplete)?;
        let closing_evidence = sqlite_snapshot.finish()?;
        if closing_evidence != self.evidence {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        self.source_root
            .take()
            .ok_or(ShelleySourceBackedError::ScanIncomplete)?
            .revalidate()?;
        let mut content_digest = self.content_digest.clone();
        hash_counts(&mut content_digest, self.counts);
        let certificate = CertifiedSource::certify(
            self.opening_observation.clone(),
            self.opening_observation.clone(),
            SHELLEY_SOURCE_PARSER_REVISION,
            content_digest.finalize().into(),
            self.counts,
        )?;
        self.receipt = Some(ShelleySourceBackedReceipt { certificate });
        Ok(())
    }

    fn connection(&self) -> ShelleySourceBackedResult<&rusqlite::Connection> {
        self.sqlite_snapshot
            .as_ref()
            .ok_or(ShelleySourceBackedError::ScanIncomplete)?
            .connection()
            .map_err(Into::into)
    }
}

#[derive(Debug)]
pub(crate) struct ShelleySourceBackedPage {
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) rejections: Vec<ShelleySourceBackedRejection>,
    pub(crate) counts: ScannedSourceCounts,
    pub(crate) retained_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShelleySourceBackedRejection {
    pub(crate) rowid: i64,
    pub(crate) reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ShelleySourceBackedReceipt {
    pub(crate) certificate: CertifiedSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShelleyHydratedMessage {
    pub(crate) text: String,
    pub(crate) native_record_digest: [u8; 32],
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum RecordDisposition {
    Retained = 1,
    Rejected = 2,
    Ignored = 3,
}

#[derive(Debug, Clone)]
struct ShelleyDocumentLineage {
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    agent_type: String,
    is_primary: bool,
}

fn shelley_session_identity(
    source: &SourceKey,
    provider_session_id: &str,
) -> ShelleySourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        SHELLEY_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id.to_owned())?,
    )?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: SHELLEY_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })
    .map_err(Into::into)
}

fn open_root_authorized_snapshot(
    path: &Path,
) -> ShelleySourceBackedResult<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook(path, || {})
}

fn open_root_authorized_snapshot_with_hook(
    path: &Path,
    after_authorize: impl FnOnce(),
) -> ShelleySourceBackedResult<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
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
    let sqlite_authority = retain_sqlite_source_directory_authority(&parent_handle, parent)?;
    let sqlite_snapshot =
        open_root_handle_sqlite_source_snapshot(&sqlite_authority, database_leaf)?;
    after_authorize();
    source_directory.revalidate()?;
    source_root.revalidate()?;
    let connection = sqlite_snapshot.connection()?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| ShelleySourceBackedError::CountOverflow)?;
    connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(CaptureError::from)?;
    Ok((source_root, sqlite_snapshot))
}

fn shelley_source_revision(
    evidence: &SqliteSourceEvidence,
    user_version: i64,
    schema_fingerprint: &str,
) -> String {
    format!(
        "shelley-sqlite-snapshot-v1:capture={SHELLEY_CAPTURE_REVISION};policy={SHELLEY_POLICY_REVISION};user_version={user_version};schema={schema_fingerprint};identity={};length={};revision={}",
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

fn shelley_lineage_label(provider_session_id: &str) -> String {
    provider_session_id
        .chars()
        .take(SHELLEY_LINEAGE_LABEL_MAX_CHARS)
        .collect()
}

fn build_document(
    source: &SourceKey,
    context: &ProviderAdapterContext,
    value: &ShelleyMessage,
    record_digest: [u8; 32],
    lineage: &ShelleyDocumentLineage,
) -> ShelleySourceBackedResult<Option<LexicalDocument>> {
    let native_body = shelley_message_body(&value.message);
    let event_type = shelley_event_type(&value.message, &native_body);
    if shelley_output_classification(&value.message)
        .as_ref()
        .is_some_and(|classification| {
            !matches!(
                classification.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            )
        })
    {
        return Ok(None);
    }
    let role = shelley_event_role(&value.message.entry_type);
    let native_item_key = NativeItemKey::composite(
        SHELLEY_NATIVE_MESSAGE_NAMESPACE,
        vec![
            TypedKey::utf8(value.message.conversation_id.clone())?,
            TypedKey::I64(value.message.sequence_id),
            TypedKey::utf8(value.message.message_id.clone())?,
        ],
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: lineage.session_id,
        logical_item_kind: SHELLEY_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let primary_key = TypedKey::composite(vec![
        TypedKey::Bool(value.parent_bearing),
        TypedKey::I64(value.message.rowid),
        TypedKey::I64(value.conversation.rowid),
    ])?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: SHELLEY_COMPOUND_LOCATOR_RELATION.to_owned(),
            primary_key,
            row_version: None,
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        record_digest,
    )?;
    let body = shelley_message_complete_text(&value.message)
        .unwrap_or_else(|| format!("Shelley {} message", value.message.entry_type));
    if body.trim().is_empty() {
        return Err(ShelleySourceBackedError::MissingLexicalBody);
    }
    let cwd = value.conversation.cwd.clone();
    let started_at = shelley_timestamp(
        value.conversation.created_at.as_deref(),
        chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
    );
    let occurred_at = shelley_timestamp(value.message.created_at.as_deref(), started_at);
    Ok(Some(LexicalDocument {
        event_id,
        session_id: lineage.session_id,
        parent_session_id: lineage.parent_session_id,
        root_session_id: lineage.root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(value.message.conversation_id.clone()),
        branch: None,
        source_path: context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string()),
        agent_type: lineage.agent_type.clone(),
        is_primary: lineage.is_primary,
        event_sequence: value.provider_event_index,
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        event_type: event_type.as_str().to_owned(),
        role: role.map(|role| role.as_str().to_owned()),
        body,
        workspace: cwd.clone(),
        cwd,
        touched_files: Vec::new(),
    }))
}

fn decode_compound_locator(
    locator: &SourceRecordLocator,
) -> ShelleySourceBackedResult<(bool, i64, i64)> {
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(ShelleySourceBackedError::InvalidLocator);
    };
    if logical_relation != SHELLEY_COMPOUND_LOCATOR_RELATION || row_version.is_some() {
        return Err(ShelleySourceBackedError::InvalidLocator);
    }
    let TypedKey::Composite(parts) = primary_key else {
        return Err(ShelleySourceBackedError::InvalidLocator);
    };
    let [TypedKey::Bool(parent_bearing), TypedKey::I64(message_rowid), TypedKey::I64(conversation_rowid)] =
        parts.as_slice()
    else {
        return Err(ShelleySourceBackedError::InvalidLocator);
    };
    Ok((*parent_bearing, *message_rowid, *conversation_rowid))
}

fn checked_add_count(target: &mut u64, value: u64) -> ShelleySourceBackedResult<()> {
    *target = target
        .checked_add(value)
        .ok_or(ShelleySourceBackedError::CountOverflow)?;
    Ok(())
}

fn merge_counts(
    target: &mut ScannedSourceCounts,
    page: ScannedSourceCounts,
) -> ShelleySourceBackedResult<()> {
    checked_add_count(&mut target.complete_records, page.complete_records)?;
    checked_add_count(&mut target.retained_records, page.retained_records)?;
    checked_add_count(&mut target.rejected_records, page.rejected_records)?;
    checked_add_count(&mut target.ignored_records, page.ignored_records)?;
    checked_add_count(&mut target.indexed_documents, page.indexed_documents)?;
    checked_add_count(&mut target.certified_bytes, page.certified_bytes)
}

fn hash_counts(digest: &mut Sha256, counts: ScannedSourceCounts) {
    digest.update(counts.complete_records.to_be_bytes());
    digest.update(counts.retained_records.to_be_bytes());
    digest.update(counts.rejected_records.to_be_bytes());
    digest.update(counts.ignored_records.to_be_bytes());
    digest.update(counts.indexed_documents.to_be_bytes());
    digest.update(counts.certified_bytes.to_be_bytes());
}

#[cfg(test)]
mod tests;
