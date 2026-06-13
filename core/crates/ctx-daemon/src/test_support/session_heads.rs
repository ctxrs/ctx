use std::time::Duration;

use ctx_core::ids::{MessageId, RunId, SessionEventId, SessionId, TaskId, TurnId, WorkspaceId};
use ctx_core::models::{
    Message, MessageDelivery, MessageRole, Session, SessionEvent, SessionEventType,
    SessionHeadDelta, SessionHeadSnapshot, SessionTurn, SessionTurnStatus,
};
use sqlx::{QueryBuilder, Sqlite};

use super::{
    fixed_test_utc, latest_ctx_ui_sized_turn_id, seed_ctx_ui_sized_session,
    tail_ctx_ui_sized_turn_ids, CtxUiSizedHeadSeedSpec, CtxUiSizedHeadSeedStats,
    CtxUiSizedToolSummaryProbe, HotEndpointManualHeadProbe, TestDaemon,
};

impl TestDaemon {
    pub async fn seed_large_session_head_fixture_for_test(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
        task_id: TaskId,
        turns: i64,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        struct SeedRow {
            index: i64,
            event_seq: i64,
            event_id: String,
            message_id: String,
            run_id: String,
            turn_id: String,
            created_at: String,
            payload_json: String,
            input_json: String,
        }

        let store = self.state.store_for_session(session_id).await?;
        let session_id_value = session_id.0.to_string();
        let task_id_value = task_id.0.to_string();
        let started_at = chrono::Utc::now();
        // Seed fixture rows directly so this response-size test does not enqueue
        // projection work once per row before the explicit refresh below.
        let rows = (0..turns)
            .map(|index| {
                let created_at = started_at + chrono::Duration::milliseconds(index);
                SeedRow {
                    index,
                    event_seq: index + 1,
                    event_id: uuid::Uuid::new_v4().to_string(),
                    message_id: MessageId::new().0.to_string(),
                    run_id: RunId::new().0.to_string(),
                    turn_id: TurnId::new().0.to_string(),
                    created_at: created_at.to_rfc3339(),
                    payload_json: serde_json::json!({
                        "kind": "large_head_checkpoint",
                        "turn_index": index,
                    })
                    .to_string(),
                    input_json: serde_json::json!({ "cmd": format!("echo {index}") }).to_string(),
                }
            })
            .collect::<Vec<_>>();

        let mut turn_builder = QueryBuilder::<Sqlite>::new(
            r#"INSERT INTO session_turns (
                turn_id, session_id, run_id, user_message_id, status, start_seq, end_seq,
                started_at, updated_at, assistant_partial, thought_partial, metrics_json,
                tool_total, tool_pending, tool_running, tool_completed, tool_failed
            ) "#,
        );
        turn_builder.push_values(&rows, |mut values, row| {
            values
                .push_bind(&row.turn_id)
                .push_bind(&session_id_value)
                .push_bind(&row.run_id)
                .push_bind(Option::<String>::None)
                .push_bind("completed")
                .push_bind(row.index + 1)
                .push_bind(row.index + 1)
                .push_bind(&row.created_at)
                .push_bind(&row.created_at)
                .push_bind(Option::<String>::None)
                .push_bind(Option::<String>::None)
                .push_bind(Option::<String>::None)
                .push_bind(1_i64)
                .push_bind(0_i64)
                .push_bind(0_i64)
                .push_bind(1_i64)
                .push_bind(0_i64);
        });
        turn_builder.build().execute(store.pool()).await?;

        let mut event_builder = QueryBuilder::<Sqlite>::new(
            r#"INSERT INTO session_events (
                seq, id, session_id, run_id, turn_id, event_type, payload_json, transient, created_at
            ) "#,
        );
        event_builder.push_values(&rows, |mut values, row| {
            values
                .push_bind(row.event_seq)
                .push_bind(&row.event_id)
                .push_bind(&session_id_value)
                .push_bind(&row.run_id)
                .push_bind(&row.turn_id)
                .push_bind("notice")
                .push_bind(&row.payload_json)
                .push_bind(0_i64)
                .push_bind(&row.created_at);
        });
        event_builder.build().execute(store.pool()).await?;

        let mut message_builder = QueryBuilder::<Sqlite>::new(
            r#"INSERT INTO messages (
                id, session_id, task_id, run_id, turn_id, turn_sequence, order_seq, role, content,
                attachments_json, delivery, delivered_at, created_at
            ) "#,
        );
        message_builder.push_values(&rows, |mut values, row| {
            values
                .push_bind(&row.message_id)
                .push_bind(&session_id_value)
                .push_bind(&task_id_value)
                .push_bind(&row.run_id)
                .push_bind(&row.turn_id)
                .push_bind(1_i64)
                .push_bind(Option::<i64>::None)
                .push_bind("assistant")
                .push_bind(format!("answer {}", row.index))
                .push_bind("[]")
                .push_bind("immediate")
                .push_bind(Option::<String>::None)
                .push_bind(&row.created_at);
        });
        message_builder.build().execute(store.pool()).await?;

        let mut tool_builder = QueryBuilder::<Sqlite>::new(
            r#"INSERT INTO session_turn_tools (
                session_id, tool_call_id, turn_id, tool_kind, provider_tool_name, title, subtitle,
                status, input_json, output_text, order_seq, first_event_seq, input_truncated,
                input_original_bytes, output_truncated, output_original_bytes, created_at, updated_at
            ) "#,
        );
        tool_builder.push_values(&rows, |mut values, row| {
            values
                .push_bind(&session_id_value)
                .push_bind(format!("tool-{}", row.index))
                .push_bind(&row.turn_id)
                .push_bind("execute")
                .push_bind("Bash")
                .push_bind("Bash")
                .push_bind(format!("turn {}", row.index))
                .push_bind("completed")
                .push_bind(&row.input_json)
                .push_bind(format!("output {}", row.index))
                .push_bind(1_i64)
                .push_bind(row.event_seq)
                .push_bind(0_i64)
                .push_bind(Option::<i64>::None)
                .push_bind(0_i64)
                .push_bind(Option::<i64>::None)
                .push_bind(&row.created_at)
                .push_bind(&row.created_at);
        });
        tool_builder.build().execute(store.pool()).await?;

        tokio::time::timeout(
            timeout,
            store.refresh_active_session_head_projection(session_id),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out refreshing active session head projection"))?
        .map_err(|err| anyhow::anyhow!("refresh active session head projection: {err}"))?;

        tokio::time::timeout(
            timeout,
            self.ensure_workspace_active_snapshot_hydrated(workspace_id),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out hydrating workspace active snapshot"))?
        .map_err(|err| anyhow::anyhow!("hydrate workspace active snapshot: {err:?}"))?;

        Ok(())
    }

    pub async fn seed_ctx_ui_sized_session_head_fixture_for_test(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
        task_id: TaskId,
        seed: CtxUiSizedHeadSeedSpec,
        timeout: Duration,
    ) -> anyhow::Result<CtxUiSizedHeadSeedStats> {
        let store = self.state.store_for_session(session_id).await?;
        seed_ctx_ui_sized_session(&store, session_id, task_id, &seed).await?;

        let event_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_events WHERE session_id = ?")
                .bind(session_id.0.to_string())
                .fetch_one(store.pool())
                .await?;
        let tool_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_turn_tools WHERE session_id = ?")
                .bind(session_id.0.to_string())
                .fetch_one(store.pool())
                .await?;
        let message_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE session_id = ?")
                .bind(session_id.0.to_string())
                .fetch_one(store.pool())
                .await?;

        tokio::time::timeout(
            timeout,
            store.refresh_active_session_head_projection(session_id),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out refreshing active projection for ctx-ui fixture"))?
        .map_err(|err| anyhow::anyhow!("refresh active ctx-ui projection: {err}"))?;

        tokio::time::timeout(
            timeout,
            self.ensure_workspace_active_snapshot_hydrated(workspace_id),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out hydrating workspace active snapshot"))?
        .map_err(|err| anyhow::anyhow!("hydrate workspace active snapshot: {err:?}"))?;

        Ok(CtxUiSizedHeadSeedStats {
            event_count,
            tool_count,
            message_count,
        })
    }

    pub async fn ctx_ui_sized_recent_tool_summary_probe_for_test(
        &self,
        session_id: SessionId,
        head_limit: i64,
        tool_summary_limit: usize,
    ) -> anyhow::Result<CtxUiSizedToolSummaryProbe> {
        let store = self.state.store_for_session(session_id).await?;
        let latest_turn_id = latest_ctx_ui_sized_turn_id(&store, session_id).await?;
        let tail_turn_ids = tail_ctx_ui_sized_turn_ids(&store, session_id, head_limit).await?;
        let bounded_tools = store
            .list_recent_turn_tool_summaries_for_turns(
                session_id,
                &tail_turn_ids,
                tool_summary_limit,
            )
            .await?;
        let oldest_loaded_order_seq = bounded_tools
            .iter()
            .map(|tool| tool.order_seq)
            .min()
            .ok_or_else(|| anyhow::anyhow!("ctx-ui sized tool probe returned no tools"))?;
        Ok(CtxUiSizedToolSummaryProbe {
            latest_turn_id,
            bounded_tool_count: bounded_tools.len(),
            oldest_loaded_order_seq,
        })
    }

    pub async fn seed_hot_endpoint_caches_for_test(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
        session_id: SessionId,
        limit: i64,
        session_head_limit: u32,
        include_events: bool,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let store = self.state.store_for_session(session_id).await?;
        let _ = store
            .append_session_event(
                session_id,
                None,
                None,
                SessionEventType::Notice,
                serde_json::json!({"msg":"warm"}),
            )
            .await
            .map_err(|err| anyhow::anyhow!("append warm session event: {err}"))?;
        let _ = store
            .refresh_active_session_head_projection(session_id)
            .await
            .map_err(|err| anyhow::anyhow!("refresh active session head projection: {err}"))?;

        self.state
            .task_publication
            .emit_workspace_task_upsert(task_id)
            .await?;
        self.state
            .task_session_cleanup
            .refresh_session_head_cache(session_id)
            .await;

        let head_snapshot = store
            .get_session_head_snapshot(session_id, session_head_limit, include_events)
            .await
            .map_err(|err| anyhow::anyhow!("load session head snapshot: {err}"))?
            .ok_or_else(|| anyhow::anyhow!("session head snapshot {session_id:?} not found"))?;
        self.state
            .sessions
            .cache_session_head_snapshot(
                session_id,
                session_head_limit,
                include_events,
                head_snapshot,
            )
            .await;

        let deadline = tokio::time::Instant::now() + timeout;
        let mut cached_snapshot = self
            .state
            .workspaces
            .workspace_active_snapshot
            .active_snapshot(workspace_id, limit)
            .await;
        while cached_snapshot.active.tasks.is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cached_snapshot = self
                .state
                .workspaces
                .workspace_active_snapshot
                .active_snapshot(workspace_id, limit)
                .await;
        }
        if cached_snapshot.active.tasks.is_empty() {
            anyhow::bail!("expected active snapshot to be cached for workspace {workspace_id:?}");
        }
        self.state
            .test_cache_workspace_active_snapshot(cached_snapshot)
            .await;

        self.ensure_workspace_active_snapshot_hydrated(workspace_id)
            .await
            .map_err(|err| anyhow::anyhow!("hydrate workspace active snapshot: {err:?}"))?;

        let mut cached_heads = self
            .state
            .workspaces
            .workspace_active_snapshot
            .active_heads(workspace_id)
            .await;
        while cached_heads.heads.is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cached_heads = self
                .state
                .workspaces
                .workspace_active_snapshot
                .active_heads(workspace_id)
                .await;
        }
        if cached_heads.heads.is_empty() {
            anyhow::bail!("expected active heads to be cached for workspace {workspace_id:?}");
        }
        self.state
            .test_cache_workspace_active_heads(cached_heads)
            .await;

        Ok(())
    }

    pub async fn append_hot_endpoint_delta_notice_for_test(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<SessionEvent> {
        self.state
            .store_for_session(session_id)
            .await?
            .append_session_event(
                session_id,
                None,
                None,
                SessionEventType::Notice,
                serde_json::json!({"msg":"delta only"}),
            )
            .await
            .map_err(|err| anyhow::anyhow!("append delta session event: {err}"))
    }

    pub async fn probe_hot_endpoint_manual_session_head_for_test(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<HotEndpointManualHeadProbe> {
        let store = self.state.store_for_session(session_id).await?;
        match store.get_session_head_snapshot(session_id, 10, true).await {
            Ok(_) => Ok(HotEndpointManualHeadProbe::UnexpectedlySucceeded),
            Err(_) => Ok(HotEndpointManualHeadProbe::FailedClosed),
        }
    }

    pub async fn publish_hot_endpoint_event_and_active_head_seq_for_test(
        &self,
        workspace_id: WorkspaceId,
        event: SessionEvent,
        settle_for: Duration,
    ) -> anyhow::Result<i64> {
        self.state.session_publication.publish_event(event).await;
        tokio::time::sleep(settle_for).await;
        let active_heads = self
            .state
            .workspaces
            .workspace_active_snapshot
            .active_heads(workspace_id)
            .await;
        let head = active_heads.heads.first().ok_or_else(|| {
            anyhow::anyhow!("expected active head for workspace {workspace_id:?}")
        })?;
        Ok(head.last_event_seq)
    }

    pub async fn seed_fault_matrix_replay_notice_for_test(
        &self,
        task_id: TaskId,
        session_id: SessionId,
    ) -> anyhow::Result<i64> {
        let store = self.state.store_for_task(task_id).await?;
        let sessions = store.list_sessions_for_task(task_id).await?;
        if !sessions.iter().any(|session| session.id == session_id) {
            anyhow::bail!("expected session {session_id:?} to belong to task {task_id:?}");
        }
        Ok(store
            .append_session_event(
                session_id,
                None,
                None,
                SessionEventType::Notice,
                serde_json::json!({"msg":"last"}),
            )
            .await?
            .seq)
    }

    pub async fn seed_workspace_stream_stress_session_head_for_test(
        &self,
        session: &Session,
        turns_per_session: i64,
        message_content: &str,
        head_limit: u32,
    ) -> anyhow::Result<()> {
        let store = self.state.store_for_session(session.id).await?;
        for turn_sequence in 0..turns_per_session {
            let turn_id = TurnId::new();
            let at = fixed_test_utc(turn_sequence);
            store
                .insert_session_turn(SessionTurn {
                    turn_id,
                    session_id: session.id,
                    run_id: None,
                    user_message_id: None,
                    status: SessionTurnStatus::Completed,
                    start_seq: Some(turn_sequence),
                    end_seq: Some(turn_sequence),
                    started_at: at,
                    updated_at: at,
                    assistant_partial: None,
                    thought_partial: None,
                    metrics_json: None,
                    failure: None,
                    tool_total: 0,
                    tool_pending: 0,
                    tool_running: 0,
                    tool_completed: 0,
                    tool_failed: 0,
                })
                .await?;

            store
                .insert_message(Message {
                    id: MessageId::new(),
                    session_id: session.id,
                    task_id: session.task_id,
                    run_id: None,
                    turn_id: Some(turn_id),
                    turn_sequence: Some(turn_sequence),
                    order_seq: None,
                    role: MessageRole::User,
                    content: message_content.to_string(),
                    attachments: Vec::new(),
                    delivery: MessageDelivery::Immediate,
                    delivered_at: None,
                    created_at: at,
                })
                .await?;
        }

        let head = store
            .get_session_head_snapshot(session.id, head_limit, true)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session head snapshot {:?} not found", session.id))?;
        self.state.test_update_session_head(head).await;
        Ok(())
    }

    pub async fn publish_workspace_stream_stress_delta_for_test(
        &self,
        workspace_id: WorkspaceId,
        session: &Session,
        seq: i64,
    ) {
        let delta = SessionHeadDelta {
            session_id: session.id,
            last_event_seq: seq,
            projection_rev: seq,
            state_rev: 0,
            emitted_at_ms: None,
            session: None,
            activity: None,
            event: Some(SessionEvent {
                seq,
                id: SessionEventId::new(),
                session_id: session.id,
                run_id: None,
                turn_id: None,
                event_type: SessionEventType::Done,
                payload_json: serde_json::json!({"ok": true}),
                transient: false,
                created_at: chrono::Utc::now(),
            }),
            turn: None,
            message: None,
            tool_summaries: Vec::new(),
        };
        self.state
            .test_publish_session_head_delta_for_workspace(workspace_id, session, delta, false)
            .await;
    }

    pub async fn cache_rehydration_seed_replay_head_cache_for_test(
        &self,
        head: SessionHeadSnapshot,
    ) {
        self.state
            .workspaces
            .workspace_active_snapshot
            .update_session_head(head)
            .await;
    }

    pub async fn cache_rehydration_seed_compact_head_cache_for_test(
        &self,
        head: SessionHeadSnapshot,
    ) {
        self.state
            .workspaces
            .workspace_active_snapshot
            .update_compact_session_head(head)
            .await;
    }

    pub async fn cache_rehydration_replay_session_head_cached_for_test(
        &self,
        session_id: SessionId,
    ) -> Option<SessionHeadSnapshot> {
        self.state
            .workspaces
            .workspace_active_snapshot
            .get_session_head(session_id)
            .await
    }

    pub async fn cache_rehydration_session_head_for_read_cached_for_test(
        &self,
        session_id: SessionId,
    ) -> Option<SessionHeadSnapshot> {
        self.state
            .workspaces
            .workspace_active_snapshot
            .get_cached_session_head_for_read(session_id)
            .await
    }
}
