use std::collections::BTreeSet;

use rusqlite::Connection;

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRow;
use crate::captured_batch::{CapturedSqliteValue, NativePosition, ProviderRecordKind};
use crate::provider::provider_safe_path_segment;
use crate::provider::sqlite::open_provider_sqlite_readonly;
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
    ReadOnlySqliteConnection,
};
use crate::{CaptureError, Result};

use super::position::{
    decode_nanoclaw_position, encode_nanoclaw_position, nanoclaw_locator, nanoclaw_message_locator,
    nanoclaw_next_ordinal, NanoClawKeyset, NanoClawMessageSource, NanoClawPositionPhase,
};
use super::project::{NanoClawProjectDatabaseSnapshot, NanoClawProjectSnapshot};
use super::rows::{
    nanoclaw_fetch_message_candidate, nanoclaw_fetch_session_candidate, nanoclaw_hydrate_message,
    nanoclaw_hydrate_session, nanoclaw_message_after, nanoclaw_message_candidate_key,
    nanoclaw_oversize_limit, nanoclaw_session_candidate_by_rowid, nanoclaw_session_captured_values,
    nanoclaw_session_columns, NanoClawMessageAfter, NanoClawMessageCandidate, NanoClawSessionRow,
};
use super::{nanoclaw_captured_error, NANOCLAW_MESSAGE_RECORD_KIND, NANOCLAW_SESSION_RECORD_KIND};

struct NanoClawDatabaseSource<'snapshot> {
    source: NanoClawMessageSource,
    snapshot: &'snapshot NanoClawProjectDatabaseSnapshot,
    conn: ReadOnlySqliteConnection,
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
        let conn = open_provider_sqlite_readonly(snapshot.path())?;
        if !snapshot.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let table = source.table();
        if !sqlite_table_exists(&conn, table)? {
            return Ok(None);
        }
        let columns = sqlite_table_columns(&conn, table)?;
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
            conn,
            columns,
        }))
    }

    fn revalidate(&self) -> Result<bool> {
        self.snapshot.revalidate()
    }

    fn message_after(&self, keyset: NanoClawKeyset) -> Result<NanoClawMessageAfter> {
        nanoclaw_message_after(&self.conn, &self.columns, self.source, keyset)
    }

    fn fetch_candidate(
        &self,
        after: Option<NanoClawMessageAfter>,
    ) -> Result<Option<NanoClawMessageCandidate>> {
        if !self.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let candidate =
            nanoclaw_fetch_message_candidate(&self.conn, &self.columns, self.source, after)?;
        if !self.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(candidate)
    }

    fn hydrate(&self, rowid: i64) -> Result<Vec<CapturedSqliteValue>> {
        if !self.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let values = nanoclaw_hydrate_message(&self.conn, &self.columns, self.source, rowid)?;
        if !self.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(values)
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
}

pub(super) struct NanoClawRowFetcher<'connection, 'snapshot> {
    central: &'connection Connection,
    snapshot: &'snapshot NanoClawProjectSnapshot,
    session_columns: BTreeSet<String>,
    active_session: Option<NanoClawActiveSession<'snapshot>>,
    session_record_kind: ProviderRecordKind,
    message_record_kind: ProviderRecordKind,
}

impl<'connection, 'snapshot> NanoClawRowFetcher<'connection, 'snapshot> {
    pub(super) fn new(
        central: &'connection Connection,
        snapshot: &'snapshot NanoClawProjectSnapshot,
    ) -> Result<Self> {
        let session_columns = nanoclaw_session_columns(central)?;
        Ok(Self {
            central,
            snapshot,
            session_columns,
            active_session: None,
            session_record_kind: ProviderRecordKind::new(NANOCLAW_SESSION_RECORD_KIND)
                .map_err(nanoclaw_captured_error)?,
            message_record_kind: ProviderRecordKind::new(NANOCLAW_MESSAGE_RECORD_KIND)
                .map_err(nanoclaw_captured_error)?,
        })
    }

    pub(super) fn fetch(&mut self, after: NativePosition) -> Result<Option<SqliteLogicalRow>> {
        let keyset = decode_nanoclaw_position(&after)?;
        match keyset {
            None => self.fetch_session_header(0, 0),
            Some(keyset) if keyset.phase == NanoClawPositionPhase::NextSession => {
                self.fetch_session_header(keyset.session_rowid, keyset.next_ordinal)
            }
            Some(keyset) => {
                self.ensure_active_session(keyset.session_rowid)?;
                if let Some(row) = self.fetch_message(keyset)? {
                    return Ok(Some(row));
                }
                self.active_session = None;
                self.fetch_session_header(keyset.session_rowid, keyset.next_ordinal)
            }
        }
    }

    fn fetch_session_header(
        &mut self,
        after_rowid: i64,
        ordinal: u64,
    ) -> Result<Option<SqliteLogicalRow>> {
        let Some(candidate) = nanoclaw_fetch_session_candidate(
            self.central,
            &self.session_columns,
            (after_rowid != 0).then_some(after_rowid),
        )?
        else {
            return Ok(None);
        };
        let locator = nanoclaw_locator(None, candidate.rowid)?;
        let observed_bytes = candidate.observed_bytes()?;
        if observed_bytes > nanoclaw_oversize_limit()? {
            let next_position = encode_nanoclaw_position(NanoClawKeyset {
                next_ordinal: nanoclaw_next_ordinal(ordinal)?,
                phase: NanoClawPositionPhase::NextSession,
                session_rowid: candidate.rowid,
                message_source: None,
                message_rowid: 0,
            })?;
            self.active_session = None;
            return SqliteLogicalRow::oversize(
                next_position,
                ordinal,
                locator,
                self.session_record_kind.clone(),
                observed_bytes,
            )
            .map(Some)
            .map_err(nanoclaw_captured_error);
        }

        let (row, values) =
            nanoclaw_hydrate_session(self.central, &self.session_columns, candidate.rowid)?;
        let identifiers_are_safe =
            provider_safe_path_segment(&row.agent_group_id) && provider_safe_path_segment(&row.id);
        let next_phase = if identifiers_are_safe {
            NanoClawPositionPhase::Messages
        } else {
            NanoClawPositionPhase::NextSession
        };
        let next_position = encode_nanoclaw_position(NanoClawKeyset {
            next_ordinal: nanoclaw_next_ordinal(ordinal)?,
            phase: next_phase,
            session_rowid: candidate.rowid,
            message_source: None,
            message_rowid: 0,
        })?;
        self.active_session = if identifiers_are_safe {
            Some(self.open_active_session(candidate.rowid, row, observed_bytes)?)
        } else {
            None
        };
        SqliteLogicalRow::values(
            next_position,
            ordinal,
            locator,
            self.session_record_kind.clone(),
            values,
        )
        .map(Some)
        .map_err(nanoclaw_captured_error)
    }

    fn ensure_active_session(&mut self, rowid: i64) -> Result<()> {
        if self
            .active_session
            .as_ref()
            .is_some_and(|session| session.rowid == rowid)
        {
            return Ok(());
        }
        let candidate =
            nanoclaw_session_candidate_by_rowid(self.central, &self.session_columns, rowid)?
                .ok_or(CaptureError::SourceChangedDuringCapture)?;
        let observed_bytes = candidate.observed_bytes()?;
        if observed_bytes > nanoclaw_oversize_limit()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let (row, _) =
            nanoclaw_hydrate_session(self.central, &self.session_columns, candidate.rowid)?;
        if !provider_safe_path_segment(&row.agent_group_id) || !provider_safe_path_segment(&row.id)
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        self.active_session = Some(self.open_active_session(rowid, row, observed_bytes)?);
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
        let inbound =
            NanoClawDatabaseSource::open(NanoClawMessageSource::Inbound, inbound_snapshot)?;
        let outbound =
            NanoClawDatabaseSource::open(NanoClawMessageSource::Outbound, outbound_snapshot)?;
        Ok(NanoClawActiveSession {
            rowid,
            row,
            retained_bytes,
            inbound,
            outbound,
        })
    }

    fn fetch_message(&mut self, keyset: NanoClawKeyset) -> Result<Option<SqliteLogicalRow>> {
        let active = self
            .active_session
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw message phase has no active session",
            ))?;
        if !active.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let after = keyset
            .message_source
            .map(|source| {
                active
                    .component(source)
                    .ok_or(CaptureError::SourceChangedDuringCapture)?
                    .message_after(keyset)
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
        let observed_bytes = candidate.observed_bytes(active.retained_bytes)?;
        let next_position = encode_nanoclaw_position(NanoClawKeyset {
            next_ordinal: nanoclaw_next_ordinal(keyset.next_ordinal)?,
            phase: NanoClawPositionPhase::Messages,
            session_rowid: active.rowid,
            message_source: Some(candidate.source),
            message_rowid: candidate.rowid,
        })?;
        let locator = nanoclaw_message_locator(active.rowid, candidate.source, candidate.rowid)?;
        if observed_bytes > nanoclaw_oversize_limit()? {
            return SqliteLogicalRow::oversize(
                next_position,
                keyset.next_ordinal,
                locator,
                self.message_record_kind.clone(),
                observed_bytes,
            )
            .map(Some)
            .map_err(nanoclaw_captured_error);
        }
        let component = active
            .component(candidate.source)
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw selected message component is unavailable",
            ))?;
        let mut values = component.hydrate(candidate.rowid)?;
        values.extend(nanoclaw_session_captured_values(&active.row));
        if !active.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        SqliteLogicalRow::values(
            next_position,
            keyset.next_ordinal,
            locator,
            self.message_record_kind.clone(),
            values,
        )
        .map(Some)
        .map_err(nanoclaw_captured_error)
    }
}
