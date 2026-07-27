use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection};

use crate::events::parse_provider_event_dedupe_key;
use crate::schema::ddl::{table_exists, CREATE_TABLES_SQL};
use crate::schema::indexes::INDEXES_SQL;
use crate::schema::provider_session_identity::PROVIDER_SESSION_INVARIANTS_SQL;
use crate::schema::rebuild::sanitize_v44_result_event_payloads;
use crate::search::projections::{
    event_scriptgram_table_ready, event_search_lookup_table_ready,
    populate_event_search_projection_from_query, rebuild_search_projection,
    refresh_semantic_searchable_item_stats,
};
use crate::{Result, StoreError, FINAL_SCHEMA_IDENTITY};

const FINAL_SCHEMA_SQL: &str = r#"
DROP TRIGGER IF EXISTS sync_cursors_provider_publication_insert_fence;
DROP TRIGGER IF EXISTS sync_cursors_provider_publication_update_fence;
DROP TABLE IF EXISTS cloud_publication_cursor_permits;
DROP TABLE IF EXISTS cloud_publication_state;

CREATE TABLE IF NOT EXISTS ctx_store_schema_identity (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    schema_version INTEGER NOT NULL CHECK (schema_version = 47),
    schema_identity TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS projection_journal_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    active INTEGER NOT NULL DEFAULT 0 CHECK (active IN (0, 1)),
    generation INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0),
    projection_contract_version INTEGER NOT NULL DEFAULT 1
        CHECK (projection_contract_version = 1),
    contract_fingerprint TEXT,
    high_water_sequence INTEGER NOT NULL DEFAULT 0 CHECK (high_water_sequence >= 0),
    cumulative_digest TEXT NOT NULL DEFAULT
        '0000000000000000000000000000000000000000000000000000000000000000',
    acknowledged_sequence INTEGER NOT NULL DEFAULT 0 CHECK (acknowledged_sequence >= 0),
    acknowledged_cumulative_digest TEXT NOT NULL DEFAULT
        '0000000000000000000000000000000000000000000000000000000000000000',
    activated_at_ms INTEGER,
    CHECK ((active = 0) OR (
        generation > 0 AND activated_at_ms IS NOT NULL
        AND length(contract_fingerprint) = 64
        AND length(cumulative_digest) = 64
        AND acknowledged_sequence <= high_water_sequence
        AND length(acknowledged_cumulative_digest) = 64
    ))
);
INSERT OR IGNORE INTO projection_journal_state (singleton) VALUES (1);

CREATE TABLE IF NOT EXISTS projection_journal_chunks (
    generation INTEGER NOT NULL CHECK (generation > 0),
    first_sequence INTEGER NOT NULL CHECK (first_sequence > 0),
    last_sequence INTEGER NOT NULL CHECK (last_sequence >= first_sequence),
    record_count INTEGER NOT NULL CHECK (record_count > 0 AND record_count <= 64),
    uncompressed_bytes INTEGER NOT NULL CHECK (
        uncompressed_bytes > 0 AND uncompressed_bytes <= 8388608
    ),
    records_zstd BLOB NOT NULL CHECK (length(records_zstd) > 0),
    PRIMARY KEY (generation, first_sequence),
    UNIQUE (generation, last_sequence),
    CHECK (last_sequence - first_sequence + 1 = record_count)
);

CREATE TABLE IF NOT EXISTS projection_journal_entities (
    generation INTEGER NOT NULL CHECK (generation > 0),
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('event', 'file_touch', 'vcs_change')),
    stable_entity_id TEXT NOT NULL,
    entity_revision INTEGER NOT NULL CHECK (entity_revision > 0),
    content_digest TEXT NOT NULL CHECK (length(content_digest) = 64),
    PRIMARY KEY (generation, entity_kind, stable_entity_id)
);
"#;

// Removal plan: once ctx intentionally requires on-disk schema v47 or newer
// and provides an explicit path for older stores, delete this module and its
// companion test and remove the v47 dispatch from migrations.rs. Keep the
// provider-session invariant; telemetry adoption alone is insufficient because
// analytics may be disabled or undelivered.
pub(super) fn migrate_to_v47(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let migration = (|| -> Result<()> {
        conn.execute_batch(CREATE_TABLES_SQL)?;
        let affected_event_ids = repair_duplicate_provider_sessions(conn)?;
        if !affected_event_ids.is_empty() {
            refresh_event_search_projection_for_event_ids(conn, &affected_event_ids)?;
        }
        sanitize_v44_result_event_payloads(conn)?;
        conn.execute_batch(INDEXES_SQL)?;
        conn.execute_batch(PROVIDER_SESSION_INVARIANTS_SQL)?;
        conn.execute_batch(FINAL_SCHEMA_SQL)?;
        // v46 may contain a silently partial projection left by an interrupted
        // legacy autocommit rebuild. Rebuild once inside this final migration
        // transaction so v47 never blesses that state as initialized.
        rebuild_search_projection(conn)?;
        conn.execute(
            "INSERT OR REPLACE INTO ctx_store_schema_identity
                 (singleton, schema_version, schema_identity)
             VALUES (1, 47, ?1)",
            [FINAL_SCHEMA_IDENTITY],
        )?;
        conn.execute_batch("PRAGMA user_version = 47;")?;
        Ok(())
    })();

    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(err) => {
            if let Err(rollback_err) = conn.execute_batch("ROLLBACK;") {
                return Err(StoreError::Sql(rollback_err));
            }
            Err(err)
        }
    }
}

#[derive(Clone)]
struct SessionCandidate {
    id: String,
    source_id: String,
    raw_source_path: Option<String>,
    source_format: Option<String>,
    source_identity: Option<String>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

struct EventCandidate {
    id: String,
    seq: i64,
    dedupe_key: Option<String>,
    provider_index: Option<u64>,
    provider_hash: Option<String>,
}

fn repair_duplicate_provider_sessions(conn: &Connection) -> Result<BTreeSet<String>> {
    let mut groups = BTreeMap::<(String, String), Vec<SessionCandidate>>::new();
    {
        let mut stmt = conn.prepare(
            r#"
            SELECT
                s.id,
                s.provider,
                s.external_session_id,
                s.created_at_ms,
                cs.id,
                cs.raw_source_path,
                COALESCE(
                    cs.source_format,
                    json_extract(cs.metadata_json, '$.source_format')
                ),
                cs.source_identity,
                s.updated_at_ms
            FROM sessions s
            JOIN capture_sources cs ON cs.id = s.capture_source_id
            WHERE s.external_session_id IS NOT NULL
              AND s.deleted_at_ms IS NULL
              AND cs.kind = 'provider_import'
              AND (
                  (cs.source_identity IS NOT NULL AND cs.source_identity <> '')
                  OR (cs.raw_source_path IS NOT NULL AND cs.raw_source_path <> '')
              )
            ORDER BY s.created_at_ms, s.id
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                SessionCandidate {
                    id: row.get(0)?,
                    source_id: row.get(4)?,
                    raw_source_path: row.get(5)?,
                    source_format: row.get(6)?,
                    source_identity: row.get(7)?,
                    created_at_ms: row.get(3)?,
                    updated_at_ms: row.get(8)?,
                },
            ))
        })?;
        for row in rows {
            let (provider, external_session_id, candidate) = row?;
            groups
                .entry((provider, external_session_id))
                .or_default()
                .push(candidate);
        }
    }

    let mut affected_event_ids = BTreeSet::new();
    for ((provider, external_session_id), candidates) in groups {
        for component in equivalent_session_components(&candidates) {
            if component.len() < 2 || !source_formats_are_compatible(&component) {
                continue;
            }
            merge_session_group(
                conn,
                &provider,
                &external_session_id,
                &component,
                &mut affected_event_ids,
            )?;
        }
    }
    Ok(affected_event_ids)
}

fn refresh_event_search_projection_for_event_ids(
    conn: &Connection,
    event_ids: &BTreeSet<String>,
) -> Result<()> {
    let has_event_search = table_exists(conn, "event_search")?;
    let has_event_lookup = event_search_lookup_table_ready(conn)?;
    let has_event_scriptgram = event_scriptgram_table_ready(conn)?;
    if !has_event_search && !has_event_lookup && !has_event_scriptgram {
        return Ok(());
    }

    conn.execute_batch(
        r#"
        CREATE TEMP TABLE ctx_v47_affected_event_ids (
            event_id TEXT PRIMARY KEY NOT NULL
        ) WITHOUT ROWID;
        "#,
    )?;
    {
        let mut insert = conn
            .prepare_cached("INSERT INTO temp.ctx_v47_affected_event_ids (event_id) VALUES (?1)")?;
        for event_id in event_ids {
            insert.execute([event_id])?;
        }
    }

    if has_event_search {
        delete_affected_fts_rows(conn, "event_search", event_ids)?;
    }
    if has_event_scriptgram {
        delete_affected_fts_rows(conn, "event_search_scriptgram", event_ids)?;
    }
    if has_event_lookup {
        let mut delete =
            conn.prepare_cached("DELETE FROM event_search_lookup WHERE event_id = ?1")?;
        for event_id in event_ids {
            delete.execute([event_id])?;
        }
    }

    populate_event_search_projection_from_query(
        conn,
        r#"
        SELECT e.id,
               COALESCE(e.history_record_id, r.history_record_id, s.history_record_id, rs.history_record_id),
               e.session_id,
               e.role,
               e.event_type,
               e.payload_json,
               'safe_preview',
               e.visibility,
               e.sync_state,
               e.deleted_at_ms
        FROM temp.ctx_v47_affected_event_ids AS affected
        CROSS JOIN events AS e ON e.id = affected.event_id
        LEFT JOIN runs r ON r.id = e.run_id
        LEFT JOIN sessions s ON s.id = e.session_id
        LEFT JOIN sessions rs ON rs.id = r.session_id
        "#,
        has_event_search,
        has_event_lookup,
        has_event_scriptgram,
    )?;
    conn.execute_batch("DROP TABLE temp.ctx_v47_affected_event_ids;")?;
    refresh_semantic_searchable_item_stats(conn)?;
    Ok(())
}

fn delete_affected_fts_rows(
    conn: &Connection,
    table: &str,
    event_ids: &BTreeSet<String>,
) -> Result<()> {
    let (select_sql, delete_sql) = match table {
        "event_search" => (
            "SELECT rowid, event_id FROM event_search",
            "DELETE FROM event_search WHERE rowid = ?1",
        ),
        "event_search_scriptgram" => (
            "SELECT rowid, event_id FROM event_search_scriptgram",
            "DELETE FROM event_search_scriptgram WHERE rowid = ?1",
        ),
        _ => unreachable!("invalid FTS table {table}"),
    };
    let rowids = {
        let mut stmt = conn.prepare(select_sql)?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut rowids = Vec::new();
        for row in rows {
            let (rowid, event_id) = row?;
            if event_ids.contains(&event_id) {
                rowids.push(rowid);
            }
        }
        rowids
    };
    let mut delete = conn.prepare_cached(delete_sql)?;
    for rowid in rowids {
        delete.execute([rowid])?;
    }
    Ok(())
}

fn equivalent_session_components(candidates: &[SessionCandidate]) -> Vec<Vec<SessionCandidate>> {
    let mut components = Vec::new();
    let mut visited = vec![false; candidates.len()];

    for start in 0..candidates.len() {
        if visited[start] {
            continue;
        }

        visited[start] = true;
        let mut component_indexes = vec![start];
        let mut cursor = 0;
        while cursor < component_indexes.len() {
            let current = component_indexes[cursor];
            for candidate in 0..candidates.len() {
                if !visited[candidate]
                    && sessions_share_source(&candidates[current], &candidates[candidate])
                {
                    visited[candidate] = true;
                    component_indexes.push(candidate);
                }
            }
            cursor += 1;
        }

        components.push(
            component_indexes
                .into_iter()
                .map(|index| candidates[index].clone())
                .collect(),
        );
    }

    components
}

fn sessions_share_source(left: &SessionCandidate, right: &SessionCandidate) -> bool {
    same_nonempty_value(
        left.source_identity.as_deref(),
        right.source_identity.as_deref(),
    ) || same_nonempty_value(
        left.raw_source_path.as_deref(),
        right.raw_source_path.as_deref(),
    )
}

fn same_nonempty_value(left: Option<&str>, right: Option<&str>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if !left.is_empty() && left == right)
}

fn source_formats_are_compatible(candidates: &[SessionCandidate]) -> bool {
    candidates
        .iter()
        .filter_map(|candidate| candidate.source_format.as_deref())
        .collect::<BTreeSet<_>>()
        .len()
        <= 1
}

fn merge_session_group(
    conn: &Connection,
    provider: &str,
    external_session_id: &str,
    candidates: &[SessionCandidate],
    affected_event_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let canonical = candidates
        .iter()
        .min_by_key(|candidate| (candidate.created_at_ms, candidate.id.as_str()))
        .expect("duplicate session group is nonempty");
    let preferred_source = candidates
        .iter()
        .max_by_key(|candidate| {
            (
                candidate.source_identity.is_some(),
                candidate.source_format.is_some(),
                candidate.updated_at_ms,
                candidate.created_at_ms,
            )
        })
        .expect("duplicate session group is nonempty");

    merge_canonical_session_state(conn, &canonical.id, &preferred_source.id)?;

    merge_group_events(
        conn,
        provider,
        external_session_id,
        &canonical.id,
        candidates,
        affected_event_ids,
    )?;

    for duplicate in candidates
        .iter()
        .filter(|candidate| candidate.id != canonical.id)
    {
        redirect_link_target(conn, "session", &duplicate.id, &canonical.id)?;
        conn.execute(
            "UPDATE sessions SET parent_session_id = ?1 WHERE parent_session_id = ?2",
            params![canonical.id, duplicate.id],
        )?;
        conn.execute(
            "UPDATE sessions SET root_session_id = ?1 WHERE root_session_id = ?2",
            params![canonical.id, duplicate.id],
        )?;
        conn.execute(
            "UPDATE session_edges SET from_session_id = ?1 WHERE from_session_id = ?2",
            params![canonical.id, duplicate.id],
        )?;
        conn.execute(
            "UPDATE session_edges SET to_session_id = ?1 WHERE to_session_id = ?2",
            params![canonical.id, duplicate.id],
        )?;
        for table in ["runs", "summaries", "event_search_lookup"] {
            conn.execute(
                &format!("UPDATE {table} SET session_id = ?1 WHERE session_id = ?2"),
                params![canonical.id, duplicate.id],
            )?;
        }
        conn.execute(
            r#"
            INSERT OR REPLACE INTO session_aliases
            (alias_id, session_id, reason, created_at_ms)
            VALUES (?1, ?2, 'provider_source_identity_repair', unixepoch('subsec') * 1000)
            "#,
            params![duplicate.id, canonical.id],
        )?;
        conn.execute(
            "DELETE FROM sessions WHERE id = ?1",
            [duplicate.id.as_str()],
        )?;
    }

    conn.execute(
        "UPDATE sessions SET capture_source_id = ?1 WHERE id = ?2",
        params![preferred_source.source_id, canonical.id],
    )?;
    Ok(())
}

fn merge_canonical_session_state(
    conn: &Connection,
    canonical_id: &str,
    preferred_id: &str,
) -> Result<()> {
    conn.execute(
        r#"
        UPDATE sessions
        SET (
            history_record_id,
            parent_session_id,
            root_session_id,
            external_agent_id,
            agent_type,
            role_hint,
            is_primary,
            status,
            fidelity,
            transcript_blob_id,
            started_at_ms,
            ended_at_ms,
            updated_at_ms,
            visibility,
            sync_state,
            sync_version,
            metadata_json
        ) = (
            SELECT
                COALESCE(preferred.history_record_id, sessions.history_record_id),
                COALESCE(preferred.parent_session_id, sessions.parent_session_id),
                COALESCE(preferred.root_session_id, sessions.root_session_id),
                COALESCE(preferred.external_agent_id, sessions.external_agent_id),
                preferred.agent_type,
                COALESCE(preferred.role_hint, sessions.role_hint),
                preferred.is_primary,
                preferred.status,
                preferred.fidelity,
                COALESCE(preferred.transcript_blob_id, sessions.transcript_blob_id),
                MIN(sessions.started_at_ms, preferred.started_at_ms),
                COALESCE(
                    MAX(sessions.ended_at_ms, preferred.ended_at_ms),
                    sessions.ended_at_ms,
                    preferred.ended_at_ms
                ),
                MAX(sessions.updated_at_ms, preferred.updated_at_ms),
                preferred.visibility,
                preferred.sync_state,
                MAX(sessions.sync_version, preferred.sync_version),
                preferred.metadata_json
            FROM sessions preferred
            WHERE preferred.id = ?2
        )
        WHERE id = ?1
        "#,
        params![canonical_id, preferred_id],
    )?;
    Ok(())
}

fn merge_group_events(
    conn: &Connection,
    _provider: &str,
    _external_session_id: &str,
    canonical_session_id: &str,
    sessions: &[SessionCandidate],
    affected_event_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let mut events = Vec::new();
    for session in sessions {
        let mut stmt = conn.prepare(
            r#"
            SELECT
                id,
                seq,
                dedupe_key,
                json_extract(metadata_json, '$.provider_event_index'),
                json_extract(metadata_json, '$.provider_event_hash')
            FROM events
            WHERE session_id = ?1
            ORDER BY seq, id
            "#,
        )?;
        let rows = stmt.query_map([session.id.as_str()], |row| {
            let provider_index = row
                .get::<_, Option<i64>>(3)?
                .and_then(|value| u64::try_from(value).ok());
            Ok(EventCandidate {
                id: row.get(0)?,
                seq: row.get(1)?,
                dedupe_key: row.get(2)?,
                provider_index,
                provider_hash: row.get(4)?,
            })
        })?;
        for row in rows {
            events.push(row?);
        }
    }
    affected_event_ids.extend(events.iter().map(|event| event.id.clone()));
    events.sort_by(|left, right| (left.seq, &left.id).cmp(&(right.seq, &right.id)));

    let mut canonical_events = BTreeMap::<(u64, String), String>::new();
    for event in events {
        let identity = event_identity(&event);
        if let Some(existing_id) = identity
            .as_ref()
            .and_then(|identity| canonical_events.get(identity))
        {
            merge_event(conn, &event.id, existing_id)?;
            continue;
        }
        if let Some(identity) = identity {
            canonical_events.insert(identity, event.id.clone());
        }
        conn.execute(
            "UPDATE events SET session_id = ?1 WHERE id = ?2",
            params![canonical_session_id, event.id],
        )?;
        conn.execute(
            "UPDATE event_search_lookup SET session_id = ?1 WHERE event_id = ?2",
            params![canonical_session_id, event.id],
        )?;
    }
    Ok(())
}

fn event_identity(event: &EventCandidate) -> Option<(u64, String)> {
    match (event.provider_index, event.provider_hash.as_ref()) {
        (Some(index), Some(hash)) if !hash.is_empty() => return Some((index, hash.clone())),
        _ => {}
    }
    event
        .dedupe_key
        .as_deref()
        .and_then(parse_provider_event_dedupe_key)
        .map(|parsed| (parsed.provider_index, parsed.payload_hash))
}

fn merge_event(conn: &Connection, duplicate_id: &str, canonical_id: &str) -> Result<()> {
    redirect_link_target(conn, "event", duplicate_id, canonical_id)?;
    conn.execute(
        "UPDATE files_touched SET event_id = ?1 WHERE event_id = ?2",
        params![canonical_id, duplicate_id],
    )?;
    conn.execute(
        r#"
        INSERT OR REPLACE INTO event_aliases
        (alias_id, event_id, reason, created_at_ms)
        VALUES (?1, ?2, 'provider_source_identity_repair', unixepoch('subsec') * 1000)
        "#,
        params![duplicate_id, canonical_id],
    )?;
    conn.execute(
        "DELETE FROM event_search_lookup WHERE event_id = ?1",
        [duplicate_id],
    )?;
    conn.execute("DELETE FROM events WHERE id = ?1", [duplicate_id])?;
    Ok(())
}

fn redirect_link_target(
    conn: &Connection,
    target_type: &str,
    duplicate_id: &str,
    canonical_id: &str,
) -> Result<()> {
    conn.execute(
        r#"
        DELETE FROM history_record_links
        WHERE target_type = ?1
          AND target_id = ?2
          AND EXISTS (
              SELECT 1
              FROM history_record_links existing
              WHERE existing.history_record_id = history_record_links.history_record_id
                AND existing.target_type = history_record_links.target_type
                AND existing.target_id = ?3
                AND existing.link_type = history_record_links.link_type
          )
        "#,
        params![target_type, duplicate_id, canonical_id],
    )?;
    conn.execute(
        "UPDATE history_record_links SET target_id = ?1 WHERE target_type = ?2 AND target_id = ?3",
        params![canonical_id, target_type, duplicate_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_identity_falls_back_to_legacy_and_source_scoped_dedupe_keys() {
        for key in [
            "provider:claude:session:7:event-hash",
            "provider-source:018fe2e4-2266-7000-8000-000000000001:7:event-hash",
        ] {
            assert_eq!(
                event_identity(&EventCandidate {
                    id: "event".to_owned(),
                    seq: 1,
                    dedupe_key: Some(key.to_owned()),
                    provider_index: None,
                    provider_hash: None,
                }),
                Some((7, "event-hash".to_owned()))
            );
        }
    }

    #[test]
    fn source_format_compatibility_allows_unknown_transition_but_rejects_conflict() {
        let candidate = |source_format: Option<&str>| SessionCandidate {
            id: "session".to_owned(),
            source_id: "source".to_owned(),
            raw_source_path: Some("/tmp/session.jsonl".to_owned()),
            source_format: source_format.map(str::to_owned),
            source_identity: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        };
        assert!(source_formats_are_compatible(&[
            candidate(None),
            candidate(Some("claude_projects_jsonl_tree")),
        ]));
        assert!(!source_formats_are_compatible(&[
            candidate(Some("claude_projects_jsonl_tree")),
            candidate(Some("claude_projects_jsonl_flat")),
        ]));
    }

    #[test]
    fn source_equivalence_uses_identity_or_exact_path_without_overmerging() {
        let candidate = |path: &str, identity: Option<&str>| SessionCandidate {
            id: format!("{path}:{identity:?}"),
            source_id: "source".to_owned(),
            raw_source_path: Some(path.to_owned()),
            source_format: Some("codex_session_jsonl_tree".to_owned()),
            source_identity: identity.map(str::to_owned),
            created_at_ms: 0,
            updated_at_ms: 0,
        };
        let components = equivalent_session_components(&[
            candidate("/tmp/original.jsonl", None),
            candidate("/tmp/original.jsonl", Some("source-a")),
            candidate("/tmp/moved.jsonl", Some("source-a")),
            candidate("/tmp/copied.jsonl", Some("source-b")),
        ]);

        assert_eq!(components.len(), 2);
        assert_eq!(components[0].len(), 3);
        assert_eq!(components[1].len(), 1);
    }
}
