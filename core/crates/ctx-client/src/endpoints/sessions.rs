use anyhow::Result;
use reqwest::Method;
use url::form_urlencoded;

use ctx_core::ids::{SessionId, TaskId, TurnId, WorkspaceId};
use ctx_core::models::{
    Message, Session, SessionEventsPage, SessionHeadSnapshot, SessionHistoryPage, SessionSnapshot,
    SessionState, SessionTurnTool,
};

use crate::client::Client;
use crate::types::*;

impl Client {
    pub async fn create_session(
        &self,
        task_id: TaskId,
        req: &CreateSessionRequest,
    ) -> Result<Session> {
        let path = format!("/api/tasks/{}/sessions", task_id.0);
        self.request_json(Method::POST, &path, Some(req)).await
    }

    pub async fn list_task_sessions(&self, task_id: TaskId) -> Result<Vec<Session>> {
        let path = format!("/api/tasks/{}/sessions", task_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn post_message(
        &self,
        session_id: SessionId,
        req: &PostMessageRequest,
    ) -> Result<Message> {
        let path = format!("/api/sessions/{}/messages", session_id.0);
        self.request_json(Method::POST, &path, Some(req)).await
    }

    pub async fn get_session_snapshot(
        &self,
        session_id: SessionId,
        limit: Option<u32>,
        include_events: Option<bool>,
    ) -> Result<SessionSnapshot> {
        let mut path = format!("/api/sessions/{}/snapshot", session_id.0);
        let mut params = Vec::new();
        if let Some(limit) = limit {
            params.push(format!("limit={limit}"));
        }
        if let Some(include_events) = include_events {
            params.push(format!(
                "include_events={}",
                if include_events { "1" } else { "0" }
            ));
        }
        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn get_session_head(
        &self,
        session_id: SessionId,
        limit: Option<u32>,
        include_events: Option<bool>,
    ) -> Result<SessionHeadSnapshot> {
        let mut path = format!("/api/sessions/{}/head", session_id.0);
        let mut params = Vec::new();
        if let Some(limit) = limit {
            params.push(format!("limit={limit}"));
        }
        if let Some(include_events) = include_events {
            params.push(format!(
                "include_events={}",
                if include_events { "1" } else { "0" }
            ));
        }
        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn get_session_state(&self, session_id: SessionId) -> Result<SessionState> {
        let path = format!("/api/sessions/{}/state", session_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn get_session_diff(&self, session_id: SessionId) -> Result<SessionDiffResponse> {
        let path = format!("/api/sessions/{}/diff", session_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn get_session_diff_summary(
        &self,
        session_id: SessionId,
    ) -> Result<SessionDiffSummaryResponse> {
        let path = format!("/api/sessions/{}/diff/summary", session_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn get_session_git_status(
        &self,
        session_id: SessionId,
    ) -> Result<SessionGitStatusResponse> {
        let path = format!("/api/sessions/{}/git/status", session_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn apply_session_diff_patch(
        &self,
        session_id: SessionId,
        action: &str,
        patch: &str,
    ) -> Result<SessionDiffResponse> {
        let path = format!("/api/sessions/{}/diff/apply", session_id.0);
        let req = SessionDiffApplyRequest {
            action: action.to_string(),
            patch: patch.to_string(),
        };
        self.request_json(Method::POST, &path, Some(&req)).await
    }

    pub async fn get_session_history(
        &self,
        session_id: SessionId,
        before_seq: Option<i64>,
        limit: Option<u32>,
    ) -> Result<SessionHistoryPage> {
        let mut path = format!("/api/sessions/{}/history", session_id.0);
        let mut params = Vec::new();
        if let Some(before_seq) = before_seq {
            params.push(format!("before_seq={before_seq}"));
        }
        if let Some(limit) = limit {
            params.push(format!("limit={limit}"));
        }
        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn get_session_events(
        &self,
        session_id: SessionId,
        after_seq: Option<i64>,
        limit: Option<u32>,
        tail: Option<u32>,
    ) -> Result<SessionEventsPage> {
        let mut path = format!("/api/sessions/{}/events", session_id.0);
        let mut params = Vec::new();
        if let Some(after_seq) = after_seq {
            params.push(format!("after_seq={after_seq}"));
        }
        if let Some(limit) = limit {
            params.push(format!("limit={limit}"));
        }
        if let Some(tail) = tail {
            params.push(format!("tail={tail}"));
        }
        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn list_turn_tools(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<Vec<SessionTurnTool>> {
        let path = format!("/api/sessions/{}/turns/{}/tools", session_id.0, turn_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn list_session_file_completions(
        &self,
        session_id: SessionId,
        query: &str,
        limit: Option<u32>,
    ) -> Result<Vec<String>> {
        let path = {
            let mut serializer = form_urlencoded::Serializer::new(String::new());
            serializer.append_pair("query", query);
            if let Some(limit) = limit {
                serializer.append_pair("limit", &limit.to_string());
            }
            let qs = serializer.finish();
            format!("/api/sessions/{}/completions/files?{}", session_id.0, qs)
        };
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn list_workspace_file_completions(
        &self,
        workspace_id: WorkspaceId,
        query: &str,
        limit: Option<u32>,
    ) -> Result<Vec<String>> {
        let path = {
            let mut serializer = form_urlencoded::Serializer::new(String::new());
            serializer.append_pair("query", query);
            if let Some(limit) = limit {
                serializer.append_pair("limit", &limit.to_string());
            }
            let qs = serializer.finish();
            format!(
                "/api/workspaces/{}/completions/files?{}",
                workspace_id.0, qs
            )
        };
        self.request_json(Method::GET, &path, None::<&()>).await
    }
}
