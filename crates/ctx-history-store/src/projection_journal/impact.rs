use std::cell::RefCell;
use std::collections::BTreeSet;

use uuid::Uuid;

use super::protocol_validity::parse_uuid;
use super::records::append_entity;
use super::records::GroupJournalCollector;
use super::JournalEntityKind;
use crate::{Result, Store};

pub(super) const SOURCE_EVENT_IMPACT_SQL: &str = "
    SELECT id FROM events WHERE capture_source_id = ?1
    UNION ALL
    SELECT event.id
    FROM sessions session
    JOIN events event ON event.session_id = session.id
    WHERE session.capture_source_id = ?1
    UNION ALL
    SELECT event.id
    FROM runs run
    JOIN events event ON event.run_id = run.id
    WHERE run.source_id = ?1
";

pub(super) const SESSION_EVENT_IMPACT_SQL: &str = "
    SELECT id FROM events WHERE session_id = ?1
    UNION ALL
    SELECT event.id
    FROM runs run
    JOIN events event ON event.run_id = run.id
    WHERE run.session_id = ?1
";

pub(super) const SESSION_FILE_TOUCH_IMPACT_SQL: &str = "
    SELECT file.id
    FROM events event
    JOIN files_touched file ON file.event_id = event.id
    WHERE event.session_id = ?1
    UNION ALL
    SELECT file.id
    FROM runs run
    JOIN events event ON event.run_id = run.id
    JOIN files_touched file ON file.event_id = event.id
    WHERE run.session_id = ?1
    UNION ALL
    SELECT file.id
    FROM runs run
    JOIN files_touched file ON file.run_id = run.id
    WHERE run.session_id = ?1
    UNION ALL
    SELECT file.id
    FROM sessions session
    JOIN files_touched file ON file.source_id = session.capture_source_id
    WHERE session.id = ?1
";

pub(super) const SESSION_VCS_CHANGE_IMPACT_SQL: &str = "
    SELECT change.id
    FROM sessions session
    JOIN vcs_changes change ON change.source_id = session.capture_source_id
    WHERE session.id = ?1
    UNION ALL
    SELECT change.id
    FROM sessions session
    JOIN history_record_links link
      ON link.history_record_id = session.history_record_id
     AND link.target_type = 'vcs_change'
     AND link.deleted_at_ms IS NULL
    JOIN vcs_changes change ON change.id = link.target_id
    WHERE session.id = ?1
";

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
        SOURCE_EVENT_IMPACT_SQL,
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
        SESSION_EVENT_IMPACT_SQL,
        id,
        collector,
    )?;
    append_query_when_active(
        conn,
        JournalEntityKind::FileTouch,
        SESSION_FILE_TOUCH_IMPACT_SQL,
        id,
        collector,
    )?;
    append_query_when_active(
        conn,
        JournalEntityKind::VcsChange,
        SESSION_VCS_CHANGE_IMPACT_SQL,
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

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;

    fn explain(store: &Store, sql: &str) -> String {
        let explain = format!("EXPLAIN QUERY PLAN {sql}");
        store
            .conn
            .prepare(&explain)
            .unwrap()
            .query_map(params![Uuid::nil().to_string()], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
    }

    #[test]
    fn impact_queries_use_indexed_union_branches() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("ctx.db")).unwrap();

        // `events(session_id)` was a strict left prefix of
        // `events(session_id, occurred_at_ms)` and has been dropped. The
        // session-scoped branches are served by the composite index with the
        // same `SEARCH … (session_id=?)` shape; the structural assertions
        // below are what actually guard against the quadratic.
        let source_plan = explain(&store, SOURCE_EVENT_IMPACT_SQL);
        for index in [
            "idx_events_capture_source_id",
            "idx_sessions_capture_source_id",
            "idx_events_session_occurred_at_ms",
            "idx_runs_source_id",
            "idx_events_run_id",
        ] {
            assert!(source_plan.contains(index), "{source_plan}");
        }
        assert!(!source_plan.contains("SCAN "), "{source_plan}");
        assert!(!source_plan.contains("TEMP B-TREE"), "{source_plan}");

        let session_plan = explain(&store, SESSION_EVENT_IMPACT_SQL);
        for index in [
            "idx_events_session_occurred_at_ms",
            "idx_runs_session_id",
            "idx_events_run_id",
        ] {
            assert!(session_plan.contains(index), "{session_plan}");
        }
        assert!(!session_plan.contains("SCAN "), "{session_plan}");
        assert!(!session_plan.contains("TEMP B-TREE"), "{session_plan}");

        let file_touch_plan = explain(&store, SESSION_FILE_TOUCH_IMPACT_SQL);
        for index in [
            "idx_events_session_occurred_at_ms",
            "idx_files_touched_event_id",
            "idx_runs_session_id",
            "idx_events_run_id",
            "idx_files_touched_run_id",
            "sqlite_autoindex_sessions_1",
            "idx_files_touched_source_id",
        ] {
            assert!(file_touch_plan.contains(index), "{file_touch_plan}");
        }
        assert!(!file_touch_plan.contains("SCAN "), "{file_touch_plan}");
        assert!(
            !file_touch_plan.contains("TEMP B-TREE"),
            "{file_touch_plan}"
        );

        let vcs_change_plan = explain(&store, SESSION_VCS_CHANGE_IMPACT_SQL);
        for index in [
            "sqlite_autoindex_sessions_1",
            "idx_vcs_changes_source_id",
            "sqlite_autoindex_history_record_links_2",
            "sqlite_autoindex_vcs_changes_1",
        ] {
            assert!(vcs_change_plan.contains(index), "{vcs_change_plan}");
        }
        assert!(!vcs_change_plan.contains("SCAN "), "{vcs_change_plan}");
        assert!(
            !vcs_change_plan.contains("TEMP B-TREE"),
            "{vcs_change_plan}"
        );
    }
}
