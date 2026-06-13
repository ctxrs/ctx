use std::time::Instant;

use ctx_route_contracts::sessions::{
    parse_boolish_flag, parse_session_id, parse_turn_id, SESSION_EVENTS_DEFAULT_LIMIT,
    SESSION_EVENTS_MAX_LIMIT,
};
use ctx_route_contracts::sessions::{
    SessionEventsRouteQuery, SessionEventsRouteResponse, SessionHeadRouteQuery,
    SessionHeadRouteResponse, SessionHistoryRouteQuery, SessionHistoryRouteResponse,
    SessionReadModelRouteError, SessionSnapshotRouteQuery, SessionSnapshotRouteResponse,
    SessionStateRouteResponse, SessionTurnToolsRouteResponse,
};

use crate::daemon::SessionReadModelsHandle;

use super::common::{SessionRouteParams, SessionTurnToolsRouteParams};

const SESSION_HEAD_CACHE: &str = "session_head";

impl SessionReadModelsHandle {
    pub async fn load_session_snapshot_for_route(
        &self,
        params: SessionRouteParams,
        query: SessionSnapshotRouteQuery,
    ) -> Result<SessionSnapshotRouteResponse, SessionReadModelRouteError> {
        let session_id = parse_session_id(params.session_id())?;
        let limit = query.limit.unwrap_or(60);
        let include_events = parse_boolish_flag(query.include_events.as_deref(), "include_events")?;
        self.load_session_snapshot(session_id, limit, include_events)
            .await
            .map_err(|error| {
                tracing::warn!(session_id = %session_id.0, "session snapshot load failed: {error:#}");
                SessionReadModelRouteError::internal("failed to load session snapshot")
            })?
            .map(Into::into)
            .ok_or_else(|| SessionReadModelRouteError::not_found("session not found"))
    }

    pub async fn session_head_for_route(
        &self,
        params: SessionRouteParams,
        query: SessionHeadRouteQuery,
    ) -> Result<SessionHeadRouteResponse, SessionReadModelRouteError> {
        let session_id = parse_session_id(params.session_id())?;
        let limit = query.limit.unwrap_or(60);
        let include_events = parse_boolish_flag(query.include_events.as_deref(), "include_events")?;
        let min_event_seq = query.min_event_seq;

        let started_at = Instant::now();
        if matches!(min_event_seq, Some(value) if value < 0) {
            return Err(SessionReadModelRouteError::bad_request(
                "min_event_seq must be non-negative",
            ));
        }

        let workspace_id = match self.workspace_id_for_session(session_id).await {
            Ok(Some(workspace_id)) => workspace_id,
            Ok(None) => return Err(SessionReadModelRouteError::not_found("session not found")),
            Err(error) => {
                tracing::warn!(session_id = %session_id.0, "session workspace lookup failed: {error:#}");
                return Err(SessionReadModelRouteError::internal(
                    "failed to load session head",
                ));
            }
        };
        if self.is_workspace_deleting(workspace_id).await {
            return Err(SessionReadModelRouteError::not_found("session not found"));
        }

        if let Some(head) = self
            .cached_session_head_for_request(session_id, include_events, limit, min_event_seq)
            .await
        {
            self.record_session_head_recovery_metrics(
                "active_snapshot_cache",
                "ok",
                started_at.elapsed(),
                limit,
                include_events,
                Some(&head),
            );
            return Ok(head.into());
        }

        self.emit_cache_miss(SESSION_HEAD_CACHE).await;
        match self
            .load_session_head_snapshot_from_store(session_id, limit, include_events)
            .await
        {
            Ok(Some(head)) => {
                if matches!(min_event_seq, Some(min_event_seq) if head.last_event_seq < min_event_seq)
                {
                    self.emit_cache_rehydrate(SESSION_HEAD_CACHE, false).await;
                    self.record_session_head_recovery_metrics(
                        "store_rebuild",
                        "stale",
                        started_at.elapsed(),
                        limit,
                        include_events,
                        Some(&head),
                    );
                    return Err(SessionReadModelRouteError::conflict(
                        "session head is stale",
                    ));
                }

                self.emit_cache_rehydrate(SESSION_HEAD_CACHE, true).await;
                self.record_session_head_recovery_metrics(
                    "store_rebuild",
                    "ok",
                    started_at.elapsed(),
                    limit,
                    include_events,
                    Some(&head),
                );
                self.update_session_head_cache(head.clone(), include_events)
                    .await;
                Ok(head.into())
            }
            Ok(None) => {
                self.emit_cache_rehydrate(SESSION_HEAD_CACHE, false).await;
                self.record_session_head_recovery_metrics(
                    "store_rebuild",
                    "missing",
                    started_at.elapsed(),
                    limit,
                    include_events,
                    None,
                );
                Err(SessionReadModelRouteError::not_found("session not found"))
            }
            Err(error) => {
                self.emit_cache_rehydrate(SESSION_HEAD_CACHE, false).await;
                self.record_session_head_recovery_metrics(
                    "store_rebuild",
                    "error",
                    started_at.elapsed(),
                    limit,
                    include_events,
                    None,
                );
                tracing::warn!(session_id = %session_id.0, "session head load failed: {error:#}");
                Err(SessionReadModelRouteError::internal(
                    "failed to load session head",
                ))
            }
        }
    }

    pub async fn load_session_history_page_for_route(
        &self,
        params: SessionRouteParams,
        query: SessionHistoryRouteQuery,
    ) -> Result<SessionHistoryRouteResponse, SessionReadModelRouteError> {
        let session_id = parse_session_id(params.session_id())?;
        let limit = query.limit.unwrap_or(60);
        self.load_session_history_page(session_id, query.before_seq, limit)
            .await
            .map_err(|error| {
                tracing::warn!(session_id = %session_id.0, "session history load failed: {error:#}");
                SessionReadModelRouteError::internal("failed to load session history")
            })?
            .map(Into::into)
            .ok_or_else(|| SessionReadModelRouteError::not_found("session not found"))
    }

    pub async fn list_session_events_page_for_route(
        &self,
        params: SessionRouteParams,
        query: SessionEventsRouteQuery,
    ) -> Result<SessionEventsRouteResponse, SessionReadModelRouteError> {
        let session_id = parse_session_id(params.session_id())?;
        let limit = query
            .limit
            .unwrap_or(SESSION_EVENTS_DEFAULT_LIMIT)
            .clamp(1, SESSION_EVENTS_MAX_LIMIT);
        let include_transient =
            parse_boolish_flag(query.include_transient.as_deref(), "include_transient")?;
        self.list_session_events_page(
            session_id,
            query.after_seq,
            limit,
            query.tail,
            include_transient,
        )
        .await
        .map_err(|error| {
            tracing::warn!(session_id = %session_id.0, "session events load failed: {error:#}");
            SessionReadModelRouteError::internal("failed to load session events")
        })?
        .map(Into::into)
        .ok_or_else(|| SessionReadModelRouteError::not_found("session not found"))
    }

    pub async fn load_session_state_for_route(
        &self,
        params: SessionRouteParams,
    ) -> Result<SessionStateRouteResponse, SessionReadModelRouteError> {
        let session_id = parse_session_id(params.session_id())?;
        self.load_session_state(session_id)
            .await
            .map_err(|error| {
                tracing::warn!(session_id = %session_id.0, "session state load failed: {error:#}");
                SessionReadModelRouteError::internal("failed to load session state")
            })?
            .map(Into::into)
            .ok_or_else(|| SessionReadModelRouteError::not_found("session not found"))
    }

    pub async fn list_session_turn_tools_for_route(
        &self,
        params: SessionTurnToolsRouteParams,
    ) -> Result<SessionTurnToolsRouteResponse, SessionReadModelRouteError> {
        let session_id = parse_session_id(params.session_id())?;
        let turn_id = parse_turn_id(params.turn_id())?;
        self.list_session_turn_tools_for_request(session_id, turn_id)
            .await
            .map_err(|error| {
                tracing::warn!(session_id = %session_id.0, turn_id = %turn_id.0, "session turn tools load failed: {error:#}");
                SessionReadModelRouteError::internal("failed to load session turn tools")
            })?
            .map(Into::into)
            .ok_or_else(|| SessionReadModelRouteError::not_found("session not found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ctx_route_contracts::sessions::SessionReadModelRouteErrorKind;

    use crate::test_support::TestDaemon;

    async fn seeded_daemon() -> (tempfile::TempDir, TestDaemon) {
        let temp = tempfile::tempdir().expect("tempdir");
        let daemon = TestDaemon::new_for_test(
            temp.path().join("data"),
            "http://127.0.0.1:4310".to_string(),
        )
        .await
        .expect("daemon");
        (temp, daemon)
    }

    #[tokio::test]
    async fn session_read_models_keep_archived_subagents_readable() {
        let (_temp, daemon) = seeded_daemon().await;
        let fixture = daemon
            .seed_cache_rehydration_primary_and_subagent_for_test()
            .await
            .expect("fixture");
        let event = daemon
            .cache_rehydration_seed_completed_notice_for_test(
                &fixture.subagent,
                fixture.task.id,
                serde_json::json!({"msg": "archived child remains readable"}),
                Some("archived child answer"),
            )
            .await
            .expect("seed subagent event");
        assert!(daemon
            .archive_task_lifecycle_subagent_session_for_test(
                fixture.workspace.id,
                fixture.primary.id,
                fixture.subagent.id,
            )
            .await
            .expect("archive subagent"));

        let handle = daemon.session_read_models_handle_for_test();
        let snapshot = handle
            .load_session_snapshot(fixture.subagent.id, 10, true)
            .await
            .expect("load archived subagent snapshot")
            .expect("archived subagent snapshot");
        assert_eq!(snapshot.summary.session.id, fixture.subagent.id);

        let events = handle
            .list_session_events_page(fixture.subagent.id, None, 10, None, false)
            .await
            .expect("load archived subagent events")
            .expect("archived subagent events");
        assert!(
            events.events.iter().any(|row| row.seq == event.event.seq),
            "archived read-model events should include seeded event: {:?}",
            events.events
        );
    }

    #[tokio::test]
    async fn session_head_route_hides_deleting_workspace_before_cached_head_recovery() {
        let (_temp, daemon) = seeded_daemon().await;
        let fixture = daemon
            .seed_cache_rehydration_session_for_test(true, true)
            .await
            .expect("fixture");
        daemon
            .cache_rehydration_seed_completed_notice_for_test(
                &fixture.session,
                fixture.task.id,
                serde_json::json!({"msg": "cached head must not leak while deleting"}),
                Some("cached answer"),
            )
            .await
            .expect("seed event");
        let head = daemon
            .cache_rehydration_full_head_for_test(fixture.session.id, 60, true)
            .await
            .expect("full head");
        daemon
            .cache_rehydration_seed_replay_head_cache_for_test(head)
            .await;
        assert!(
            daemon
                .cache_rehydration_replay_session_head_cached_for_test(fixture.session.id)
                .await
                .is_some(),
            "test requires an existing replay-capable cached head"
        );

        daemon
            .cache_rehydration_begin_workspace_delete_for_test(fixture.workspace.id)
            .await;
        let error = daemon
            .session_read_models_handle_for_test()
            .session_head_for_route(
                SessionRouteParams::new(fixture.session.id.0.to_string()),
                SessionHeadRouteQuery {
                    include_events: Some("true".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("deleting workspace must not serve cached session head");
        daemon
            .cache_rehydration_finish_workspace_delete_for_test(fixture.workspace.id)
            .await;

        assert_eq!(error.kind(), SessionReadModelRouteErrorKind::NotFound);
        assert_eq!(error.message(), "session not found");
    }
}
