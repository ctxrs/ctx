use std::{
    ffi::OsString,
    ops::Range,
    path::{Component, Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType};
use rusqlite::{Connection, OptionalExtension};
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
    },
    CaptureError, ProviderImportFailure, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::super::{
    event::{trae_event_from_owned_message, trae_message_is_output},
    json_stream::{
        trae_session_selection, trae_stream_session, TraeJsonArrayValues, TraeJsonContainerValues,
        TraeSessionSelection, TraeStreamSession,
    },
    workspace::{trae_workspace_folder, trae_workspace_id},
    TRAE_CHAT_KEYS,
};

const TRAE_SOURCE_PARSER_REVISION: &str = "trae-nativepath-parser-v2";
const TRAE_SOURCE_POLICY_REVISION: &str = "trae-nativepath-core-policy-v2";
const TRAE_PAGE_UNIT_LIMIT: usize = NATIVE_INGESTION_PAGE_MAX_UNITS - 8;
const TRAE_PAGE_BYTE_LIMIT: usize = NATIVE_INGESTION_PAGE_MAX_BYTES - 512 * 1024;
const TRAE_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 16 * 64;

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
    pub(super) raw_source_path: String,
    pub(super) workspace_id: String,
    pub(super) workspace_folder: Option<String>,
    pub(super) schema_evidence: Vec<u8>,
    observed_at: DateTime<Utc>,
}

pub(super) struct TraeSqliteDatabase {
    parent: ProviderSourceDirectory,
    authority: SqliteSourceDirectoryAuthority,
    database_name: OsString,
    evidence: SqliteSourceEvidence,
}

impl TraeSqliteDatabase {
    pub(super) fn open<T>(
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
        let authority = retain_sqlite_source_directory_authority(&authority_handle, parent_path)
            .map_err(|error| trae_sqlite_source_error(path, error))?;
        let snapshot = open_root_handle_sqlite_source_snapshot(&authority, &database_name)
            .map_err(|error| trae_sqlite_source_error(path, error))?;
        let evidence = snapshot.evidence().clone();
        let result = snapshot
            .connection()
            .map_err(|error| trae_sqlite_source_error(path, error))
            .and_then(query);
        let finished = snapshot
            .finish()
            .map_err(|error| trae_sqlite_source_error(path, error))?;
        if finished != evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let database = Self {
            parent,
            authority,
            database_name,
            evidence,
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
        self.revalidate()?;
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&self.authority, &self.database_name)
                .map_err(|error| trae_sqlite_source_error(path, error))?;
        let result = if snapshot.evidence() == &self.evidence {
            snapshot
                .connection()
                .map_err(|error| trae_sqlite_source_error(path, error))
                .and_then(query)
        } else {
            Err(CaptureError::SourceChangedDuringCapture)
        };
        let finished = snapshot
            .finish()
            .map_err(|error| trae_sqlite_source_error(path, error))?;
        self.revalidate()?;
        if finished != self.evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        result
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
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&self.authority, &self.database_name)
                .map_err(|error| E::from(trae_sqlite_source_error(path, error)))?;
        if snapshot.evidence() != &self.evidence {
            return Err(E::from(CaptureError::SourceChangedDuringCapture));
        }
        let result = snapshot
            .connection()
            .map_err(|error| E::from(trae_sqlite_source_error(path, error)))
            .and_then(query);
        let finished = snapshot
            .finish()
            .map_err(|error| E::from(trae_sqlite_source_error(path, error)))?;
        self.revalidate().map_err(E::from)?;
        if finished != self.evidence {
            return Err(E::from(CaptureError::SourceChangedDuringCapture));
        }
        result
    }

    pub(super) fn revalidate(&self) -> Result<()> {
        self.parent.revalidate()?;
        self.parent.authority_root().revalidate()
    }

    pub(super) fn evidence(&self) -> &SqliteSourceEvidence {
        &self.evidence
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
    path: &Path,
    observed_at: DateTime<Utc>,
) -> Result<TraeSourceAuthority> {
    let (database, (schema, user_version)) = TraeSqliteDatabase::open(path, |conn| {
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
    Ok(TraeSourceAuthority {
        database,
        raw_source_path: path.display().to_string(),
        workspace_id: trae_workspace_id(path),
        workspace_folder: trae_workspace_folder(path),
        schema_evidence,
        observed_at,
    })
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

    pub(super) fn next_page(&mut self, conn: &Connection) -> Result<Option<TraeScanPage>> {
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
                match self.load_key(conn, self.frontier.key_index)? {
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

    fn load_key(&mut self, conn: &Connection, key_index: u16) -> Result<TraeLoadedKey> {
        let Some(chat_key) = TRAE_CHAT_KEYS.get(usize::from(key_index)).copied() else {
            return Ok(TraeLoadedKey::Missing);
        };
        let candidate = {
            let _guard = SqliteLengthPreflightGuard::new(conn);
            conn.query_row(
                "select typeof(value), coalesce(octet_length(value), 0) \
                     from ItemTable where [key] = ?1",
                [chat_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        };
        let Some((value_type, retained_bytes)) = candidate else {
            return Ok(TraeLoadedKey::Missing);
        };
        self.decoded_rows =
            self.decoded_rows
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Trae decoded-row counter overflow",
                ))?;
        let retained_bytes = u64::try_from(retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload("Trae ItemTable value length is negative".into())
        })?;
        let observed_bytes = retained_bytes
            .saturating_add(TRAE_SQLITE_VALUE_OVERHEAD_BYTES)
            .saturating_add(u64::try_from(chat_key.len()).unwrap_or(u64::MAX));
        if observed_bytes > u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES).unwrap_or(u64::MAX) {
            return Ok(TraeLoadedKey::Rejected(format!(
                "Trae ItemTable key `{chat_key}` exceeds the provider JSON bound"
            )));
        }
        if value_type != "text" {
            return Ok(TraeLoadedKey::Rejected(format!(
                "Trae ItemTable key `{chat_key}` has unsupported SQLite type `{value_type}`"
            )));
        }
        let bytes = conn
            .query_row(
                "select cast(value as text) from ItemTable where [key] = ?1",
                [chat_key],
                |row| row.get::<_, String>(0),
            )
            .map(String::into_bytes)?;
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

    use super::TraeSqliteDatabase;

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

        let (database, opened_value) = TraeSqliteDatabase::open(&source, read_latest).unwrap();
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
