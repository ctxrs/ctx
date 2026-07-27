use std::cell::RefCell;
use std::collections::BTreeSet;

use uuid::Uuid;

use super::protocol_validity::parse_uuid;
use super::records::append_entity;
use super::records::GroupJournalCollector;
use super::JournalEntityKind;
use crate::{Result, Store};

impl Store {
    pub(crate) fn journal_event_mutated(&self, id: Uuid) -> Result<()> {
        if !self.projection_journal_active_for_mutation()? {
            return Ok(());
        }
        event_mutated(
            &self.conn,
            id,
            Some(&self.projection_journal_group_collector),
        )
    }

    pub(crate) fn journal_file_touch_mutated(&self, id: Uuid) -> Result<()> {
        if !self.projection_journal_active_for_mutation()? {
            return Ok(());
        }
        append_entity(
            &self.conn,
            JournalEntityKind::FileTouch,
            id,
            Some(&self.projection_journal_group_collector),
        )
    }

    pub(crate) fn journal_vcs_change_mutated(&self, id: Uuid) -> Result<()> {
        if !self.projection_journal_active_for_mutation()? {
            return Ok(());
        }
        append_entity(
            &self.conn,
            JournalEntityKind::VcsChange,
            id,
            Some(&self.projection_journal_group_collector),
        )
    }

    pub(crate) fn journal_source_mutated(&self, id: Uuid) -> Result<()> {
        if !self.projection_journal_active_for_mutation()? {
            return Ok(());
        }
        source_mutated(
            &self.conn,
            id,
            Some(&self.projection_journal_group_collector),
        )
    }

    pub(crate) fn journal_session_mutated(&self, id: Uuid) -> Result<()> {
        if !self.projection_journal_active_for_mutation()? {
            return Ok(());
        }
        session_mutated(
            &self.conn,
            id,
            Some(&self.projection_journal_group_collector),
        )
    }

    pub(crate) fn journal_run_mutated(&self, id: Uuid) -> Result<()> {
        if !self.projection_journal_active_for_mutation()? {
            return Ok(());
        }
        run_mutated(
            &self.conn,
            id,
            Some(&self.projection_journal_group_collector),
        )
    }

    pub(crate) fn journal_history_link_mutated(
        &self,
        target_type: &str,
        target_id: Uuid,
    ) -> Result<()> {
        if !self.projection_journal_active_for_mutation()? {
            return Ok(());
        }
        history_link_mutated(
            &self.conn,
            target_type,
            target_id,
            Some(&self.projection_journal_group_collector),
        )
    }
}

pub(super) type JournalCollector<'a> = Option<&'a RefCell<Option<GroupJournalCollector>>>;

fn event_mutated(
    conn: &rusqlite::Connection,
    id: Uuid,
    collector: JournalCollector<'_>,
) -> Result<()> {
    append_entity(conn, JournalEntityKind::Event, id, collector)?;
    append_query_when_active(
        conn,
        JournalEntityKind::FileTouch,
        "SELECT id FROM files_touched WHERE event_id = ?1 ORDER BY id",
        id,
        collector,
    )
}

fn source_mutated(
    conn: &rusqlite::Connection,
    id: Uuid,
    collector: JournalCollector<'_>,
) -> Result<()> {
    append_query_when_active(
        conn,
        JournalEntityKind::Event,
        "SELECT DISTINCT e.id FROM events e
         LEFT JOIN sessions s ON s.id = e.session_id
         LEFT JOIN runs r ON r.id = e.run_id
         WHERE e.capture_source_id = ?1 OR s.capture_source_id = ?1 OR r.source_id = ?1
         ORDER BY e.id",
        id,
        collector,
    )?;
    append_query_when_active(
        conn,
        JournalEntityKind::FileTouch,
        "SELECT id FROM files_touched WHERE source_id = ?1 ORDER BY id",
        id,
        collector,
    )?;
    append_query_when_active(
        conn,
        JournalEntityKind::VcsChange,
        "SELECT id FROM vcs_changes WHERE source_id = ?1 ORDER BY id",
        id,
        collector,
    )
}

fn session_mutated(
    conn: &rusqlite::Connection,
    id: Uuid,
    collector: JournalCollector<'_>,
) -> Result<()> {
    append_query_when_active(
        conn,
        JournalEntityKind::Event,
        "SELECT DISTINCT e.id FROM events e LEFT JOIN runs r ON r.id = e.run_id
         WHERE e.session_id = ?1 OR r.session_id = ?1 ORDER BY e.id",
        id,
        collector,
    )?;
    append_query_when_active(
        conn,
        JournalEntityKind::FileTouch,
        "SELECT DISTINCT f.id FROM files_touched f
         LEFT JOIN events e ON e.id = f.event_id
         LEFT JOIN runs fr ON fr.id = f.run_id
         LEFT JOIN runs er ON er.id = e.run_id
         LEFT JOIN sessions s ON s.id = ?1
         WHERE e.session_id = ?1 OR er.session_id = ?1 OR fr.session_id = ?1
            OR f.source_id = s.capture_source_id
         ORDER BY f.id",
        id,
        collector,
    )?;
    append_query_when_active(
        conn,
        JournalEntityKind::VcsChange,
        "SELECT DISTINCT v.id FROM vcs_changes v
         LEFT JOIN sessions s ON s.id = ?1
         LEFT JOIN history_record_links l
           ON l.target_type = 'vcs_change' AND l.target_id = v.id
          AND l.deleted_at_ms IS NULL
         WHERE v.source_id = s.capture_source_id OR l.history_record_id = s.history_record_id
         ORDER BY v.id",
        id,
        collector,
    )
}

fn run_mutated(
    conn: &rusqlite::Connection,
    id: Uuid,
    collector: JournalCollector<'_>,
) -> Result<()> {
    append_query_when_active(
        conn,
        JournalEntityKind::Event,
        "SELECT id FROM events WHERE run_id = ?1 ORDER BY id",
        id,
        collector,
    )?;
    append_query_when_active(
        conn,
        JournalEntityKind::FileTouch,
        "SELECT id FROM files_touched WHERE run_id = ?1 ORDER BY id",
        id,
        collector,
    )
}

fn history_link_mutated(
    conn: &rusqlite::Connection,
    target_type: &str,
    target_id: Uuid,
    collector: JournalCollector<'_>,
) -> Result<()> {
    if target_type == "vcs_change" {
        append_entity(conn, JournalEntityKind::VcsChange, target_id, collector)?;
    }
    Ok(())
}

pub(super) fn append_query_when_active(
    conn: &rusqlite::Connection,
    kind: JournalEntityKind,
    sql: &str,
    id: Uuid,
    collector: JournalCollector<'_>,
) -> Result<()> {
    for entity_id in query_ids(conn, sql, id)? {
        append_entity(conn, kind, entity_id, collector)?;
    }
    Ok(())
}

pub(super) fn query_ids(
    conn: &rusqlite::Connection,
    sql: &str,
    id: Uuid,
) -> Result<BTreeSet<Uuid>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map([id.to_string()], |row| row.get::<_, String>(0))?;
    let mut ids = BTreeSet::new();
    for row in rows {
        ids.insert(parse_uuid(&row?)?);
    }
    Ok(ids)
}
