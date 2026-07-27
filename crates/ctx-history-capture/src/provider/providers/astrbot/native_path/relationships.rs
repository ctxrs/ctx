use super::*;

pub(super) fn prepare_relationship_projection(conn: &Connection, sql: &AstrBotSql) -> Result<()> {
    if relationship_projection_exists(conn)? {
        return Ok(());
    }
    let original_query_only: i64 = conn.pragma_query_value(None, "query_only", |row| row.get(0))?;
    let operation = (|| {
        conn.pragma_update(None, "query_only", false)?;
        conn.execute_batch(
            "pragma temp_store = file;
             drop table if exists temp.astrbot_nativepath_checkpoint_sessions;
             create temp table astrbot_nativepath_checkpoint_sessions (
                 checkpoint_id text primary key,
                 provider_session_id text not null,
                 parent_created_at integer
             ) without rowid;",
        )?;
        let mut insert = conn.prepare(
            "insert into temp.astrbot_nativepath_checkpoint_sessions
                 (checkpoint_id, provider_session_id, parent_created_at)
             values (?1, ?2, ?3)
             on conflict(checkpoint_id) do update set
                 provider_session_id = excluded.provider_session_id,
                 parent_created_at = excluded.parent_created_at",
        )?;
        let mut after = None;
        loop {
            let Some(candidate) = fetch_candidate(
                conn,
                &sql.conversation_candidate_initial,
                &sql.conversation_candidate_after,
                after,
            )?
            else {
                break;
            };
            after = Some(candidate.physical_rowid);
            if candidate.observed_bytes()?
                > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
            {
                continue;
            }
            let row =
                hydrate_conversation(conn, &sql.conversation_hydration, candidate.physical_rowid)?;
            let session_id = provider_session_id(&row);
            for item in conversation_items(&row.content).0 {
                if let Some(checkpoint) = checkpoint_id(&item) {
                    insert.execute(rusqlite::params![checkpoint, session_id, row.created_at])?;
                }
            }
        }
        Ok(())
    })();
    let restore = conn
        .pragma_update(None, "query_only", original_query_only)
        .map_err(CaptureError::from);
    match (operation, restore) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

pub(super) fn relationship_projection_exists(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "select exists(
             select 1 from temp.sqlite_temp_master
             where type = 'table' and name = 'astrbot_nativepath_checkpoint_sessions'
         )",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists != 0)
    .map_err(CaptureError::from)
}

pub(super) fn linked_platform_message_parent(
    conn: &Connection,
    checkpoint: Option<&str>,
) -> Result<Option<PlatformMessageLink>> {
    let Some(checkpoint) = checkpoint else {
        return Ok(None);
    };
    conn.query_row(
        "select provider_session_id, parent_created_at
         from temp.astrbot_nativepath_checkpoint_sessions
         where checkpoint_id = ?1",
        [checkpoint],
        |row| {
            Ok(PlatformMessageLink {
                provider_session_id: row.get(0)?,
                parent_created_at: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(CaptureError::from)
}
