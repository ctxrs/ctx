impl Store {
    pub async fn upsert_subagent_invocation(
        &self,
        invocation: SubagentInvocation,
    ) -> Result<SubagentInvocation> {
        self.query(
            r#"INSERT INTO subagent_invocations (
                   id, tool_call_id, parent_session_id, parent_turn_id,
                   requested_count, request_json, status, created_at, updated_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(id) DO UPDATE SET
                   tool_call_id = excluded.tool_call_id,
                   parent_session_id = excluded.parent_session_id,
                   parent_turn_id = COALESCE(excluded.parent_turn_id, subagent_invocations.parent_turn_id),
                   requested_count = excluded.requested_count,
                   request_json = COALESCE(excluded.request_json, subagent_invocations.request_json),
                   status = excluded.status,
                   updated_at = excluded.updated_at"#,
        )
        .bind(&invocation.id)
        .bind(&invocation.tool_call_id)
        .bind(invocation.parent_session_id.0.to_string())
        .bind(invocation.parent_turn_id.map(|t| t.0.to_string()))
        .bind(invocation.requested_count)
        .bind(invocation.request_json.as_ref().map(serde_json::to_string).transpose()?)
        .bind(&invocation.status)
        .bind(invocation.created_at.to_rfc3339())
        .bind(invocation.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(invocation)
    }

    pub async fn update_subagent_invocation_status(
        &self,
        id: &str,
        status: &str,
        updated_at: DateTime<Utc>,
    ) -> Result<()> {
        self.query(
            r#"UPDATE subagent_invocations
               SET status = ?, updated_at = ?
               WHERE id = ?"#,
        )
        .bind(status)
        .bind(updated_at.to_rfc3339())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_subagent_invocation_child(
        &self,
        child: SubagentInvocationChild,
    ) -> Result<SubagentInvocationChild> {
        self.query(
            r#"INSERT INTO subagent_invocation_children (
                   invocation_id, child_session_id, run_id, position, status,
                   label, harness, model, reasoning_effort, prompt_length,
                   created_at, updated_at
               )
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(invocation_id, child_session_id) DO UPDATE SET
                   run_id = COALESCE(excluded.run_id, subagent_invocation_children.run_id),
                   position = excluded.position,
                   status = excluded.status,
                   label = COALESCE(excluded.label, subagent_invocation_children.label),
                   harness = COALESCE(excluded.harness, subagent_invocation_children.harness),
                   model = COALESCE(excluded.model, subagent_invocation_children.model),
                   reasoning_effort = COALESCE(excluded.reasoning_effort, subagent_invocation_children.reasoning_effort),
                   prompt_length = excluded.prompt_length,
                   updated_at = excluded.updated_at"#,
        )
        .bind(&child.invocation_id)
        .bind(child.child_session_id.0.to_string())
        .bind(child.run_id.as_ref().map(|run_id| run_id.0.to_string()))
        .bind(child.position)
        .bind(&child.status)
        .bind(child.label.as_deref())
        .bind(child.harness.as_deref())
        .bind(child.model.as_deref())
        .bind(child.reasoning_effort.as_deref())
        .bind(child.prompt_length)
        .bind(child.created_at.to_rfc3339())
        .bind(child.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(child)
    }

    pub async fn get_subagent_invocation(&self, id: &str) -> Result<Option<SubagentInvocation>> {
        let row = self
            .query(
                r#"SELECT id, tool_call_id, parent_session_id, parent_turn_id,
                      requested_count, request_json, status, created_at, updated_at
               FROM subagent_invocations
               WHERE id = ?"#,
            )
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let mut invocation = build_subagent_invocation_from_row(row)?;
        let rows = self
            .query(
                r#"SELECT invocation_id, child_session_id, run_id, position, status,
                      label, harness, model, reasoning_effort, prompt_length,
                      created_at, updated_at
               FROM subagent_invocation_children
               WHERE invocation_id = ?
               ORDER BY position ASC"#,
            )
            .bind(&invocation.id)
            .fetch_all(&self.pool)
            .await?;

        invocation.children = rows
            .into_iter()
            .filter_map(|r| build_subagent_invocation_child_from_row(r).ok())
            .collect();
        Ok(Some(invocation))
    }

    pub async fn list_subagent_invocations_for_session(
        &self,
        parent_session_id: SessionId,
        parent_turn_id: Option<TurnId>,
    ) -> Result<Vec<SubagentInvocation>> {
        let mut sql = String::from(
            r#"SELECT id, tool_call_id, parent_session_id, parent_turn_id,
                      requested_count, request_json, status, created_at, updated_at
               FROM subagent_invocations
               WHERE parent_session_id = ?"#,
        );
        if parent_turn_id.is_some() {
            sql.push_str(" AND parent_turn_id = ?");
        }
        sql.push_str(" ORDER BY created_at ASC");
        let sql = self.rewrite_sql(&sql);
        let mut query = sqlx::query(sql.as_ref()).bind(parent_session_id.0.to_string());
        if let Some(turn_id) = parent_turn_id {
            query = query.bind(turn_id.0.to_string());
        }
        let rows = query.fetch_all(&self.pool).await?;

        let mut invocations = Vec::with_capacity(rows.len());
        for r in rows {
            if let Ok(invocation) = build_subagent_invocation_from_row(r) {
                invocations.push(invocation);
            }
        }
        if invocations.is_empty() {
            return Ok(invocations);
        }

        let mut child_sql = String::from(
            r#"SELECT invocation_id, child_session_id, run_id, position, status,
                      label, harness, model, reasoning_effort, prompt_length,
                      created_at, updated_at
               FROM subagent_invocation_children
               WHERE invocation_id IN ("#,
        );
        for i in 0..invocations.len() {
            if i > 0 {
                child_sql.push_str(", ");
            }
            child_sql.push('?');
        }
        child_sql.push_str(") ORDER BY position ASC");
        let child_sql = self.rewrite_sql(&child_sql);
        let mut child_query = sqlx::query(child_sql.as_ref());
        for invocation in &invocations {
            child_query = child_query.bind(invocation.id.clone());
        }
        let child_rows = child_query.fetch_all(&self.pool).await?;

        let mut children_by_id: HashMap<String, Vec<SubagentInvocationChild>> = HashMap::new();
        for r in child_rows {
            if let Ok(child) = build_subagent_invocation_child_from_row(r) {
                children_by_id
                    .entry(child.invocation_id.clone())
                    .or_default()
                    .push(child);
            }
        }
        for invocation in &mut invocations {
            if let Some(children) = children_by_id.remove(&invocation.id) {
                invocation.children = children;
            }
        }

        Ok(invocations)
    }
}
