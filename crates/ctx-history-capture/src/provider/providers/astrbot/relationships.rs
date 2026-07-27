use std::{
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::cell::{Cell, RefCell};

use rusqlite::Connection;
use serde_json::Value;

use crate::captured_batch::{
    NativePosition, CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES, CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::normalization::provider_json_text;
use crate::{CaptureError, Result};

use super::codec::{
    astrbot_checkpoint_id, astrbot_oversize_limit, astrbot_provider_session_id,
    decode_astrbot_position, AstrBotPhase,
};
use super::producer::{astrbot_fetch_candidate, astrbot_hydrate_conversation};
use super::source::AstrBotSql;

pub(super) const ASTRBOT_CONVERSATION_SESSIONS_TEMP_TABLE: &str = "astrbot_conversation_sessions";
pub(super) const ASTRBOT_CHECKPOINT_SESSIONS_TEMP_TABLE: &str = "astrbot_checkpoint_sessions";
pub(super) const ASTRBOT_RELATIONSHIP_PROJECTION_MAX_SOURCE_ROWS_PER_PAGE: usize =
    CAPTURE_BATCH_MAX_RECORDS;
pub(super) const ASTRBOT_RELATIONSHIP_PROJECTION_MAX_TEMP_WRITES_PER_PAGE: usize =
    CAPTURE_BATCH_MAX_RECORDS;
pub(super) const ASTRBOT_RELATIONSHIP_PROJECTION_MAX_RETAINED_BYTES_PER_PAGE: u64 =
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES as u64;
const ASTRBOT_RELATIONSHIP_PROJECTION_MIN_PAGE_INTERVAL: Duration = Duration::from_millis(5);
pub(super) const ASTRBOT_RELATIONSHIP_LOOKUP_SQL: &str =
    "select s.provider_session_id, s.created_at from temp.astrbot_checkpoint_sessions c \
     join temp.astrbot_conversation_sessions s \
       on s.session_key = c.session_key \
     where c.checkpoint_id = ?1";
pub(super) const ASTRBOT_RELATIONSHIP_RETAINED_BYTES_SQL: &str =
    "select octet_length(s.provider_session_id) + coalesce(octet_length(s.created_at), 0) \
     from temp.astrbot_checkpoint_sessions c \
     join temp.astrbot_conversation_sessions s \
       on s.session_key = c.session_key \
     where c.checkpoint_id = ?1";

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct AstrBotRelationshipProjectionTestPacing {
    pub(super) pages: usize,
    pub(super) max_source_rows: usize,
    pub(super) max_retained_bytes: u64,
    pub(super) max_temp_writes: usize,
    pub(super) total_temp_writes: usize,
}

#[cfg(test)]
thread_local! {
    static ASTRBOT_RELATIONSHIP_PROJECTION_TEST_PACING:
        Cell<AstrBotRelationshipProjectionTestPacing> =
            const { Cell::new(AstrBotRelationshipProjectionTestPacing {
                pages: 0,
                max_source_rows: 0,
                max_retained_bytes: 0,
                max_temp_writes: 0,
                total_temp_writes: 0,
            }) };
    static ASTRBOT_RELATIONSHIP_PROJECTION_TEST_WAIT_COUNT: Cell<Option<usize>> =
        const { Cell::new(None) };
    static ASTRBOT_RELATIONSHIP_PROJECTION_TEST_PREPARE_COUNT: Cell<usize> =
        const { Cell::new(0) };
    static ASTRBOT_RELATIONSHIP_PROJECTION_TEST_RELEASE_HOOK:
        RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

struct AstrBotRelationshipProjectionPacer {
    page_started: Instant,
}

impl AstrBotRelationshipProjectionPacer {
    fn new() -> Self {
        Self {
            page_started: Instant::now(),
        }
    }

    fn finish_page(&mut self) {
        let elapsed = self.page_started.elapsed();
        let wait = ASTRBOT_RELATIONSHIP_PROJECTION_MIN_PAGE_INTERVAL.saturating_sub(elapsed);
        #[cfg(test)]
        let intercepted = ASTRBOT_RELATIONSHIP_PROJECTION_TEST_WAIT_COUNT.with(|count| {
            let Some(current) = count.get() else {
                return false;
            };
            count.set(Some(current.saturating_add(1)));
            true
        });
        #[cfg(not(test))]
        let intercepted = false;
        if !intercepted && !wait.is_zero() {
            thread::sleep(wait);
        }
        self.page_started = Instant::now();
    }
}

pub(super) fn astrbot_relationship_projection_needed(
    conn: &Connection,
    sql: &AstrBotSql,
    start_position: &NativePosition,
) -> Result<bool> {
    let keyset = decode_astrbot_position(start_position)?;
    let (Some(initial_sql), Some(after_sql)) = (
        sql.platform_message_candidate_initial.as_deref(),
        sql.platform_message_candidate_after.as_deref(),
    ) else {
        return Ok(false);
    };
    let after_rowid = keyset
        .filter(|keyset| keyset.phase == AstrBotPhase::PlatformMessages)
        .map(|keyset| keyset.physical_rowid);
    // Do not search ahead for a non-NULL, representable checkpoint reference: without a native
    // predicate index that can scan an arbitrarily large suffix before LIMIT 1. One next-row
    // keyset seek is enough to prove platform work remains; any relationship traversal then uses
    // the explicitly paced, file-backed TEMP projection.
    astrbot_fetch_candidate(conn, initial_sql, after_sql, after_rowid).map(|row| row.is_some())
}

pub(super) fn astrbot_relationship_projection_exists(conn: &Connection) -> Result<bool> {
    let tables: i64 = conn.query_row(
        "select count(*) from sqlite_temp_master \
         where type = 'table' and name in (?1, ?2)",
        [
            ASTRBOT_CONVERSATION_SESSIONS_TEMP_TABLE,
            ASTRBOT_CHECKPOINT_SESSIONS_TEMP_TABLE,
        ],
        |row| row.get(0),
    )?;
    Ok(tables == 2)
}

pub(super) fn astrbot_prepare_relationship_projection(
    conn: &Connection,
    sql: &AstrBotSql,
) -> Result<()> {
    if astrbot_relationship_projection_exists(conn)? {
        return Ok(());
    }
    #[cfg(test)]
    ASTRBOT_RELATIONSHIP_PROJECTION_TEST_PREPARE_COUNT
        .with(|count| count.set(count.get().saturating_add(1)));
    let original_query_only: i64 = conn.pragma_query_value(None, "query_only", |row| row.get(0))?;
    let prepare = (|| -> Result<()> {
        // AstrBot stores checkpoint IDs inside JSON, so source indexes cannot
        // serve the reverse lookup. Keep that corpus-sized join in connection-local
        // TEMP, not cursors or a durable source schema; process restarts rebuild it.
        conn.pragma_update(None, "query_only", false)?;
        conn.execute_batch(
            "pragma temp_store = file; \
             drop table if exists temp.astrbot_checkpoint_sessions; \
             drop table if exists temp.astrbot_conversation_sessions; \
             create temp table astrbot_conversation_sessions ( \
                 session_key integer primary key, \
                 provider_session_id text not null unique, \
                 source_rowid integer not null, \
                 created_at integer \
             ) without rowid; \
             create temp table astrbot_checkpoint_sessions ( \
                 checkpoint_id text primary key, \
                 session_key integer not null \
             ) without rowid;",
        )?;
        let mut page_open = false;
        let mut pacer = AstrBotRelationshipProjectionPacer::new();
        let populate = (|| -> Result<()> {
            let mut insert_session = conn.prepare(
                "insert into temp.astrbot_conversation_sessions \
                     (session_key, provider_session_id, source_rowid, created_at) \
                     values (?2, ?1, ?2, ?3) \
                 on conflict(provider_session_id) do update \
                     set source_rowid = excluded.source_rowid, \
                         created_at = excluded.created_at",
            )?;
            let mut select_session_key = conn.prepare(
                "select session_key from temp.astrbot_conversation_sessions \
                 where provider_session_id = ?1",
            )?;
            let mut insert_checkpoint = conn.prepare(
                "insert into temp.astrbot_checkpoint_sessions \
                     (checkpoint_id, session_key) values (?1, ?2) \
                 on conflict(checkpoint_id) do update \
                     set session_key = excluded.session_key",
            )?;
            let mut after_rowid = None;
            let mut pending_candidate = None;
            let mut page_rows = 0_usize;
            let mut page_retained_bytes = 0_u64;
            let mut page_temp_writes = 0_usize;
            loop {
                if !page_open {
                    conn.execute_batch("savepoint astrbot_relationship_projection_page")?;
                    page_open = true;
                    page_rows = 0;
                    page_retained_bytes = 0;
                    page_temp_writes = 0;
                }
                let candidate = match pending_candidate.take() {
                    Some(candidate) => Some(candidate),
                    None => astrbot_fetch_candidate(
                        conn,
                        &sql.conversation_candidate_initial,
                        &sql.conversation_candidate_after,
                        after_rowid,
                    )?,
                };
                let Some(candidate) = candidate else {
                    astrbot_release_relationship_projection_page(
                        conn,
                        page_rows,
                        page_retained_bytes,
                        page_temp_writes,
                        &mut pacer,
                    )?;
                    page_open = false;
                    break;
                };
                let observed_bytes = candidate.observed_bytes()?;
                // Oversize rows are never hydrated; charge them one full page
                // so even their length probe rotates the source snapshot.
                let paced_bytes =
                    observed_bytes.min(ASTRBOT_RELATIONSHIP_PROJECTION_MAX_RETAINED_BYTES_PER_PAGE);
                let next_page_bytes = page_retained_bytes.checked_add(paced_bytes).ok_or(
                    CaptureError::SystemInvariant(
                        "AstrBot relationship projection page byte count overflowed",
                    ),
                )?;
                if page_rows > 0
                    && (page_rows >= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_SOURCE_ROWS_PER_PAGE
                        || next_page_bytes
                            > ASTRBOT_RELATIONSHIP_PROJECTION_MAX_RETAINED_BYTES_PER_PAGE)
                {
                    pending_candidate = Some(candidate);
                    astrbot_release_relationship_projection_page(
                        conn,
                        page_rows,
                        page_retained_bytes,
                        page_temp_writes,
                        &mut pacer,
                    )?;
                    page_open = false;
                    continue;
                }
                after_rowid = Some(candidate.physical_rowid);
                page_rows = page_rows.saturating_add(1);
                page_retained_bytes = next_page_bytes;
                if observed_bytes <= astrbot_oversize_limit()? {
                    let conversation = astrbot_hydrate_conversation(
                        conn,
                        &sql.conversation_hydration,
                        candidate.physical_rowid,
                    )?;
                    if conversation.row_id >= 0 {
                        let provider_session_id = astrbot_provider_session_id(&conversation);
                        insert_session.execute(rusqlite::params![
                            &provider_session_id,
                            candidate.physical_rowid,
                            conversation.created_at,
                        ])?;
                        page_temp_writes = page_temp_writes.checked_add(1).ok_or(
                            CaptureError::SystemInvariant(
                                "AstrBot relationship projection TEMP write count overflowed",
                            ),
                        )?;
                        if page_temp_writes
                            >= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_TEMP_WRITES_PER_PAGE
                        {
                            astrbot_release_relationship_projection_page(
                                conn,
                                page_rows,
                                page_retained_bytes,
                                page_temp_writes,
                                &mut pacer,
                            )?;
                            page_open = false;
                        }
                        let session_key = select_session_key
                            .query_row([&provider_session_id], |row| row.get::<_, i64>(0))?;
                        if let Value::Array(items) = provider_json_text(&conversation.content) {
                            for item in items {
                                let Some(checkpoint_id) = astrbot_checkpoint_id(&item) else {
                                    continue;
                                };
                                if !page_open {
                                    conn.execute_batch(
                                        "savepoint astrbot_relationship_projection_page",
                                    )?;
                                    page_open = true;
                                    page_rows = 0;
                                    page_retained_bytes = 0;
                                    page_temp_writes = 0;
                                }
                                insert_checkpoint
                                    .execute(rusqlite::params![&checkpoint_id, session_key])?;
                                page_temp_writes = page_temp_writes
                                    .checked_add(1)
                                    .ok_or(CaptureError::SystemInvariant(
                                    "AstrBot relationship projection TEMP write count overflowed",
                                ))?;
                                if page_temp_writes
                                    >= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_TEMP_WRITES_PER_PAGE
                                {
                                    astrbot_release_relationship_projection_page(
                                        conn,
                                        page_rows,
                                        page_retained_bytes,
                                        page_temp_writes,
                                        &mut pacer,
                                    )?;
                                    page_open = false;
                                }
                            }
                        }
                    }
                }
                if page_open
                    && (page_rows >= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_SOURCE_ROWS_PER_PAGE
                        || page_retained_bytes
                            >= ASTRBOT_RELATIONSHIP_PROJECTION_MAX_RETAINED_BYTES_PER_PAGE)
                {
                    astrbot_release_relationship_projection_page(
                        conn,
                        page_rows,
                        page_retained_bytes,
                        page_temp_writes,
                        &mut pacer,
                    )?;
                    page_open = false;
                }
            }
            Ok(())
        })();
        match populate {
            Ok(()) => Ok(()),
            Err(error) => {
                if page_open {
                    let _ = conn.execute_batch(
                        "rollback to astrbot_relationship_projection_page; \
                         release astrbot_relationship_projection_page;",
                    );
                }
                Err(error)
            }
        }
    })();
    if prepare.is_err() {
        let _ = conn.execute_batch(
            "drop table if exists temp.astrbot_checkpoint_sessions; \
             drop table if exists temp.astrbot_conversation_sessions;",
        );
    }
    let restore = conn
        .pragma_update(None, "query_only", original_query_only)
        .map_err(CaptureError::from);
    match (prepare, restore) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn astrbot_release_relationship_projection_page(
    conn: &Connection,
    source_rows: usize,
    _retained_bytes: u64,
    temp_writes: usize,
    pacer: &mut AstrBotRelationshipProjectionPacer,
) -> Result<()> {
    conn.execute_batch("release astrbot_relationship_projection_page")?;
    if source_rows > 0 || temp_writes > 0 {
        #[cfg(test)]
        ASTRBOT_RELATIONSHIP_PROJECTION_TEST_PACING.with(|pacing| {
            let current = pacing.get();
            pacing.set(AstrBotRelationshipProjectionTestPacing {
                pages: current.pages.saturating_add(1),
                max_source_rows: current.max_source_rows.max(source_rows),
                max_retained_bytes: current.max_retained_bytes.max(_retained_bytes),
                max_temp_writes: current.max_temp_writes.max(temp_writes),
                total_temp_writes: current.total_temp_writes.saturating_add(temp_writes),
            });
        });
        #[cfg(test)]
        if let Some(hook) =
            ASTRBOT_RELATIONSHIP_PROJECTION_TEST_RELEASE_HOOK.with(|hook| hook.borrow_mut().take())
        {
            hook();
        }
        pacer.finish_page();
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn astrbot_relationship_projection_test_pacing(
) -> AstrBotRelationshipProjectionTestPacing {
    ASTRBOT_RELATIONSHIP_PROJECTION_TEST_PACING.with(Cell::get)
}

#[cfg(test)]
pub(super) fn astrbot_reset_relationship_projection_test_pacing() {
    ASTRBOT_RELATIONSHIP_PROJECTION_TEST_PACING
        .with(|pacing| pacing.set(AstrBotRelationshipProjectionTestPacing::default()));
    ASTRBOT_RELATIONSHIP_PROJECTION_TEST_WAIT_COUNT.with(|count| count.set(Some(0)));
    ASTRBOT_RELATIONSHIP_PROJECTION_TEST_PREPARE_COUNT.with(|count| count.set(0));
    ASTRBOT_RELATIONSHIP_PROJECTION_TEST_RELEASE_HOOK.with(|hook| {
        hook.borrow_mut().take();
    });
}

#[cfg(test)]
pub(super) fn astrbot_relationship_projection_test_wait_count() -> usize {
    ASTRBOT_RELATIONSHIP_PROJECTION_TEST_WAIT_COUNT.with(|count| count.get().unwrap_or_default())
}

#[cfg(test)]
pub(super) fn astrbot_disable_relationship_projection_test_wait_hook() {
    ASTRBOT_RELATIONSHIP_PROJECTION_TEST_WAIT_COUNT.with(|count| count.set(None));
}

#[cfg(test)]
pub(super) fn astrbot_relationship_projection_test_prepare_count() -> usize {
    ASTRBOT_RELATIONSHIP_PROJECTION_TEST_PREPARE_COUNT.with(Cell::get)
}

#[cfg(test)]
pub(super) fn astrbot_set_relationship_projection_test_release_hook(hook: impl FnOnce() + 'static) {
    ASTRBOT_RELATIONSHIP_PROJECTION_TEST_RELEASE_HOOK.with(|current| {
        *current.borrow_mut() = Some(Box::new(hook));
    });
}
