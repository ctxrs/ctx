impl Store {
    pub async fn get_session_projection_rev(&self, session_id: SessionId) -> Result<i64> {
        self.ensure_session_snapshot_summary(session_id).await?;
        let session_id = session_id.0.to_string();
        let projection_rev = self
            .query_scalar::<Option<i64>>(
                r#"SELECT projection_rev
                   FROM session_snapshot_summaries
                   WHERE session_id = ?"#,
            )
            .bind(&session_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten()
            .unwrap_or(0);
        Ok(projection_rev)
    }

    pub async fn list_messages_for_session(&self, session_id: SessionId) -> Result<Vec<Message>> {
        let rows = self.query(
            r#"SELECT id, session_id, task_id, run_id, turn_id, turn_sequence, order_seq, role, content, attachments_json, delivery, delivered_at, created_at
               FROM messages
               WHERE session_id = ?
               ORDER BY created_at ASC, turn_sequence ASC"#,
        )
        .bind(session_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.try_get("id")?;
            let session_id: String = r.try_get("session_id")?;
            let task_id: String = r.try_get("task_id")?;
            let created_at: String = r.try_get("created_at")?;
            let delivered_at: Option<String> = r.try_get("delivered_at")?;
            let run_id: Option<String> = r.try_get("run_id")?;
            let turn_id: Option<String> = r.try_get("turn_id")?;
            let turn_sequence: Option<i64> = r.try_get("turn_sequence")?;
            let order_seq: Option<i64> = r.try_get("order_seq")?;
            let attachments_json: Option<String> = r.try_get("attachments_json")?;
            let attachments = attachments_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<MessageAttachment>>(s).ok())
                .unwrap_or_default();
            out.push(Message {
                id: MessageId(uuid::Uuid::parse_str(&id)?),
                session_id: SessionId(uuid::Uuid::parse_str(&session_id)?),
                task_id: TaskId(uuid::Uuid::parse_str(&task_id)?),
                run_id: run_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(RunId),
                turn_id: turn_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(TurnId),
                turn_sequence,
                order_seq,
                role: parse_message_role(r.try_get::<String, _>("role")?.as_str()),
                content: r.try_get("content")?,
                attachments,
                delivery: parse_message_delivery(r.try_get::<String, _>("delivery")?.as_str()),
                delivered_at: delivered_at.as_deref().map(parse_dt).transpose()?,
                created_at: parse_dt(&created_at)?,
            });
        }
        Ok(out)
    }

    pub async fn get_last_assistant_message_for_run(
        &self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<Option<Message>> {
        let row = self.query(
            r#"SELECT id, session_id, task_id, run_id, turn_id, turn_sequence, order_seq, role, content, attachments_json, delivery, delivered_at, created_at
               FROM messages
               WHERE session_id = ? AND run_id = ? AND role = 'assistant'
               ORDER BY created_at DESC, turn_sequence DESC
               LIMIT 1"#,
        )
        .bind(session_id.0.to_string())
        .bind(run_id.0.to_string())
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| {
            let id: String = r.try_get("id").ok()?;
            let session_id: String = r.try_get("session_id").ok()?;
            let task_id: String = r.try_get("task_id").ok()?;
            let created_at: String = r.try_get("created_at").ok()?;
            let delivered_at: Option<String> = r.try_get("delivered_at").ok()?;
            let run_id: Option<String> = r.try_get("run_id").ok()?;
            let turn_id: Option<String> = r.try_get("turn_id").ok()?;
            let turn_sequence: Option<i64> = r.try_get("turn_sequence").ok()?;
            let order_seq: Option<i64> = r.try_get("order_seq").ok()?;
            let attachments_json: Option<String> = r.try_get("attachments_json").ok()?;
            let attachments = attachments_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<MessageAttachment>>(s).ok())
                .unwrap_or_default();
            Some(Message {
                id: MessageId(uuid::Uuid::parse_str(&id).ok()?),
                session_id: SessionId(uuid::Uuid::parse_str(&session_id).ok()?),
                task_id: TaskId(uuid::Uuid::parse_str(&task_id).ok()?),
                run_id: run_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(RunId),
                turn_id: turn_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(TurnId),
                turn_sequence,
                order_seq,
                role: parse_message_role(r.try_get::<String, _>("role").ok()?.as_str()),
                content: r.try_get("content").ok()?,
                attachments,
                delivery: parse_message_delivery(r.try_get::<String, _>("delivery").ok()?.as_str()),
                delivered_at: delivered_at.as_deref().map(parse_dt).transpose().ok()?,
                created_at: parse_dt(&created_at).ok()?,
            })
        }))
    }

    pub async fn count_user_messages_for_session(&self, session_id: SessionId) -> Result<i64> {
        let count: i64 = self
            .query_scalar(r#"SELECT COUNT(*) FROM messages WHERE session_id = ? AND role = 'user'"#)
            .bind(session_id.0.to_string())
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    pub async fn get_first_user_message_content(
        &self,
        session_id: SessionId,
    ) -> Result<Option<String>> {
        let row = self
            .query(
                r#"SELECT content
               FROM messages
               WHERE session_id = ? AND role = 'user'
               ORDER BY created_at ASC, id ASC
               LIMIT 1"#,
            )
            .bind(session_id.0.to_string())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.and_then(|r| r.try_get("content").ok()))
    }

    pub(super) async fn list_messages_for_turns(
        &self,
        session_id: SessionId,
        turn_ids: &[TurnId],
    ) -> Result<Vec<Message>> {
        let mut sql = String::from(
            "SELECT id, session_id, task_id, run_id, turn_id, turn_sequence, order_seq, role, content, attachments_json, delivery, delivered_at, created_at
             FROM messages
             WHERE session_id = ?",
        );
        if turn_ids.is_empty() {
            sql.push_str(" AND delivery = 'queued' AND delivered_at IS NULL");
        } else {
            sql.push_str(" AND (turn_id IN (");
            for i in 0..turn_ids.len() {
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push('?');
            }
            sql.push_str(") OR (delivery = 'queued' AND delivered_at IS NULL))");
        }
        sql.push_str(" ORDER BY created_at ASC, turn_sequence ASC");

        let sql = self.rewrite_sql(&sql);
        let mut query = sqlx::query(sql.as_ref()).bind(session_id.0.to_string());
        if !turn_ids.is_empty() {
            for turn_id in turn_ids {
                query = query.bind(turn_id.0.to_string());
            }
        }
        let rows = query.fetch_all(&self.pool).await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.try_get("id")?;
            let session_id: String = r.try_get("session_id")?;
            let task_id: String = r.try_get("task_id")?;
            let created_at: String = r.try_get("created_at")?;
            let delivered_at: Option<String> = r.try_get("delivered_at")?;
            let run_id: Option<String> = r.try_get("run_id")?;
            let turn_id: Option<String> = r.try_get("turn_id")?;
            let turn_sequence: Option<i64> = r.try_get("turn_sequence")?;
            let order_seq: Option<i64> = r.try_get("order_seq")?;
            let attachments_json: Option<String> = r.try_get("attachments_json")?;
            let attachments = attachments_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<MessageAttachment>>(s).ok())
                .unwrap_or_default();
            out.push(Message {
                id: MessageId(uuid::Uuid::parse_str(&id)?),
                session_id: SessionId(uuid::Uuid::parse_str(&session_id)?),
                task_id: TaskId(uuid::Uuid::parse_str(&task_id)?),
                run_id: run_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(RunId),
                turn_id: turn_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(TurnId),
                turn_sequence,
                order_seq,
                role: parse_message_role(r.try_get::<String, _>("role")?.as_str()),
                content: r.try_get("content")?,
                attachments,
                delivery: parse_message_delivery(r.try_get::<String, _>("delivery")?.as_str()),
                delivered_at: delivered_at.as_deref().map(parse_dt).transpose()?,
                created_at: parse_dt(&created_at)?,
            });
        }
        Ok(out)
    }

    pub async fn list_queued_messages_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<Message>> {
        let rows = self.query(
            r#"SELECT id, session_id, task_id, run_id, turn_id, turn_sequence, order_seq, role, content, attachments_json, delivery, delivered_at, created_at
               FROM messages
               WHERE session_id = ? AND delivery = 'queued' AND delivered_at IS NULL
               ORDER BY created_at ASC, turn_sequence ASC"#,
        )
        .bind(session_id.0.to_string())
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.try_get("id")?;
            let session_id: String = r.try_get("session_id")?;
            let task_id: String = r.try_get("task_id")?;
            let created_at: String = r.try_get("created_at")?;
            let run_id: Option<String> = r.try_get("run_id")?;
            let turn_id: Option<String> = r.try_get("turn_id")?;
            let turn_sequence: Option<i64> = r.try_get("turn_sequence")?;
            let order_seq: Option<i64> = r.try_get("order_seq")?;
            let attachments_json: Option<String> = r.try_get("attachments_json")?;
            let attachments = attachments_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<MessageAttachment>>(s).ok())
                .unwrap_or_default();
            out.push(Message {
                id: MessageId(uuid::Uuid::parse_str(&id)?),
                session_id: SessionId(uuid::Uuid::parse_str(&session_id)?),
                task_id: TaskId(uuid::Uuid::parse_str(&task_id)?),
                run_id: run_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(RunId),
                turn_id: turn_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(TurnId),
                turn_sequence,
                order_seq,
                role: parse_message_role(r.try_get::<String, _>("role")?.as_str()),
                content: r.try_get("content")?,
                attachments,
                delivery: MessageDelivery::Queued,
                delivered_at: None,
                created_at: parse_dt(&created_at)?,
            });
        }
        Ok(out)
    }

    pub async fn get_message(&self, id: MessageId) -> Result<Option<Message>> {
        let row = self.query(
            r#"SELECT id, session_id, task_id, run_id, turn_id, turn_sequence, order_seq, role, content, attachments_json, delivery, delivered_at, created_at
               FROM messages WHERE id = ?"#,
        )
        .bind(id.0.to_string())
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| {
            let id: String = r.try_get("id").ok()?;
            let session_id: String = r.try_get("session_id").ok()?;
            let task_id: String = r.try_get("task_id").ok()?;
            let created_at: String = r.try_get("created_at").ok()?;
            let delivered_at: Option<String> = r.try_get("delivered_at").ok()?;
            let run_id: Option<String> = r.try_get("run_id").ok()?;
            let turn_id: Option<String> = r.try_get("turn_id").ok()?;
            let turn_sequence: Option<i64> = r.try_get("turn_sequence").ok()?;
            let order_seq: Option<i64> = r.try_get("order_seq").ok()?;
            let attachments_json: Option<String> = r.try_get("attachments_json").ok()?;
            let attachments = attachments_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<MessageAttachment>>(s).ok())
                .unwrap_or_default();
            Some(Message {
                id: MessageId(uuid::Uuid::parse_str(&id).ok()?),
                session_id: SessionId(uuid::Uuid::parse_str(&session_id).ok()?),
                task_id: TaskId(uuid::Uuid::parse_str(&task_id).ok()?),
                run_id: run_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(RunId),
                turn_id: turn_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(TurnId),
                turn_sequence,
                order_seq,
                role: parse_message_role(r.try_get::<String, _>("role").ok()?.as_str()),
                content: r.try_get("content").ok()?,
                attachments,
                delivery: parse_message_delivery(r.try_get::<String, _>("delivery").ok()?.as_str()),
                delivered_at: delivered_at.as_deref().map(parse_dt).transpose().ok()?,
                created_at: parse_dt(&created_at).ok()?,
            })
        }))
    }

    pub async fn delete_message(&self, id: MessageId) -> Result<()> {
        self.query(r#"DELETE FROM messages WHERE id = ?"#)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_message_delivered(&self, id: MessageId) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let delivery = "immediate";
        let write_bytes = bytes_str(delivery) + bytes_str(&now);
        let result = self
            .query(
                r#"UPDATE messages
               SET delivery = 'immediate', delivered_at = ?
               WHERE id = ?"#,
            )
            .bind(now)
            .bind(id.0.to_string())
            .execute(&self.pool)
            .await?;
        record_write(
            WriteMetricTable::Messages,
            result.rows_affected(),
            write_bytes,
        );
        Ok(())
    }
}
