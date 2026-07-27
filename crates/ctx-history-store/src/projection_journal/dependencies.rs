use std::collections::BTreeSet;

use ctx_history_core::Session;
use rusqlite::OptionalExtension;
use uuid::Uuid;

use super::impact::query_ids;
use super::impact::JournalCollector;
use super::records::append_entity;
use super::{JournalEntityKind, Result, Store};
use crate::connection::parse_optional_uuid;

#[derive(Debug, Default)]
pub(crate) struct SessionJournalDependencies {
    pub(super) file_touch_ids: BTreeSet<Uuid>,
    pub(super) vcs_change_ids: BTreeSet<Uuid>,
}

impl Store {
    pub(crate) fn capture_session_upsert_journal_dependencies(
        &self,
        session: &Session,
    ) -> Result<SessionJournalDependencies> {
        if !self.projection_journal_active_for_mutation()? {
            return Ok(SessionJournalDependencies::default());
        }
        capture_replaced_session_dependencies(
            &self.conn,
            session.id,
            session.capture_source_id,
            session.history_record_id,
        )
    }

    pub(crate) fn capture_session_record_assignment_journal_dependencies(
        &self,
        id: Uuid,
        history_record_id: Uuid,
    ) -> Result<SessionJournalDependencies> {
        if !self.projection_journal_active_for_mutation()? {
            return Ok(SessionJournalDependencies::default());
        }
        let Some((_, previous_history_record_id)) = stored_session_relations(&self.conn, id)?
        else {
            return Ok(SessionJournalDependencies::default());
        };
        if previous_history_record_id == Some(history_record_id) {
            return Ok(SessionJournalDependencies::default());
        }
        capture_session_dependencies(&self.conn, id)
    }

    pub(crate) fn journal_captured_session_dependencies(
        &self,
        dependencies: SessionJournalDependencies,
    ) -> Result<()> {
        journal_session_dependencies(
            &self.conn,
            dependencies,
            Some(&self.projection_journal_group_collector),
        )
    }
}

pub(super) fn capture_replaced_session_dependencies(
    conn: &rusqlite::Connection,
    id: Uuid,
    capture_source_id: Option<Uuid>,
    history_record_id: Option<Uuid>,
) -> Result<SessionJournalDependencies> {
    let Some(previous) = stored_session_relations(conn, id)? else {
        return Ok(SessionJournalDependencies::default());
    };
    if previous == (capture_source_id, history_record_id) {
        return Ok(SessionJournalDependencies::default());
    }
    capture_session_dependencies(conn, id)
}

fn stored_session_relations(
    conn: &rusqlite::Connection,
    id: Uuid,
) -> Result<Option<(Option<Uuid>, Option<Uuid>)>> {
    conn.query_row(
        "SELECT capture_source_id, history_record_id FROM sessions WHERE id = ?1",
        [id.to_string()],
        |row| {
            Ok((
                parse_optional_uuid(row.get(0)?)?,
                parse_optional_uuid(row.get(1)?)?,
            ))
        },
    )
    .optional()
    .map_err(Into::into)
}

pub(super) fn capture_session_dependencies(
    conn: &rusqlite::Connection,
    id: Uuid,
) -> Result<SessionJournalDependencies> {
    Ok(SessionJournalDependencies {
        file_touch_ids: query_ids(
            conn,
            "SELECT DISTINCT f.id FROM files_touched f
             LEFT JOIN events e ON e.id = f.event_id
             LEFT JOIN runs fr ON fr.id = f.run_id
             LEFT JOIN runs er ON er.id = e.run_id
             LEFT JOIN sessions s ON s.id = ?1
             WHERE e.session_id = ?1 OR er.session_id = ?1 OR fr.session_id = ?1
                OR f.source_id = s.capture_source_id
             ORDER BY f.id",
            id,
        )?,
        vcs_change_ids: query_ids(
            conn,
            "SELECT DISTINCT v.id FROM vcs_changes v
             LEFT JOIN sessions s ON s.id = ?1
             LEFT JOIN history_record_links l
               ON l.target_type = 'vcs_change' AND l.target_id = v.id
              AND l.deleted_at_ms IS NULL
             WHERE v.source_id = s.capture_source_id
                OR l.history_record_id = s.history_record_id
             ORDER BY v.id",
            id,
        )?,
    })
}

pub(super) fn journal_session_dependencies(
    conn: &rusqlite::Connection,
    dependencies: SessionJournalDependencies,
    collector: JournalCollector<'_>,
) -> Result<()> {
    for id in dependencies.file_touch_ids {
        append_entity(conn, JournalEntityKind::FileTouch, id, collector)?;
    }
    for id in dependencies.vcs_change_ids {
        append_entity(conn, JournalEntityKind::VcsChange, id, collector)?;
    }
    Ok(())
}
