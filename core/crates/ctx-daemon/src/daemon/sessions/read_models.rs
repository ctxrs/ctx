use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use ctx_core::ids::{SessionId, TurnId, WorkspaceId};
use ctx_core::models::{
    Session, SessionEventsPage, SessionHeadSnapshot, SessionHistoryPage, SessionSnapshot,
    SessionState, SessionTurnTool,
};
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind};
use ctx_store::Store;

use crate::daemon::SessionReadModelsHandle;
use crate::daemon::SessionStoreAccessError;

impl SessionReadModelsHandle {
    pub async fn load_session_snapshot(
        &self,
        session_id: SessionId,
        limit: u32,
        include_events: bool,
    ) -> Result<Option<SessionSnapshot>> {
        let Some(store) = self
            .session_store_allow_archived_or_none(session_id)
            .await?
        else {
            return Ok(None);
        };
        store
            .get_session_snapshot(session_id, limit, include_events)
            .await
    }

    pub async fn list_session_events_page(
        &self,
        session_id: SessionId,
        after_seq: Option<i64>,
        limit: u32,
        tail: Option<u32>,
        include_transient: bool,
    ) -> Result<Option<SessionEventsPage>> {
        const MAX_LIMIT: u32 = 1000;

        let Some(store) = self
            .session_store_allow_archived_or_none(session_id)
            .await?
        else {
            return Ok(None);
        };

        let (events, has_more, next_cursor) = if let Some(tail) = tail {
            let tail = tail.clamp(1, MAX_LIMIT);
            let mut rows = store
                .list_session_events_tail_by_seq(session_id, tail + 1, include_transient)
                .await?;
            let has_more = rows.len() as u32 > tail;
            if has_more {
                rows = rows.split_off(rows.len().saturating_sub(tail as usize));
            }
            let next_cursor = rows.last().map(|ev| ev.seq);
            (rows, has_more, next_cursor)
        } else {
            let limit = limit.clamp(1, MAX_LIMIT);
            let mut rows = store
                .list_session_events_page_by_seq(
                    session_id,
                    after_seq,
                    Some(limit + 1),
                    include_transient,
                )
                .await?;
            let has_more = rows.len() as u32 > limit;
            if has_more {
                rows.truncate(limit as usize);
            }
            let next_cursor = rows.last().map(|ev| ev.seq);
            (rows, has_more, next_cursor)
        };

        Ok(Some(SessionEventsPage {
            session_id,
            events,
            next_cursor,
            has_more,
        }))
    }

    pub async fn load_session_history_page(
        &self,
        session_id: SessionId,
        before_seq: Option<i64>,
        limit: u32,
    ) -> Result<Option<SessionHistoryPage>> {
        let Some(store) = self
            .session_store_allow_archived_or_none(session_id)
            .await?
        else {
            return Ok(None);
        };
        store
            .get_session_history_page(session_id, before_seq, limit)
            .await
    }

    pub async fn list_session_turn_tools_for_request(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Option<Vec<SessionTurnTool>>> {
        let Some(store) = self
            .session_store_allow_archived_or_none(session_id)
            .await?
        else {
            return Ok(None);
        };
        store.list_turn_tools(session_id, turn_id).await.map(Some)
    }

    pub async fn load_session_state(&self, session_id: SessionId) -> Result<Option<SessionState>> {
        let Some(store) = self
            .session_store_allow_archived_or_none(session_id)
            .await?
        else {
            return Ok(None);
        };
        let Some(session) = store.get_session(session_id).await? else {
            return Ok(None);
        };
        let mut session_state = store.get_session_state(session_id).await?;
        for artifact in session_state.artifacts.iter_mut() {
            if !self
                .session_artifact_path_is_accessible(
                    &store,
                    &session,
                    Path::new(&artifact.absolute_path),
                )
                .await?
            {
                artifact.missing = Some(true);
            }
        }
        Ok(Some(session_state))
    }

    pub(in crate::daemon) async fn load_session_head_snapshot_from_store(
        &self,
        session_id: SessionId,
        limit: u32,
        include_events: bool,
    ) -> Result<Option<SessionHeadSnapshot>> {
        let Some(store) = self
            .session_store_allow_archived_or_none(session_id)
            .await?
        else {
            return Ok(None);
        };
        store
            .get_session_head_snapshot(session_id, limit, include_events)
            .await
    }

    pub(in crate::daemon) async fn workspace_id_for_session(
        &self,
        session_id: SessionId,
    ) -> anyhow::Result<Option<WorkspaceId>> {
        self.global_store()
            .get_workspace_id_for_session(session_id)
            .await
    }

    pub(in crate::daemon) async fn is_workspace_deleting(&self, workspace_id: WorkspaceId) -> bool {
        self.stores().is_workspace_deleting(workspace_id).await
    }

    pub(in crate::daemon) async fn cached_session_head_for_request(
        &self,
        session_id: SessionId,
        include_events: bool,
        limit: u32,
        min_event_seq: Option<i64>,
    ) -> Option<SessionHeadSnapshot> {
        self.active_snapshot()
            .get_cached_session_head_for_request(session_id, include_events, limit, min_event_seq)
            .await
    }

    pub(in crate::daemon) async fn update_session_head_cache(
        &self,
        head: SessionHeadSnapshot,
        include_events: bool,
    ) {
        if include_events {
            self.active_snapshot().update_session_head(head).await;
        } else {
            self.active_snapshot()
                .update_compact_session_head(head)
                .await;
        }
    }

    pub(in crate::daemon) async fn emit_cache_miss(&self, cache: &str) {
        self.emit_cache_counter("daemon.cache_miss", cache, 1, None)
            .await;
    }

    pub(in crate::daemon) async fn emit_cache_rehydrate(&self, cache: &str, ok: bool) {
        let result = if ok { "ok" } else { "fail" };
        self.emit_cache_counter("daemon.cache_rehydrate", cache, 1, Some(("result", result)))
            .await;
    }

    pub(in crate::daemon) fn record_session_head_recovery_metrics(
        &self,
        source: &'static str,
        result: &'static str,
        elapsed: Duration,
        limit: u32,
        include_events: bool,
        head: Option<&SessionHeadSnapshot>,
    ) {
        let mut labels = HashMap::new();
        labels.insert("source".to_string(), "daemon".to_string());
        labels.insert("surface".to_string(), "session_head_recovery".to_string());
        labels.insert("recovery_source".to_string(), source.to_string());
        labels.insert("result".to_string(), result.to_string());
        labels.insert(
            "include_events".to_string(),
            if include_events { "true" } else { "false" }.to_string(),
        );
        labels.insert(
            "limit_bucket".to_string(),
            session_head_limit_bucket(limit).to_string(),
        );

        let response_bytes = head
            .map(|value| value.head_window.bytes.max(0) as f64)
            .unwrap_or(0.0);
        let metrics = [
            (
                "workbench.session_head_recovery_ms",
                "ms",
                elapsed.as_millis() as f64,
            ),
            (
                "workbench.session_head_recovery_response_bytes",
                "bytes",
                response_bytes,
            ),
            (
                "workbench.session_head_recovery_turn_count",
                "count",
                head.map(|value| value.turns.len() as f64).unwrap_or(0.0),
            ),
            (
                "workbench.session_head_recovery_message_count",
                "count",
                head.map(|value| value.messages.len() as f64).unwrap_or(0.0),
            ),
            (
                "workbench.session_head_recovery_tool_summary_count",
                "count",
                head.map(|value| value.tool_summaries.len() as f64)
                    .unwrap_or(0.0),
            ),
            (
                "workbench.session_head_recovery_event_count",
                "count",
                head.map(|value| value.events.len() as f64).unwrap_or(0.0),
            ),
        ];
        let perf_telemetry = self.perf_telemetry().clone();
        tokio::spawn(async move {
            for (name, unit, value) in metrics {
                perf_telemetry
                    .record_metric(
                        PerfMetric {
                            name: name.to_string(),
                            kind: PerfMetricKind::Histogram,
                            unit: unit.to_string(),
                            value,
                            labels: labels.clone(),
                        },
                        None,
                        None,
                        None,
                    )
                    .await;
            }
        });
    }

    async fn session_store_allow_archived_or_none(
        &self,
        session_id: SessionId,
    ) -> Result<Option<Store>> {
        match self
            .session_stores()
            .existing_session_store_allow_archived(session_id)
            .await
        {
            Ok(store) => Ok(Some(store)),
            Err(SessionStoreAccessError::NotFound) => Ok(None),
            Err(error) => Err(session_store_access_anyhow(error)),
        }
    }

    async fn session_artifact_path_is_accessible(
        &self,
        store: &Store,
        session: &Session,
        path: &Path,
    ) -> anyhow::Result<bool> {
        let session_spool_dir = self.tool_output_spool_dir().join(session.id.0.to_string());
        ctx_session_artifacts::session_artifact_path_is_accessible(
            store,
            session,
            &session_spool_dir,
            path,
        )
        .await
        .map_err(Into::into)
    }

    async fn emit_cache_counter(
        &self,
        name: &str,
        cache: &str,
        value: u64,
        extra_label: Option<(&str, &str)>,
    ) {
        if value == 0 {
            return;
        }
        let mut labels = HashMap::new();
        labels.insert("cache".to_string(), cache.to_string());
        labels.insert("source".to_string(), "daemon".to_string());
        if let Some((key, val)) = extra_label {
            labels.insert(key.to_string(), val.to_string());
        }
        let metric = PerfMetric {
            name: name.to_string(),
            kind: PerfMetricKind::Counter,
            unit: "count".to_string(),
            value: value as f64,
            labels,
        };
        self.perf_telemetry()
            .record_metric(metric, None, None, None)
            .await;
    }
}

fn session_store_access_anyhow(error: SessionStoreAccessError) -> anyhow::Error {
    match error {
        SessionStoreAccessError::NotFound => anyhow::anyhow!("session not found"),
        SessionStoreAccessError::LookupUnavailable(error) => error,
        SessionStoreAccessError::StoreUnavailable => anyhow::anyhow!("session store unavailable"),
    }
}

fn session_head_limit_bucket(limit: u32) -> &'static str {
    match limit {
        0 => "zero",
        1..=5 => "1_5",
        6..=60 => "6_60",
        61..=200 => "61_200",
        _ => "gt_200",
    }
}
