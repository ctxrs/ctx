use super::*;

#[derive(Clone)]
pub struct Store {
    pub(super) pool: Pool<Sqlite>,
    pub(super) sqlite_path: Option<std::path::PathBuf>,
    pub(super) event_log: Arc<EventLogRuntime>,
    pub(super) active_head_projection: Arc<ActiveHeadProjectionRuntime>,
    pub(super) write_gate: Arc<Mutex<()>>,
    pub(super) _lease_guard: Option<Arc<dyn StoreLeaseGuard>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StoreStats {
    pub pool_size: usize,
    pub pool_idle: usize,
}

pub struct SessionRetentionPruneStats {
    pub tool_summaries_deleted: u64,
    pub turn_thoughts_cleared: u64,
}

pub fn is_unique_constraint_violation(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        let Some(sqlx::Error::Database(db_err)) = cause.downcast_ref::<sqlx::Error>() else {
            continue;
        };
        if let Some(code) = db_err.code() {
            if matches!(code.as_ref(), "1555" | "2067") {
                return true;
            }
        }
        let message = db_err.message();
        if message.contains("UNIQUE constraint failed") || message.contains("PRIMARY KEY") {
            return true;
        }
    }
    false
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_sqlite(path, None).await
    }

    pub fn pool(&self) -> &Pool<Sqlite> {
        &self.pool
    }

    pub fn stats(&self) -> StoreStats {
        StoreStats {
            pool_size: self.pool.size() as usize,
            pool_idle: self.pool.num_idle(),
        }
    }

    pub async fn checkpoint_wal_truncate(&self) -> Result<()> {
        if self.sqlite_path.is_none() {
            return Ok(());
        }
        let _write_guard = self.write_gate.lock().await;
        let (busy, log_frames, checkpointed_frames): (i64, i64, i64) =
            sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
                .fetch_one(&self.pool)
                .await?;
        if busy != 0 {
            anyhow::bail!(
                "sqlite WAL truncate checkpoint failed (busy={}, log_frames={}, checkpointed_frames={})",
                busy,
                log_frames,
                checkpointed_frames
            );
        }
        Ok(())
    }

    pub async fn close(&self) {
        if self._lease_guard.is_some() {
            tracing::debug!("ignoring close() on lease-backed store handle");
            return;
        }
        if let Err(err) = self.event_log.shutdown().await {
            tracing::warn!("event log shutdown failed during close: {err:#}");
        }
        if let Err(err) = self.active_head_projection.shutdown().await {
            tracing::warn!("active head projection shutdown failed during close: {err:#}");
        }
        self.pool.close().await;
    }

    pub fn close_blocking(&self) {
        if self._lease_guard.is_some() {
            tracing::debug!("ignoring close_blocking() on lease-backed store handle");
            return;
        }
        if let Err(err) = self.event_log.shutdown_blocking() {
            tracing::warn!("event log shutdown failed during blocking close: {err:#}");
        }
        if let Err(err) = self.active_head_projection.shutdown_blocking() {
            tracing::warn!("active head projection shutdown failed during blocking close: {err:#}");
        }
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(async {
                self.pool.close().await;
            }),
            Err(err) => {
                tracing::warn!("failed to build runtime for blocking pool close: {err:#}");
                futures::executor::block_on(async {
                    self.pool.close().await;
                });
            }
        }
    }

    pub(super) fn sql(&self, sql: &'static str) -> &'static str {
        sql
    }

    pub(super) fn query<'q>(
        &'q self,
        sql: &'static str,
    ) -> sqlx::query::Query<'q, Sqlite, SqliteArguments<'q>> {
        let sql: &'q str = self.sql(sql);
        sqlx::query(sql)
    }

    pub(super) fn query_scalar<'q, T>(
        &'q self,
        sql: &'static str,
    ) -> sqlx::query::QueryScalar<'q, Sqlite, T, SqliteArguments<'q>>
    where
        for<'r> T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite> + Send,
    {
        let sql: &'q str = self.sql(sql);
        sqlx::query_scalar(sql)
    }

    pub(super) fn rewrite_sql<'a>(&self, sql: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(sql)
    }

    pub async fn prune_session_data_older_than_days(
        &self,
        retention_days: u64,
    ) -> Result<SessionRetentionPruneStats> {
        if retention_days == 0 {
            return Ok(SessionRetentionPruneStats {
                tool_summaries_deleted: 0,
                turn_thoughts_cleared: 0,
            });
        }
        let cutoff = Utc::now() - chrono::Duration::days(retention_days as i64);
        let cutoff_str = cutoff.to_rfc3339();

        let tool_summaries_deleted = if disable_tool_summary_persistence() {
            0
        } else {
            self.query(
                r#"DELETE FROM session_turn_tools
               WHERE session_id IN (
                   SELECT s.id
                   FROM sessions s
                   JOIN tasks t ON t.id = s.task_id
                   WHERE t.archived_at IS NOT NULL
                     AND t.archived_at < ?
               )"#,
            )
            .bind(&cutoff_str)
            .execute(&self.pool)
            .await?
            .rows_affected()
        };

        // Keep the row (turn metadata is still useful), but remove old final thoughts.
        let turn_thoughts_cleared = self
            .query(
                r#"UPDATE session_turns
               SET thought_partial = NULL
               WHERE thought_partial IS NOT NULL
                 AND session_id IN (
                     SELECT s.id
                     FROM sessions s
                     JOIN tasks t ON t.id = s.task_id
                     WHERE t.archived_at IS NOT NULL
                       AND t.archived_at < ?
                 )"#,
            )
            .bind(&cutoff_str)
            .execute(&self.pool)
            .await?
            .rows_affected();
        record_write(WriteMetricTable::SessionTurns, turn_thoughts_cleared, 0);

        Ok(SessionRetentionPruneStats {
            tool_summaries_deleted,
            turn_thoughts_cleared,
        })
    }
}
