impl Store {
    pub async fn list_session_events(&self, session_id: SessionId) -> Result<Vec<SessionEvent>> {
        self.list_session_events_page_by_seq(session_id, None, None, false)
            .await
    }

    pub(super) async fn session_last_event_seq(&self, session_id: SessionId) -> Result<i64> {
        self.flush_event_log_for_reads().await;
        let seq = self
            .query_scalar::<Option<i64>>(
                r#"SELECT MAX(seq) FROM session_events WHERE session_id = ?"#,
            )
            .bind(session_id.0.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(seq.unwrap_or(0))
    }

    pub async fn get_session_last_event_seq(&self, session_id: SessionId) -> Result<i64> {
        self.session_last_event_seq(session_id).await
    }

    pub async fn list_session_events_page_by_seq(
        &self,
        session_id: SessionId,
        after_seq: Option<i64>,
        limit: Option<u32>,
        include_transient: bool,
    ) -> Result<Vec<SessionEvent>> {
        crate::fault_injection::maybe_fail("ctx_store.list_session_events_page_by_seq")?;
        self.flush_event_log_for_reads().await;
        let session_id_str = session_id.0.to_string();
        let limit_i64 = limit.map(|n| n as i64);
        let include_transient = if include_transient { 1 } else { 0 };
        let rows = if let Some(after_seq) = after_seq {
            if let Some(limit) = limit_i64 {
                self.query(
                    r#"SELECT seq, id, session_id, run_id, turn_id, event_type, payload_json, transient, created_at
                       FROM session_events
                       WHERE session_id = ?
                         AND (? = 1 OR transient = 0)
                         AND seq > ?
                       ORDER BY seq ASC
                       LIMIT ?"#,
                )
                .bind(&session_id_str)
                .bind(include_transient)
                .bind(after_seq)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            } else {
                self.query(
                    r#"SELECT seq, id, session_id, run_id, turn_id, event_type, payload_json, transient, created_at
                       FROM session_events
                       WHERE session_id = ?
                         AND (? = 1 OR transient = 0)
                         AND seq > ?
                       ORDER BY seq ASC"#,
                )
                .bind(&session_id_str)
                .bind(include_transient)
                .bind(after_seq)
                .fetch_all(&self.pool)
                .await?
            }
        } else if let Some(limit) = limit_i64 {
            self.query(
                r#"SELECT seq, id, session_id, run_id, turn_id, event_type, payload_json, transient, created_at
                   FROM session_events
                   WHERE session_id = ?
                     AND (? = 1 OR transient = 0)
                   ORDER BY seq ASC
                   LIMIT ?"#,
            )
            .bind(&session_id_str)
            .bind(include_transient)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            self.query(
                r#"SELECT seq, id, session_id, run_id, turn_id, event_type, payload_json, transient, created_at
                   FROM session_events
                   WHERE session_id = ?
                     AND (? = 1 OR transient = 0)
                   ORDER BY seq ASC"#,
            )
            .bind(&session_id_str)
            .bind(include_transient)
            .fetch_all(&self.pool)
            .await?
        };

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let seq: i64 = r.try_get("seq")?;
            let id: String = r.try_get("id")?;
            let session_id: String = r.try_get("session_id")?;
            let run_id: Option<String> = r.try_get("run_id")?;
            let turn_id: Option<String> = r.try_get("turn_id")?;
            let created_at: String = r.try_get("created_at")?;
            let payload_json: String = r.try_get("payload_json")?;
            let transient: i64 = r.try_get("transient")?;
            out.push(SessionEvent {
                seq,
                id: SessionEventId(uuid::Uuid::parse_str(&id)?),
                session_id: SessionId(uuid::Uuid::parse_str(&session_id)?),
                run_id: run_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(RunId),
                turn_id: turn_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(TurnId),
                event_type: parse_session_event_type(
                    r.try_get::<String, _>("event_type")?.as_str(),
                ),
                payload_json: serde_json::from_str(&payload_json)
                    .context("parsing payload_json")?,
                transient: transient != 0,
                created_at: parse_dt(&created_at)?,
            });
        }
        Ok(out)
    }

    pub async fn list_session_events_for_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        include_transient: bool,
    ) -> Result<Vec<SessionEvent>> {
        self.flush_event_log_for_reads().await;
        let include_transient = if include_transient { 1 } else { 0 };
        let rows = self.query(
            r#"SELECT seq, id, session_id, run_id, turn_id, event_type, payload_json, transient, created_at
               FROM session_events
               WHERE session_id = ? AND turn_id = ? AND (? = 1 OR transient = 0)
               ORDER BY seq ASC"#,
        )
        .bind(session_id.0.to_string())
        .bind(turn_id.0.to_string())
        .bind(include_transient)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.try_get("id")?;
            let session_id: String = r.try_get("session_id")?;
            let created_at: String = r.try_get("created_at")?;
            let run_id: Option<String> = r.try_get("run_id")?;
            let turn_id: Option<String> = r.try_get("turn_id")?;
            let payload_json: String = r.try_get("payload_json")?;
            let transient: i64 = r.try_get("transient")?;
            out.push(SessionEvent {
                seq: r.try_get("seq")?,
                id: SessionEventId(uuid::Uuid::parse_str(&id)?),
                session_id: SessionId(uuid::Uuid::parse_str(&session_id)?),
                run_id: run_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(RunId),
                turn_id: turn_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(TurnId),
                event_type: parse_session_event_type(
                    r.try_get::<String, _>("event_type")?.as_str(),
                ),
                payload_json: serde_json::from_str(&payload_json)
                    .context("parsing session event payload")?,
                transient: transient != 0,
                created_at: parse_dt(&created_at)?,
            });
        }
        Ok(out)
    }

    pub async fn get_terminal_event_for_run(
        &self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<Option<SessionEvent>> {
        self.flush_event_log_for_reads().await;
        let row = self.query(
            r#"SELECT seq, id, session_id, run_id, turn_id, event_type, payload_json, transient, created_at
               FROM session_events
               WHERE session_id = ? AND run_id = ? AND event_type IN ('done', 'turn_interrupted', 'turn_finished')
               ORDER BY seq DESC
               LIMIT 1"#,
        )
        .bind(session_id.0.to_string())
        .bind(run_id.0.to_string())
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| {
            let id: String = r.try_get("id").ok()?;
            let session_id: String = r.try_get("session_id").ok()?;
            let run_id: Option<String> = r.try_get("run_id").ok()?;
            let turn_id: Option<String> = r.try_get("turn_id").ok()?;
            let created_at: String = r.try_get("created_at").ok()?;
            let payload_json: String = r.try_get("payload_json").ok()?;
            let transient: i64 = r.try_get("transient").ok()?;
            Some(SessionEvent {
                seq: r.try_get("seq").ok()?,
                id: SessionEventId(uuid::Uuid::parse_str(&id).ok()?),
                session_id: SessionId(uuid::Uuid::parse_str(&session_id).ok()?),
                run_id: run_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(RunId),
                turn_id: turn_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(TurnId),
                event_type: parse_session_event_type(
                    r.try_get::<String, _>("event_type").ok()?.as_str(),
                ),
                payload_json: serde_json::from_str(&payload_json).ok()?,
                transient: transient != 0,
                created_at: parse_dt(&created_at).ok()?,
            })
        }))
    }

    pub async fn delete_session_events_for_turn_types(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        event_types: &[SessionEventType],
    ) -> Result<()> {
        if event_types.is_empty() {
            return Ok(());
        }
        let mut sql = String::from(
            "DELETE FROM session_events WHERE session_id = ? AND turn_id = ? AND event_type IN (",
        );
        for i in 0..event_types.len() {
            if i > 0 {
                sql.push(',');
            }
            sql.push('?');
        }
        sql.push(')');
        let sql = self.rewrite_sql(&sql);
        let mut q = sqlx::query(sql.as_ref())
            .bind(session_id.0.to_string())
            .bind(turn_id.0.to_string());
        for t in event_types {
            q = q.bind(session_event_type_to_str(t));
        }
        q.execute(&self.pool).await?;
        Ok(())
    }

    pub async fn list_session_events_tail_by_seq(
        &self,
        session_id: SessionId,
        limit: u32,
        include_transient: bool,
    ) -> Result<Vec<SessionEvent>> {
        self.flush_event_log_for_reads().await;
        let session_id_str = session_id.0.to_string();
        let include_transient = if include_transient { 1 } else { 0 };
        let rows = self.query(
            r#"SELECT seq, id, session_id, run_id, turn_id, event_type, payload_json, transient, created_at
               FROM session_events
               WHERE session_id = ?
                 AND (? = 1 OR transient = 0)
               ORDER BY seq DESC
               LIMIT ?"#,
        )
        .bind(session_id_str)
        .bind(include_transient)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let seq: i64 = r.try_get("seq")?;
            let id: String = r.try_get("id")?;
            let session_id: String = r.try_get("session_id")?;
            let run_id: Option<String> = r.try_get("run_id")?;
            let turn_id: Option<String> = r.try_get("turn_id")?;
            let created_at: String = r.try_get("created_at")?;
            let payload_json: String = r.try_get("payload_json")?;
            let transient: i64 = r.try_get("transient")?;
            out.push(SessionEvent {
                seq,
                id: SessionEventId(uuid::Uuid::parse_str(&id)?),
                session_id: SessionId(uuid::Uuid::parse_str(&session_id)?),
                run_id: run_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(RunId),
                turn_id: turn_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(TurnId),
                event_type: parse_session_event_type(
                    r.try_get::<String, _>("event_type")?.as_str(),
                ),
                payload_json: serde_json::from_str(&payload_json)
                    .context("parsing payload_json")?,
                transient: transient != 0,
                created_at: parse_dt(&created_at)?,
            });
        }
        out.reverse(); // return ASC
        Ok(out)
    }
}
