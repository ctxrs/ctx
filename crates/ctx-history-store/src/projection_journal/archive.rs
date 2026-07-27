use std::collections::BTreeSet;

use ctx_history_core::SessionHistoryArchive;
use uuid::Uuid;

use crate::archive::ImportedArchiveCanonicalIds;

use super::dependencies::{
    capture_replaced_session_dependencies, journal_session_dependencies, SessionJournalDependencies,
};
use super::impact::append_query_when_active;
use super::records::append_entity;
use super::{active_state, JournalEntityKind, Result};

pub(crate) fn capture_archive_journal_dependencies(
    conn: &rusqlite::Connection,
    archive: &SessionHistoryArchive,
) -> Result<SessionJournalDependencies> {
    if active_state(conn)?.is_none() {
        return Ok(SessionJournalDependencies::default());
    }
    let mut dependencies = SessionJournalDependencies::default();
    let mut seen = BTreeSet::new();
    for session in &archive.sessions {
        if !seen.insert(session.id) {
            continue;
        }
        let captured = capture_replaced_session_dependencies(
            conn,
            session.id,
            session.capture_source_id,
            session.history_record_id,
        )?;
        dependencies.file_touch_ids.extend(captured.file_touch_ids);
        dependencies.vcs_change_ids.extend(captured.vcs_change_ids);
    }
    Ok(dependencies)
}

pub(crate) fn journal_archive_dependencies(
    conn: &rusqlite::Connection,
    dependencies: SessionJournalDependencies,
) -> Result<()> {
    journal_session_dependencies(conn, dependencies)
}

pub(crate) fn journal_archive_mutations(
    conn: &rusqlite::Connection,
    archive: &SessionHistoryArchive,
    canonical_ids: &ImportedArchiveCanonicalIds,
    injected_source_id: Option<Uuid>,
) -> Result<()> {
    if active_state(conn)?.is_none() {
        return Ok(());
    }

    let source_ids = archive
        .capture_sources
        .iter()
        .map(|source| source.id)
        .chain(injected_source_id)
        .collect::<BTreeSet<_>>();
    for id in source_ids {
        append_query_when_active(
            conn,
            JournalEntityKind::Event,
            "SELECT DISTINCT e.id FROM events e
             LEFT JOIN sessions s ON s.id = e.session_id
             LEFT JOIN runs r ON r.id = e.run_id
             WHERE e.capture_source_id = ?1 OR s.capture_source_id = ?1 OR r.source_id = ?1
             ORDER BY e.id",
            id,
        )?;
        append_query_when_active(
            conn,
            JournalEntityKind::FileTouch,
            "SELECT id FROM files_touched WHERE source_id = ?1 ORDER BY id",
            id,
        )?;
        append_query_when_active(
            conn,
            JournalEntityKind::VcsChange,
            "SELECT id FROM vcs_changes WHERE source_id = ?1 ORDER BY id",
            id,
        )?;
    }

    for id in archive
        .sessions
        .iter()
        .map(|session| session.id)
        .collect::<BTreeSet<_>>()
    {
        append_query_when_active(
            conn,
            JournalEntityKind::Event,
            "SELECT DISTINCT e.id FROM events e LEFT JOIN runs r ON r.id = e.run_id
             WHERE e.session_id = ?1 OR r.session_id = ?1 ORDER BY e.id",
            id,
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
        )?;
    }

    for id in archive
        .runs
        .iter()
        .map(|run| run.id)
        .collect::<BTreeSet<_>>()
    {
        append_query_when_active(
            conn,
            JournalEntityKind::Event,
            "SELECT id FROM events WHERE run_id = ?1 ORDER BY id",
            id,
        )?;
        append_query_when_active(
            conn,
            JournalEntityKind::FileTouch,
            "SELECT id FROM files_touched WHERE run_id = ?1 ORDER BY id",
            id,
        )?;
    }

    for id in canonical_ids.event_ids.iter().copied() {
        append_entity(conn, JournalEntityKind::Event, id)?;
        append_query_when_active(
            conn,
            JournalEntityKind::FileTouch,
            "SELECT id FROM files_touched WHERE event_id = ?1 ORDER BY id",
            id,
        )?;
    }
    for id in archive
        .files_touched
        .iter()
        .map(|file| file.id)
        .collect::<BTreeSet<_>>()
    {
        append_entity(conn, JournalEntityKind::FileTouch, id)?;
    }
    for id in canonical_ids
        .vcs_change_ids
        .iter()
        .copied()
        .chain(
            archive
                .history_record_links
                .iter()
                .filter(|link| link.target_type.as_str() == "vcs_change")
                .map(|link| link.target_id),
        )
        .collect::<BTreeSet<_>>()
    {
        append_entity(conn, JournalEntityKind::VcsChange, id)?;
    }
    Ok(())
}
