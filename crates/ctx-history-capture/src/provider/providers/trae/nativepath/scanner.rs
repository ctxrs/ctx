use std::{
    collections::BTreeMap,
    ffi::OsString,
    ops::Range,
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType};
use rusqlite::Connection;
use serde::de::IgnoredAny;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    common::io::{ProviderSourceDirectory, ProviderSourceRoot},
    provider::{
        native_ingestion::{NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS},
        normalization::provider_role,
        sqlite::{
            sqlite_schema_fingerprint, sqlite_table_columns, sqlite_table_exists,
            SqliteLengthPreflightGuard,
        },
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
        SqliteSourceReadSnapshot,
    },
    CaptureError, ProviderImportFailure, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::super::{
    event::{trae_event_from_owned_message, trae_message_is_output},
    json_stream::{
        trae_session_selection, trae_stream_session, TraeJsonArrayValues, TraeJsonContainerValues,
        TraeSessionSelection, TraeStreamSession,
    },
    trae_sqlite_value_fits_parser_bound,
    workspace::{trae_workspace_folder, trae_workspace_id},
    TRAE_CHAT_KEYS, TRAE_CHAT_ROWS_QUERY, TRAE_SQLITE_VALUE_OVERHEAD_BYTES,
};

const TRAE_SOURCE_PARSER_REVISION: &str = "trae-nativepath-parser-v2";
const TRAE_SOURCE_POLICY_REVISION: &str = "trae-nativepath-core-policy-v2";
const TRAE_PAGE_UNIT_LIMIT: usize = NATIVE_INGESTION_PAGE_MAX_UNITS - 8;
const TRAE_PAGE_BYTE_LIMIT: usize = NATIVE_INGESTION_PAGE_MAX_BYTES - 512 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct TraeFrontier {
    key_index: u16,
    session_index: u32,
    message_index: u32,
}

impl TraeFrontier {
    fn terminal() -> Self {
        Self {
            key_index: u16::try_from(TRAE_CHAT_KEYS.len()).unwrap_or(u16::MAX),
            session_index: 0,
            message_index: 0,
        }
    }

    fn is_terminal(self) -> bool {
        usize::from(self.key_index) >= TRAE_CHAT_KEYS.len()
    }
}

pub(super) struct TraeSourceAuthority {
    pub(super) database: TraeSqliteDatabase,
    pub(super) workspace_id: String,
    pub(super) workspace_folder: Option<String>,
    pub(super) schema_evidence: Vec<u8>,
    pub(super) logical_fingerprint: [u8; 32],
    observed_keys: Vec<Option<TraeObservedKey>>,
    observed_at: DateTime<Utc>,
}

struct TraeObservedKey {
    value_type: String,
    retained_bytes: i64,
    value: Option<Vec<u8>>,
}

pub(super) struct TraeSqliteDatabase {
    parent: ProviderSourceDirectory,
    _authority: SqliteSourceDirectoryAuthority,
    database_name: OsString,
    evidence: SqliteSourceEvidence,
    snapshot: Mutex<Option<SqliteSourceReadSnapshot>>,
}

impl TraeSqliteDatabase {
    pub(super) fn open<T>(
        data_root: &Path,
        path: &Path,
        query: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<(Self, T)> {
        let parent_path =
            path.parent()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "Trae SQLite source must have a parent directory",
                })?;
        let database_name = path
            .file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Trae SQLite source must have a database leaf name",
            })?
            .to_os_string();
        let parent = ProviderSourceRoot::open(parent_path)?.directory()?;
        let authority_handle = parent.try_clone_authority_handle()?;
        let authority =
            retain_sqlite_source_directory_authority(data_root, &authority_handle, parent_path)
                .map_err(|error| trae_sqlite_source_error(path, error))?;
        let snapshot = open_root_handle_sqlite_source_snapshot(&authority, &database_name)
            .map_err(|error| trae_sqlite_source_error(path, error))?;
        let evidence = snapshot.evidence().clone();
        let result = snapshot
            .connection()
            .map_err(|error| trae_sqlite_source_error(path, error))
            .and_then(query);
        let database = Self {
            parent,
            _authority: authority,
            database_name,
            evidence,
            snapshot: Mutex::new(Some(snapshot)),
        };
        database.revalidate()?;
        Ok((database, result?))
    }

    #[cfg(test)]
    pub(super) fn read<T>(
        &self,
        path: &Path,
        query: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        self.read_provider(path, query)
    }

    pub(super) fn read_provider<T, E>(
        &self,
        path: &Path,
        query: impl FnOnce(&Connection) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<CaptureError>,
    {
        self.revalidate().map_err(E::from)?;
        let retained = self.snapshot.lock().map_err(|_| {
            E::from(CaptureError::ProviderSource {
                provider: CaptureProvider::Trae.as_str(),
                path: path.to_path_buf(),
                kind: crate::ProviderSourceFailureKind::SourceDatabase,
                detail: "Trae SQLite snapshot lock was poisoned".to_owned(),
            })
        })?;
        let snapshot = retained
            .as_ref()
            .ok_or_else(|| E::from(CaptureError::SourceChangedDuringCapture))?;
        snapshot
            .revalidate()
            .map_err(|error| E::from(trae_sqlite_source_error(path, error)))?;
        if snapshot.evidence() != &self.evidence {
            return Err(E::from(CaptureError::SourceChangedDuringCapture));
        }
        snapshot
            .connection()
            .map_err(|error| E::from(trae_sqlite_source_error(path, error)))
            .and_then(query)
    }

    pub(super) fn revalidate(&self) -> Result<()> {
        self.parent.revalidate()?;
        self.parent.authority_root().revalidate()
    }

    pub(super) fn evidence(&self) -> &SqliteSourceEvidence {
        &self.evidence
    }

    pub(super) fn terminal_revalidator(
        &self,
    ) -> Result<
        Box<dyn Fn() -> std::result::Result<(), SqliteSourceAccessError> + Send + Sync + 'static>,
    > {
        let retained = self
            .snapshot
            .lock()
            .map_err(|_| CaptureError::ProviderSource {
                provider: CaptureProvider::Trae.as_str(),
                path: PathBuf::from(&self.database_name),
                kind: crate::ProviderSourceFailureKind::SourceDatabase,
                detail: "Trae SQLite snapshot lock was poisoned".to_owned(),
            })?;
        retained
            .as_ref()
            .map(SqliteSourceReadSnapshot::terminal_revalidator)
            .ok_or(CaptureError::SourceChangedDuringCapture)
    }

    pub(super) fn seal(&self, path: &Path) -> Result<SqliteSourceEvidence> {
        self.seal_if_active(path)?
            .ok_or(CaptureError::SourceChangedDuringCapture)
    }

    pub(super) fn seal_if_active(&self, path: &Path) -> Result<Option<SqliteSourceEvidence>> {
        let Some(snapshot) = self
            .snapshot
            .lock()
            .map_err(|_| CaptureError::ProviderSource {
                provider: CaptureProvider::Trae.as_str(),
                path: path.to_path_buf(),
                kind: crate::ProviderSourceFailureKind::SourceDatabase,
                detail: "Trae SQLite snapshot lock was poisoned".to_owned(),
            })?
            .take()
        else {
            return Ok(None);
        };
        let closing_evidence = snapshot
            .finish()
            .map_err(|error| trae_sqlite_source_error(path, error))?;
        self.revalidate()?;
        if closing_evidence != self.evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(Some(closing_evidence))
    }
}

fn trae_sqlite_source_error(path: &Path, error: SqliteSourceAccessError) -> CaptureError {
    match error {
        SqliteSourceAccessError::SourceChanged
        | SqliteSourceAccessError::ConnectionIdentityMismatch => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::ProviderSource {
            provider: CaptureProvider::Trae.as_str(),
            path: path.to_path_buf(),
            kind: crate::ProviderSourceFailureKind::SourceDatabase,
            detail: error.to_string(),
        },
    }
}

struct TraeSessionPlan {
    session: TraeStreamSession,
    raw_session_index: u32,
    messages: Vec<Range<usize>>,
}

struct TraeActiveKey {
    key_index: u16,
    chat_key: &'static str,
    bytes: Vec<u8>,
    value_digest: [u8; 32],
    sessions: Vec<TraeSessionPlan>,
}

pub(super) struct TraeScanner<'a> {
    authority: &'a TraeSourceAuthority,
    frontier: TraeFrontier,
    active: Option<TraeActiveKey>,
    source_content_hasher: Sha256,
    certified_source_bytes: u64,
    decoded_rows: u64,
}

pub(super) struct TraeCoreRecord {
    pub(super) provider_session_id: String,
    pub(super) native_session_id: String,
    pub(super) native_session_id_from_provider: bool,
    pub(super) native_message_id: String,
    pub(super) native_message_id_from_provider: bool,
    pub(super) chat_key: &'static str,
    pub(super) value_digest: [u8; 32],
    pub(super) key_index: u16,
    pub(super) raw_session_index: u32,
    pub(super) message_index: u32,
    pub(super) lexical_text: String,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
}

pub(super) struct TraeScanPage {
    pub(super) terminal: bool,
    pub(super) logical_units: usize,
    estimated_bytes: usize,
    pub(super) core: Vec<TraeCoreRecord>,
    pub(super) rejections: Vec<ProviderImportFailure>,
}

enum TraeLoadedKey {
    Missing,
    Rejected(String),
    Active(TraeActiveKey),
}

pub(super) fn acquire_source(
    data_root: &Path,
    path: &Path,
    observed_at: DateTime<Utc>,
) -> Result<TraeSourceAuthority> {
    let (database, (schema, user_version)) = TraeSqliteDatabase::open(data_root, path, |conn| {
        validate_schema(conn, path)?;
        Ok((
            sqlite_schema_fingerprint(conn)?,
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
        ))
    })?;
    let schema_evidence = format!(
        "trae-logical-schema-v1;parser={TRAE_SOURCE_PARSER_REVISION};\
         policy={TRAE_SOURCE_POLICY_REVISION};user_version={user_version};schema={schema}"
    )
    .into_bytes();
    let (logical_fingerprint, observed_keys) = database.read_provider(path, |conn| {
        trae_logical_observation(conn, &schema_evidence)
    })?;
    Ok(TraeSourceAuthority {
        database,
        workspace_id: trae_workspace_id(path),
        workspace_folder: trae_workspace_folder(path),
        schema_evidence,
        logical_fingerprint,
        observed_keys,
        observed_at,
    })
}

fn trae_logical_observation(
    conn: &Connection,
    schema_evidence: &[u8],
) -> Result<([u8; 32], Vec<Option<TraeObservedKey>>)> {
    let parser_bound = i64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Trae JSON bound exceeds i64"))?;
    let parser_overhead = i64::try_from(TRAE_SQLITE_VALUE_OVERHEAD_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Trae SQLite overhead exceeds i64"))?;
    let _guard = SqliteLengthPreflightGuard::new(conn);
    let mut statement = conn.prepare(TRAE_CHAT_ROWS_QUERY)?;
    let mut rows = statement.query(rusqlite::params![
        TRAE_CHAT_KEYS[0],
        TRAE_CHAT_KEYS[1],
        TRAE_CHAT_KEYS[2],
        TRAE_CHAT_KEYS[3],
        TRAE_CHAT_KEYS[4],
        TRAE_CHAT_KEYS[5],
        parser_overhead,
        parser_bound,
    ])?;
    let mut observed = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let chat_key = row.get::<_, String>(0)?;
        let cardinality = row.get::<_, i64>(1)?;
        if cardinality != 1 {
            return Err(CaptureError::InvalidPayload(format!(
                "Trae ItemTable key `{chat_key}` appears {cardinality} times"
            )));
        }
        observed.insert(
            chat_key,
            (
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?.map(String::into_bytes),
            ),
        );
    }
    let mut digest = Sha256::new();
    digest.update(b"ctx-trae-logical-snapshot-v1\0");
    digest.update((schema_evidence.len() as u64).to_be_bytes());
    digest.update(schema_evidence);
    let mut observed_keys = Vec::with_capacity(TRAE_CHAT_KEYS.len());
    for chat_key in TRAE_CHAT_KEYS {
        digest.update((chat_key.len() as u64).to_be_bytes());
        digest.update(chat_key.as_bytes());
        match observed.remove(*chat_key) {
            Some((value_type, retained_bytes, value)) => {
                digest.update([1]);
                digest.update((value_type.len() as u64).to_be_bytes());
                digest.update(value_type.as_bytes());
                digest.update(retained_bytes.to_be_bytes());
                if let Some(value) = value.as_deref() {
                    digest.update([1]);
                    digest.update((value.len() as u64).to_be_bytes());
                    digest.update(value);
                } else {
                    digest.update([0]);
                }
                observed_keys.push(Some(TraeObservedKey {
                    value_type,
                    retained_bytes,
                    value,
                }));
            }
            None => {
                digest.update([0]);
                observed_keys.push(None);
            }
        }
    }
    Ok((digest.finalize().into(), observed_keys))
}

pub(super) fn validate_schema(conn: &Connection, path: &Path) -> Result<()> {
    if !sqlite_table_exists(conn, "ItemTable")? {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Trae state.vscdb is missing ItemTable",
        });
    }
    let columns = sqlite_table_columns(conn, "ItemTable")?;
    if ["key", "value"]
        .iter()
        .all(|column| columns.contains(*column))
    {
        Ok(())
    } else {
        Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Trae ItemTable is missing required key/value columns",
        })
    }
}

impl<'a> TraeScanner<'a> {
    pub(super) fn new(authority: &'a TraeSourceAuthority, frontier: TraeFrontier) -> Self {
        Self {
            authority,
            frontier,
            active: None,
            source_content_hasher: Sha256::new(),
            certified_source_bytes: 0,
            decoded_rows: 0,
        }
    }

    pub(super) fn next_page(&mut self) -> Result<Option<TraeScanPage>> {
        if self.frontier.is_terminal() {
            return Ok(None);
        }
        let mut page = TraeScanPage {
            terminal: false,
            logical_units: 0,
            estimated_bytes: 0,
            core: Vec::new(),
            rejections: Vec::new(),
        };
        while page.logical_units < TRAE_PAGE_UNIT_LIMIT
            && page.estimated_bytes < TRAE_PAGE_BYTE_LIMIT
            && !self.frontier.is_terminal()
        {
            if self
                .active
                .as_ref()
                .is_none_or(|active| active.key_index != self.frontier.key_index)
            {
                self.active = None;
                match self.load_key(self.frontier.key_index)? {
                    TraeLoadedKey::Missing => {
                        self.advance_key()?;
                        continue;
                    }
                    TraeLoadedKey::Rejected(error) => {
                        page.logical_units = page.logical_units.saturating_add(1);
                        page.estimated_bytes = page
                            .estimated_bytes
                            .saturating_add(error.len())
                            .saturating_add(128);
                        page.rejections.push(ProviderImportFailure {
                            line: packed_native_index(
                                self.frontier.key_index,
                                self.frontier.session_index,
                                self.frontier.message_index,
                            )
                            .unwrap_or(u64::MAX) as usize,
                            error,
                        });
                        self.advance_key()?;
                        continue;
                    }
                    TraeLoadedKey::Active(active) => self.active = Some(active),
                }
            }
            let active = self.active.as_ref().ok_or(CaptureError::SystemInvariant(
                "Trae active ItemTable key is unavailable",
            ))?;
            let session_index = usize::try_from(self.frontier.session_index).map_err(|_| {
                CaptureError::InvalidPayload("Trae session frontier exceeds platform limits".into())
            })?;
            let Some(session_plan) = active.sessions.get(session_index) else {
                self.advance_key()?;
                continue;
            };
            let message_index = usize::try_from(self.frontier.message_index).map_err(|_| {
                CaptureError::InvalidPayload("Trae message frontier exceeds platform limits".into())
            })?;
            let Some(range) = session_plan.messages.get(message_index).cloned() else {
                self.frontier.session_index = self.frontier.session_index.checked_add(1).ok_or(
                    CaptureError::SystemInvariant("Trae session frontier exhausted"),
                )?;
                self.frontier.message_index = 0;
                continue;
            };
            let message: Value = match serde_json::from_slice(&active.bytes[range]) {
                Ok(message) => message,
                Err(error) => {
                    page.rejections.push(ProviderImportFailure {
                        line: packed_native_index(
                            self.frontier.key_index,
                            self.frontier.session_index,
                            self.frontier.message_index,
                        )
                        .unwrap_or(u64::MAX) as usize,
                        error: format!(
                            "Trae ItemTable key `{}` message is invalid JSON: {error}",
                            active.chat_key
                        ),
                    });
                    page.logical_units = page.logical_units.saturating_add(1);
                    page.estimated_bytes = page.estimated_bytes.saturating_add(256);
                    self.advance_message()?;
                    continue;
                }
            };
            let output = trae_message_is_output(&message);
            let provider_session_id = format!(
                "{}/{}",
                self.authority.workspace_id, session_plan.session.native_session_id
            );
            let Some(event) = trae_event_from_owned_message(
                &provider_session_id,
                &self.authority.workspace_id,
                active.chat_key,
                message,
                message_index,
                self.authority.observed_at,
            ) else {
                page.logical_units = page.logical_units.saturating_add(1);
                self.advance_message()?;
                continue;
            };
            if output {
                page.estimated_bytes = page.estimated_bytes.saturating_add(256);
            } else {
                page.estimated_bytes = page
                    .estimated_bytes
                    .saturating_add(event.text.len())
                    .saturating_add(4096);
                page.core.push(TraeCoreRecord {
                    provider_session_id,
                    native_session_id: session_plan.session.native_session_id.clone(),
                    native_session_id_from_provider: session_plan
                        .session
                        .native_session_id_from_provider,
                    native_message_id: event.native_message_id,
                    native_message_id_from_provider: event.native_message_id_from_provider,
                    chat_key: active.chat_key,
                    value_digest: active.value_digest,
                    key_index: self.frontier.key_index,
                    raw_session_index: session_plan.raw_session_index,
                    message_index: self.frontier.message_index,
                    lexical_text: event.text,
                    occurred_at: event.occurred_at,
                    event_type: EventType::Message,
                    role: Some(provider_role(event.role.as_deref())),
                });
            }
            page.logical_units = page.logical_units.saturating_add(1);
            self.advance_message()?;
        }
        self.frontier = normalize_frontier(self.frontier, self.active.as_ref())?;
        page.terminal = self.frontier.is_terminal();
        page.estimated_bytes = page.estimated_bytes.saturating_add(4096);
        if page.estimated_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Trae source-backed page exceeds retained-byte bounds".into(),
            ));
        }
        Ok(Some(page))
    }

    fn load_key(&mut self, key_index: u16) -> Result<TraeLoadedKey> {
        let Some(chat_key) = TRAE_CHAT_KEYS.get(usize::from(key_index)).copied() else {
            return Ok(TraeLoadedKey::Missing);
        };
        let Some(observed) = self
            .authority
            .observed_keys
            .get(usize::from(key_index))
            .and_then(Option::as_ref)
        else {
            return Ok(TraeLoadedKey::Missing);
        };
        self.decoded_rows =
            self.decoded_rows
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Trae decoded-row counter overflow",
                ))?;
        let retained_bytes = u64::try_from(observed.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload("Trae ItemTable value length is negative".into())
        })?;
        if !trae_sqlite_value_fits_parser_bound(chat_key, retained_bytes) {
            return Ok(TraeLoadedKey::Rejected(format!(
                "Trae ItemTable key `{chat_key}` exceeds the provider JSON bound"
            )));
        }
        if observed.value_type != "text" {
            return Ok(TraeLoadedKey::Rejected(format!(
                "Trae ItemTable key `{chat_key}` has unsupported SQLite type `{}`",
                observed.value_type
            )));
        }
        let bytes = observed.value.clone().ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "Trae ItemTable key `{chat_key}` exceeded its bounded observation"
            ))
        })?;
        if bytes.len() != usize::try_from(retained_bytes).unwrap_or(usize::MAX) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let key_bytes = u64::try_from(chat_key.len())
            .map_err(|_| CaptureError::SystemInvariant("Trae chat key length overflow"))?;
        let value_bytes = u64::try_from(bytes.len())
            .map_err(|_| CaptureError::SystemInvariant("Trae value length overflow"))?;
        self.source_content_hasher.update(key_bytes.to_be_bytes());
        self.source_content_hasher.update(chat_key.as_bytes());
        self.source_content_hasher.update(value_bytes.to_be_bytes());
        self.source_content_hasher.update(&bytes);
        self.certified_source_bytes = self
            .certified_source_bytes
            .checked_add(16)
            .and_then(|total| total.checked_add(key_bytes))
            .and_then(|total| total.checked_add(value_bytes))
            .ok_or(CaptureError::SystemInvariant(
                "Trae certified source byte count overflow",
            ))?;
        if let Err(error) = serde_json::from_slice::<IgnoredAny>(&bytes) {
            return Ok(TraeLoadedKey::Rejected(format!(
                "Trae ItemTable key `{chat_key}` contains invalid JSON: {error}"
            )));
        }
        let sessions = match trae_session_selection(&bytes, chat_key) {
            Ok(None) => Vec::new(),
            Ok(Some(TraeSessionSelection::CnMessages(messages))) => vec![session_plan(
                &bytes,
                TraeStreamSession {
                    native_session_id: "trae-cn-input-history".to_owned(),
                    native_session_id_from_provider: true,
                    messages,
                },
                0,
            )?],
            Ok(Some(TraeSessionSelection::Sessions(container))) => {
                let mut values = TraeJsonContainerValues::new(&bytes, container)?;
                let mut sessions = Vec::new();
                let mut session_index = 0_usize;
                while let Some(range) = values.next_range()? {
                    if let Some(session) = trae_stream_session(&bytes, range, session_index)? {
                        let raw_session_index = u32::try_from(session_index).map_err(|_| {
                            CaptureError::InvalidPayload(
                                "Trae raw session ordinal exceeds u32".into(),
                            )
                        })?;
                        sessions.push(session_plan(&bytes, session, raw_session_index)?);
                    }
                    session_index = session_index.saturating_add(1);
                }
                sessions
            }
            Err(error) => {
                return Ok(TraeLoadedKey::Rejected(format!(
                    "Trae ItemTable key `{chat_key}` cannot be decoded: {error}"
                )));
            }
        };
        let value_digest: [u8; 32] = Sha256::digest(&bytes).into();
        Ok(TraeLoadedKey::Active(TraeActiveKey {
            key_index,
            chat_key,
            value_digest,
            bytes,
            sessions,
        }))
    }

    fn advance_key(&mut self) -> Result<()> {
        self.frontier.key_index = self
            .frontier
            .key_index
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant("Trae key frontier exhausted"))?;
        self.frontier.session_index = 0;
        self.frontier.message_index = 0;
        self.active = None;
        Ok(())
    }

    fn advance_message(&mut self) -> Result<()> {
        self.frontier.message_index =
            self.frontier
                .message_index
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Trae message frontier exhausted",
                ))?;
        Ok(())
    }

    pub(super) fn source_content_digest(&self) -> [u8; 32] {
        self.source_content_hasher.clone().finalize().into()
    }

    pub(super) fn certified_source_bytes(&self) -> u64 {
        self.certified_source_bytes
    }

    pub(super) fn decoded_rows(&self) -> u64 {
        self.decoded_rows
    }
}

fn normalize_frontier(
    mut frontier: TraeFrontier,
    active: Option<&TraeActiveKey>,
) -> Result<TraeFrontier> {
    if frontier.is_terminal() {
        return Ok(TraeFrontier::terminal());
    }
    let Some(active) = active.filter(|active| active.key_index == frontier.key_index) else {
        return Ok(frontier);
    };
    loop {
        let session_index = usize::try_from(frontier.session_index).map_err(|_| {
            CaptureError::InvalidPayload("Trae session frontier exceeds platform limits".into())
        })?;
        let Some(session) = active.sessions.get(session_index) else {
            frontier.key_index = frontier
                .key_index
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant("Trae key frontier exhausted"))?;
            frontier.session_index = 0;
            frontier.message_index = 0;
            return Ok(if frontier.is_terminal() {
                TraeFrontier::terminal()
            } else {
                frontier
            });
        };
        if usize::try_from(frontier.message_index).unwrap_or(usize::MAX) < session.messages.len() {
            return Ok(frontier);
        }
        frontier.session_index =
            frontier
                .session_index
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Trae session frontier exhausted",
                ))?;
        frontier.message_index = 0;
    }
}

fn session_plan(
    bytes: &[u8],
    session: TraeStreamSession,
    raw_session_index: u32,
) -> Result<TraeSessionPlan> {
    let mut values = TraeJsonArrayValues::new(bytes, session.messages.clone())?;
    let mut messages = Vec::new();
    while let Some(range) = values.next_range()? {
        messages.push(range);
    }
    Ok(TraeSessionPlan {
        session,
        raw_session_index,
        messages,
    })
}

pub(super) fn packed_native_index(key: u16, session: u32, message: u32) -> Result<u64> {
    if session > 0x00ff_ffff || message > 0x00ff_ffff {
        return Err(CaptureError::InvalidPayload(
            "Trae native message coordinate exceeds packed identity bounds".into(),
        ));
    }
    Ok((u64::from(key) << 48) | (u64::from(session) << 24) | u64::from(message))
}

pub(super) fn absolute_trae_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, ffi::OsString, fs, path::Path};

    use rusqlite::{config::DbConfig, params, Connection};

    use super::{acquire_source, TraeFrontier, TraeScanner, TraeSqliteDatabase};
    use crate::CaptureError;

    #[test]
    fn primary_key_itemtable_remains_importable() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let data_root = crate::test_support_paths::tempdir().unwrap();
        let source = temp.path().join("state.vscdb");
        let connection = Connection::open(&source).unwrap();
        connection
            .execute(
                "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
                params![
                    crate::provider::providers::trae::TRAE_CHAT_KEYS[0],
                    r#"{"list":[{"id":"supported","messages":[{"content":"hello"}]}]}"#,
                ],
            )
            .unwrap();
        drop(connection);

        let authority = acquire_source(
            data_root.path(),
            &source,
            chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        )
        .unwrap();
        let mut scanner = TraeScanner::new(&authority, TraeFrontier::default());
        let page = scanner.next_page().unwrap().unwrap();
        assert_eq!(page.core.len(), 1);
        assert!(page.rejections.is_empty());
    }

    #[test]
    fn duplicate_known_itemtable_keys_are_typed_invalid_payload() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let data_root = crate::test_support_paths::tempdir().unwrap();
        let source = temp.path().join("state.vscdb");
        let connection = Connection::open(&source).unwrap();
        connection
            .execute("CREATE TABLE ItemTable ([key] TEXT, value TEXT)", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2), (?1, ?3)",
                params![
                    crate::provider::providers::trae::TRAE_CHAT_KEYS[0],
                    r#"{"list":[{"id":"supported","messages":[{"content":"hello"}]}]}"#,
                    r#"{"list":[]}"#,
                ],
            )
            .unwrap();
        drop(connection);

        let error = match acquire_source(
            data_root.path(),
            &source,
            chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        ) {
            Ok(_) => panic!("duplicate known Trae keys must be rejected before import"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            CaptureError::InvalidPayload(detail)
                if detail == "Trae ItemTable key `memento/icube-ai-agent-storage` appears 2 times"
        ));
    }

    #[test]
    fn stock_snapshot_queries_active_wal_without_persistent_writes_and_rejects_swap() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source = temp.path().join("trae.sqlite");
        let attacker = temp.path().join("attacker.sqlite");
        let admitted = temp.path().join("admitted.sqlite");
        create_database(&source, "main");
        create_database(&attacker, "attacker");
        persist_wal_row(&source, "from-wal");
        let before_read = persistent_directory_snapshot(temp.path());

        let (database, opened_value) = TraeSqliteDatabase::open(
            crate::test_provider_sqlite_data_root(),
            &source,
            read_latest,
        )
        .unwrap();
        assert_eq!(opened_value, "from-wal");
        assert!(database.evidence().wal_length().is_some());
        assert!(database.evidence().shared_memory_length().is_some());
        assert_eq!(database.read(&source, read_latest).unwrap(), "from-wal");
        assert_eq!(persistent_directory_snapshot(temp.path()), before_read);

        fs::rename(&source, &admitted).unwrap();
        fs::rename(&attacker, &source).unwrap();
        let before_rejected_read = persistent_directory_snapshot(temp.path());
        let queried = Cell::new(false);
        let result = database.read(&source, |_| -> crate::Result<()> {
            queried.set(true);
            Ok(())
        });
        assert!(result.is_err());
        assert!(!queried.get());
        assert_eq!(
            persistent_directory_snapshot(temp.path()),
            before_rejected_read
        );
    }

    fn create_database(path: &Path, value: &str) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        connection
            .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
            .unwrap();
    }

    fn persist_wal_row(path: &Path, value: &str) {
        let writer = Connection::open(path).unwrap();
        let mode: String = writer
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer
            .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
            .unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        drop(writer);
        assert!(path.with_file_name("trae.sqlite-wal").exists());
        assert!(path.with_file_name("trae.sqlite-shm").exists());
    }

    fn read_latest(connection: &Connection) -> crate::Result<String> {
        Ok(connection.query_row(
            "SELECT body FROM messages ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )?)
    }

    fn persistent_directory_snapshot(directory: &Path) -> Vec<(OsString, Vec<u8>)> {
        let mut paths = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                !path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .ends_with("-shm")
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                (
                    path.file_name().unwrap().to_os_string(),
                    fs::read(path).unwrap(),
                )
            })
            .collect()
    }
}
