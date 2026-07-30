use std::collections::BTreeSet;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::native_source::NativeLocator;
use crate::provider::provider_safe_path_segment;
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
};
use crate::{CaptureError, Result};

use super::position::{
    nanoclaw_message_locator, nanoclaw_next_ordinal, NanoClawFrontier, NanoClawMessageSource,
    NanoClawPositionPhase,
};
use super::project::{
    NanoClawDatabaseRead, NanoClawProjectDatabaseSnapshot, NanoClawProjectSnapshot,
};
use super::rows::{
    nanoclaw_fetch_message_candidate, nanoclaw_fetch_session_candidate,
    nanoclaw_hydrate_native_message, nanoclaw_hydrate_native_session, nanoclaw_message_after,
    nanoclaw_message_candidate_key, nanoclaw_observed_bytes, nanoclaw_retained_length_expr,
    nanoclaw_session_columns, nanoclaw_session_projection, NanoClawMessageAfter,
    NanoClawMessageCandidate, NanoClawMessageRow, NanoClawSessionRow,
    NANOCLAW_NATIVE_MAX_RECORD_BYTES,
};

const NANOCLAW_NATIVE_PAGE_TARGET_BYTES: usize = 6 * 1024 * 1024;
const NANOCLAW_NATIVE_PAGE_MAX_UNITS: usize = 64;
const NANOCLAW_NATIVE_PAGE_RESERVE_BYTES: usize = 4 * 1024;
const NANOCLAW_PREFIX_DOMAIN: &[u8] = b"ctx-nanoclaw-nativepath-prefix-v1\0";

struct NanoClawDatabaseSource<'snapshot> {
    source: NanoClawMessageSource,
    snapshot: &'snapshot NanoClawProjectDatabaseSnapshot,
    read: NanoClawDatabaseRead,
    columns: BTreeSet<String>,
}

impl<'snapshot> NanoClawDatabaseSource<'snapshot> {
    fn open(
        source: NanoClawMessageSource,
        snapshot: &'snapshot NanoClawProjectDatabaseSnapshot,
    ) -> Result<Option<Self>> {
        if !snapshot.is_present() {
            return Ok(None);
        }
        let read = snapshot.open_read()?.ok_or(CaptureError::SystemInvariant(
            "NanoClaw present component did not open a database",
        ))?;
        if !read.revalidate(snapshot)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let conn = read.connection()?;
        let table = source.table();
        if !sqlite_table_exists(conn, table)? {
            return Err(CaptureError::InvalidPayload(format!(
                "NanoClaw {} is missing required {table} table",
                snapshot.path().display()
            )));
        }
        let columns = sqlite_table_columns(conn, table)?;
        ensure_sqlite_table_columns(
            &columns,
            match source {
                NanoClawMessageSource::Inbound => "NanoClaw inbound messages table",
                NanoClawMessageSource::Outbound => "NanoClaw outbound messages table",
            },
            &["id"],
        )?;
        Ok(Some(Self {
            source,
            snapshot,
            read,
            columns,
        }))
    }

    fn revalidate(&self) -> Result<bool> {
        self.read.revalidate(self.snapshot)
    }

    fn connection(&self) -> Result<&Connection> {
        self.read.connection()
    }

    fn finish(self) -> Result<()> {
        self.read.finish(self.snapshot)
    }

    fn message_after(&self, frontier: NanoClawFrontier) -> Result<NanoClawMessageAfter> {
        nanoclaw_message_after(self.connection()?, &self.columns, self.source, frontier)
    }

    fn fetch_candidate(
        &self,
        after: Option<NanoClawMessageAfter>,
    ) -> Result<Option<NanoClawMessageCandidate>> {
        if !self.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let candidate = nanoclaw_fetch_message_candidate(
            self.connection()?,
            &self.columns,
            self.source,
            after,
        )?;
        if !self.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(candidate)
    }

    fn hydrate(&self, rowid: i64) -> Result<NanoClawMessageRow> {
        if !self.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let message =
            nanoclaw_hydrate_native_message(self.connection()?, &self.columns, self.source, rowid)?;
        if !self.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(message)
    }
}

struct NanoClawActiveSession<'snapshot> {
    rowid: i64,
    row: NanoClawSessionRow,
    retained_bytes: u64,
    inbound: Option<NanoClawDatabaseSource<'snapshot>>,
    outbound: Option<NanoClawDatabaseSource<'snapshot>>,
}

impl NanoClawActiveSession<'_> {
    fn component(&self, source: NanoClawMessageSource) -> Option<&NanoClawDatabaseSource<'_>> {
        match source {
            NanoClawMessageSource::Inbound => self.inbound.as_ref(),
            NanoClawMessageSource::Outbound => self.outbound.as_ref(),
        }
    }

    fn revalidate(&self) -> Result<bool> {
        for component in [self.inbound.as_ref(), self.outbound.as_ref()]
            .into_iter()
            .flatten()
        {
            if !component.revalidate()? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn finish(self) -> Result<()> {
        if let Some(inbound) = self.inbound {
            inbound.finish()?;
        }
        if let Some(outbound) = self.outbound {
            outbound.finish()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum NanoClawNativeUnit {
    Session {
        ordinal: u64,
        session_rowid: i64,
        session: NanoClawSessionRow,
    },
    Message {
        ordinal: u64,
        session_rowid: i64,
        source: NanoClawMessageSource,
        message_rowid: i64,
        session: NanoClawSessionRow,
        message: Box<NanoClawMessageRow>,
        #[serde(skip)]
        locator: NativeLocator,
    },
    Rejection {
        ordinal: u64,
        reason: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum NanoClawPreparedUnit {
    Session {
        ordinal: u64,
        session_rowid: i64,
        session: NanoClawSessionRow,
    },
    Message {
        ordinal: u64,
        session_rowid: i64,
        source: NanoClawMessageSource,
        message_rowid: i64,
        session: Box<NanoClawSessionRow>,
        message: NanoClawPreparedMessageRow,
    },
    Rejection {
        ordinal: u64,
        reason: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct NanoClawPreparedMessageRow {
    id: String,
    seq: Option<i64>,
    kind: Option<String>,
    timestamp: Option<i64>,
    status: Option<String>,
    in_reply_to: Option<String>,
    platform_id: Option<String>,
    channel_type: Option<String>,
    thread_id: Option<String>,
    content: Option<String>,
    trigger: Option<String>,
    source_session_id: Option<String>,
    on_wake: Option<i64>,
}

impl NanoClawPreparedUnit {
    pub(super) fn from_native(unit: NanoClawNativeUnit) -> Self {
        match unit {
            NanoClawNativeUnit::Session {
                ordinal,
                session_rowid,
                session,
            } => Self::Session {
                ordinal,
                session_rowid,
                session,
            },
            NanoClawNativeUnit::Message {
                ordinal,
                session_rowid,
                source,
                message_rowid,
                session,
                message,
                ..
            } => Self::Message {
                ordinal,
                session_rowid,
                source,
                message_rowid,
                session: Box::new(session),
                message: NanoClawPreparedMessageRow::from(*message),
            },
            NanoClawNativeUnit::Rejection { ordinal, reason } => {
                Self::Rejection { ordinal, reason }
            }
        }
    }
}

impl From<NanoClawMessageRow> for NanoClawPreparedMessageRow {
    fn from(row: NanoClawMessageRow) -> Self {
        Self {
            id: row.id,
            seq: row.seq,
            kind: row.kind,
            timestamp: row.timestamp,
            status: row.status,
            in_reply_to: row.in_reply_to,
            platform_id: row.platform_id,
            channel_type: row.channel_type,
            thread_id: row.thread_id,
            content: row.content,
            trigger: row.trigger,
            source_session_id: row.source_session_id,
            on_wake: row.on_wake,
        }
    }
}

impl NanoClawPreparedMessageRow {
    pub(super) fn into_native(self, source: NanoClawMessageSource) -> NanoClawMessageRow {
        NanoClawMessageRow {
            source: source.label(),
            id: self.id,
            seq: self.seq,
            kind: self.kind,
            timestamp: self.timestamp,
            status: self.status,
            in_reply_to: self.in_reply_to,
            platform_id: self.platform_id,
            channel_type: self.channel_type,
            thread_id: self.thread_id,
            content: self.content,
            trigger: self.trigger,
            source_session_id: self.source_session_id,
            on_wake: self.on_wake,
        }
    }
}

#[derive(Debug)]
pub(super) struct NanoClawNativePage {
    pub(super) terminal: bool,
    pub(super) units: Vec<NanoClawNativeUnit>,
}

pub(super) struct NanoClawNativeScanner<'connection, 'snapshot> {
    central: &'connection Connection,
    snapshot: &'snapshot NanoClawProjectSnapshot,
    session_columns: BTreeSet<String>,
    active_session: Option<NanoClawActiveSession<'snapshot>>,
    frontier: NanoClawFrontier,
    prefix_hasher: Sha256,
    prefix_bytes: u64,
}

impl<'connection, 'snapshot> NanoClawNativeScanner<'connection, 'snapshot> {
    pub(super) fn new(
        central: &'connection Connection,
        snapshot: &'snapshot NanoClawProjectSnapshot,
    ) -> Result<Self> {
        let mut prefix_hasher = Sha256::new();
        prefix_hasher.update(NANOCLAW_PREFIX_DOMAIN);
        Ok(Self {
            central,
            snapshot,
            session_columns: nanoclaw_session_columns(central)?,
            active_session: None,
            frontier: NanoClawFrontier::initial(),
            prefix_hasher,
            prefix_bytes: 0,
        })
    }

    pub(super) fn prefix_digest_bytes(&self) -> [u8; 32] {
        self.prefix_hasher.clone().finalize().into()
    }

    pub(super) fn prefix_bytes(&self) -> u64 {
        self.prefix_bytes
    }

    pub(super) fn finish(mut self) -> Result<()> {
        self.finish_active_session()?;
        if self.snapshot.revalidate()? {
            Ok(())
        } else {
            Err(CaptureError::SourceChangedDuringCapture)
        }
    }

    pub(super) fn next_page(&mut self) -> Result<NanoClawNativePage> {
        if !self.snapshot.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let mut units = Vec::new();
        let mut bytes = NANOCLAW_NATIVE_PAGE_RESERVE_BYTES;
        let mut terminal = false;
        while units.len() < NANOCLAW_NATIVE_PAGE_MAX_UNITS
            && bytes < NANOCLAW_NATIVE_PAGE_TARGET_BYTES
        {
            let Some(unit) = self.next_unit()? else {
                terminal = true;
                break;
            };
            bytes = bytes.saturating_add(serde_json::to_vec(&unit)?.len());
            units.push(unit);
        }
        if !self.snapshot.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(NanoClawNativePage { terminal, units })
    }

    fn next_unit(&mut self) -> Result<Option<NanoClawNativeUnit>> {
        let frontier = self.frontier;
        let unit = match frontier.phase {
            NanoClawPositionPhase::NextSession => {
                self.fetch_session_header(frontier.session_rowid, frontier.next_ordinal)?
            }
            NanoClawPositionPhase::Messages => {
                self.ensure_active_session(frontier.session_rowid)?;
                match self.fetch_message(frontier)? {
                    Some(unit) => Some(unit),
                    None => {
                        self.finish_active_session()?;
                        self.fetch_session_header(frontier.session_rowid, frontier.next_ordinal)?
                    }
                }
            }
        };
        if let Some((unit, next_frontier)) = unit {
            let encoded = serde_json::to_vec(&unit)?;
            let encoded_bytes = u64::try_from(encoded.len()).map_err(|_| {
                CaptureError::SystemInvariant("NanoClaw native unit length exceeds u64")
            })?;
            self.prefix_hasher.update(encoded_bytes.to_be_bytes());
            self.prefix_hasher.update(encoded);
            self.prefix_bytes = self
                .prefix_bytes
                .checked_add(8)
                .and_then(|value| value.checked_add(encoded_bytes))
                .ok_or(CaptureError::SystemInvariant(
                    "NanoClaw certified prefix byte count overflowed",
                ))?;
            self.frontier = next_frontier;
            Ok(Some(unit))
        } else {
            Ok(None)
        }
    }

    fn fetch_session_header(
        &mut self,
        after_rowid: i64,
        ordinal: u64,
    ) -> Result<Option<(NanoClawNativeUnit, NanoClawFrontier)>> {
        let Some(candidate) = nanoclaw_fetch_session_candidate(
            self.central,
            &self.session_columns,
            (after_rowid != 0).then_some(after_rowid),
        )?
        else {
            return Ok(None);
        };
        let next_ordinal = nanoclaw_next_ordinal(ordinal)?;
        let next_frontier = NanoClawFrontier {
            next_ordinal,
            phase: NanoClawPositionPhase::NextSession,
            session_rowid: candidate.rowid,
            message_source: None,
            message_rowid: 0,
        };
        if let Some(reason) = candidate.rejection_reason() {
            self.finish_active_session()?;
            return Ok(Some((
                NanoClawNativeUnit::Rejection {
                    ordinal,
                    reason: reason.to_owned(),
                },
                next_frontier,
            )));
        }
        let observed_bytes = match candidate.observed_bytes() {
            Ok(bytes) => bytes,
            Err(CaptureError::InvalidPayload(reason)) => {
                self.finish_active_session()?;
                return Ok(Some((
                    NanoClawNativeUnit::Rejection { ordinal, reason },
                    next_frontier,
                )));
            }
            Err(error) => return Err(error),
        };
        if observed_bytes > NANOCLAW_NATIVE_MAX_RECORD_BYTES {
            self.finish_active_session()?;
            return Ok(Some((
                NanoClawNativeUnit::Rejection {
                    ordinal,
                    reason: format!(
                        "NanoClaw session row exceeds the {NANOCLAW_NATIVE_MAX_RECORD_BYTES}-byte NativePath bound"
                    ),
                },
                next_frontier,
            )));
        }
        let session = match nanoclaw_hydrate_native_session(
            self.central,
            &self.session_columns,
            candidate.rowid,
        ) {
            Ok(session) => session,
            Err(error) if nanoclaw_row_decode_error_is_local(&error) => {
                self.finish_active_session()?;
                return Ok(Some((
                    NanoClawNativeUnit::Rejection {
                        ordinal,
                        reason: format!(
                            "NanoClaw session row {} could not be decoded: {error}",
                            candidate.rowid
                        ),
                    },
                    next_frontier,
                )));
            }
            Err(error) => return Err(error),
        };
        if !provider_safe_path_segment(&session.agent_group_id)
            || !provider_safe_path_segment(&session.id)
        {
            self.finish_active_session()?;
            return Ok(Some((
                NanoClawNativeUnit::Rejection {
                    ordinal,
                    reason: "NanoClaw session identifiers are not safe path segments".to_owned(),
                },
                next_frontier,
            )));
        }
        self.active_session =
            Some(self.open_active_session(candidate.rowid, session.clone(), observed_bytes)?);
        Ok(Some((
            NanoClawNativeUnit::Session {
                ordinal,
                session_rowid: candidate.rowid,
                session,
            },
            NanoClawFrontier {
                next_ordinal,
                phase: NanoClawPositionPhase::Messages,
                session_rowid: candidate.rowid,
                message_source: None,
                message_rowid: 0,
            },
        )))
    }

    fn ensure_active_session(&mut self, rowid: i64) -> Result<()> {
        if self
            .active_session
            .as_ref()
            .is_some_and(|session| session.rowid == rowid)
        {
            return Ok(());
        }
        self.finish_active_session()?;
        let retained = nanoclaw_retained_length_expr(&nanoclaw_session_projection(
            self.central,
            &self.session_columns,
        )?);
        let candidate = self
            .central
            .query_row(
                &format!("select rowid, {retained} from sessions s where rowid = ?1"),
                [rowid],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(CaptureError::from)?;
        let observed_bytes = nanoclaw_observed_bytes(candidate.1)?;
        if observed_bytes > NANOCLAW_NATIVE_MAX_RECORD_BYTES {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let session =
            nanoclaw_hydrate_native_session(self.central, &self.session_columns, candidate.0)?;
        if !provider_safe_path_segment(&session.agent_group_id)
            || !provider_safe_path_segment(&session.id)
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        self.active_session = Some(self.open_active_session(rowid, session, observed_bytes)?);
        Ok(())
    }

    fn finish_active_session(&mut self) -> Result<()> {
        if let Some(active) = self.active_session.take() {
            active.finish()?;
        }
        Ok(())
    }

    fn open_active_session(
        &self,
        rowid: i64,
        row: NanoClawSessionRow,
        retained_bytes: u64,
    ) -> Result<NanoClawActiveSession<'snapshot>> {
        let inbound_snapshot = self.snapshot.database(
            rowid,
            &row.agent_group_id,
            &row.id,
            NanoClawMessageSource::Inbound,
        )?;
        let outbound_snapshot = self.snapshot.database(
            rowid,
            &row.agent_group_id,
            &row.id,
            NanoClawMessageSource::Outbound,
        )?;
        Ok(NanoClawActiveSession {
            rowid,
            row,
            retained_bytes,
            inbound: NanoClawDatabaseSource::open(
                NanoClawMessageSource::Inbound,
                inbound_snapshot,
            )?,
            outbound: NanoClawDatabaseSource::open(
                NanoClawMessageSource::Outbound,
                outbound_snapshot,
            )?,
        })
    }

    fn fetch_message(
        &self,
        frontier: NanoClawFrontier,
    ) -> Result<Option<(NanoClawNativeUnit, NanoClawFrontier)>> {
        let active = self
            .active_session
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw NativePath message phase has no active session",
            ))?;
        if !active.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let after = frontier
            .message_source
            .map(|source| {
                active
                    .component(source)
                    .ok_or(CaptureError::SourceChangedDuringCapture)?
                    .message_after(frontier)
            })
            .transpose()?;
        let inbound = active
            .inbound
            .as_ref()
            .map(|component| component.fetch_candidate(after))
            .transpose()?
            .flatten();
        let outbound = active
            .outbound
            .as_ref()
            .map(|component| component.fetch_candidate(after))
            .transpose()?
            .flatten();
        let candidate = match (inbound, outbound) {
            (Some(inbound), Some(outbound)) => {
                if nanoclaw_message_candidate_key(&inbound)
                    <= nanoclaw_message_candidate_key(&outbound)
                {
                    inbound
                } else {
                    outbound
                }
            }
            (Some(candidate), None) | (None, Some(candidate)) => candidate,
            (None, None) => return Ok(None),
        };
        let next_frontier = NanoClawFrontier {
            next_ordinal: nanoclaw_next_ordinal(frontier.next_ordinal)?,
            phase: NanoClawPositionPhase::Messages,
            session_rowid: active.rowid,
            message_source: Some(candidate.source),
            message_rowid: candidate.rowid,
        };
        if let Some(reason) = candidate.rejection_reason() {
            return Ok(Some((
                NanoClawNativeUnit::Rejection {
                    ordinal: frontier.next_ordinal,
                    reason: reason.to_owned(),
                },
                next_frontier,
            )));
        }
        let observed_bytes = match candidate.observed_bytes(active.retained_bytes) {
            Ok(bytes) => bytes,
            Err(CaptureError::InvalidPayload(reason)) => {
                return Ok(Some((
                    NanoClawNativeUnit::Rejection {
                        ordinal: frontier.next_ordinal,
                        reason,
                    },
                    next_frontier,
                )));
            }
            Err(error) => return Err(error),
        };
        if observed_bytes > NANOCLAW_NATIVE_MAX_RECORD_BYTES {
            return Ok(Some((
                NanoClawNativeUnit::Rejection {
                    ordinal: frontier.next_ordinal,
                    reason: format!(
                        "NanoClaw message row exceeds the {NANOCLAW_NATIVE_MAX_RECORD_BYTES}-byte NativePath bound"
                    ),
                },
                next_frontier,
            )));
        }
        let component = active
            .component(candidate.source)
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw selected message component is unavailable",
            ))?;
        let message = match component.hydrate(candidate.rowid) {
            Ok(message) => message,
            Err(error) if nanoclaw_row_decode_error_is_local(&error) => {
                return Ok(Some((
                    NanoClawNativeUnit::Rejection {
                        ordinal: frontier.next_ordinal,
                        reason: format!(
                            "NanoClaw {} message row {} could not be decoded: {error}",
                            candidate.source.label(),
                            candidate.rowid
                        ),
                    },
                    next_frontier,
                )));
            }
            Err(error) => return Err(error),
        };
        if !active.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        if message.seq.is_some_and(|seq| seq < 0) {
            return Ok(Some((
                NanoClawNativeUnit::Rejection {
                    ordinal: frontier.next_ordinal,
                    reason: "NanoClaw message seq must be nonnegative".to_owned(),
                },
                next_frontier,
            )));
        }
        Ok(Some((
            NanoClawNativeUnit::Message {
                ordinal: frontier.next_ordinal,
                session_rowid: active.rowid,
                source: candidate.source,
                message_rowid: candidate.rowid,
                session: active.row.clone(),
                message: Box::new(message),
                locator: nanoclaw_message_locator(active.rowid, candidate.source, candidate.rowid)?,
            },
            next_frontier,
        )))
    }
}

fn nanoclaw_row_decode_error_is_local(error: &CaptureError) -> bool {
    match error {
        CaptureError::InvalidPayload(_) | CaptureError::Json(_) => true,
        CaptureError::Sqlite(error) => matches!(
            error,
            rusqlite::Error::FromSqlConversionFailure(..)
                | rusqlite::Error::IntegralValueOutOfRange(..)
                | rusqlite::Error::Utf8Error(..)
                | rusqlite::Error::InvalidColumnType(..)
        ),
        _ => false,
    }
}
