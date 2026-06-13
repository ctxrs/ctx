use anyhow::Result;
use reqwest::Method;
use serde::Deserialize;
use url::form_urlencoded;

use ctx_core::ids::{TaskId, TerminalId, WorkspaceId};
use ctx_core::models::{
    Task, TerminalSession, Workspace, WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot,
    WorkspaceArchivedPage,
};

use crate::client::Client;
use crate::types::*;

impl Client {
    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        self.request_json(Method::GET, "/api/workspaces", None::<&()>)
            .await
    }

    pub async fn create_workspace(
        &self,
        root_path: String,
        name: Option<String>,
    ) -> Result<Workspace> {
        let req = CreateWorkspaceRequest { root_path, name };
        self.request_json(Method::POST, "/api/workspaces", Some(&req))
            .await
    }

    pub async fn list_web_sessions(&self) -> Result<Vec<WebSessionInfo>> {
        self.request_json(Method::GET, "/api/sessions/web", None::<&()>)
            .await
    }

    pub async fn get_workspace(&self, workspace_id: WorkspaceId) -> Result<Workspace> {
        let path = format!("/api/workspaces/{}", workspace_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn list_workspace_tasks(&self, workspace_id: WorkspaceId) -> Result<Vec<Task>> {
        let path = format!("/api/workspaces/{}/tasks", workspace_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn create_task(
        &self,
        workspace_id: WorkspaceId,
        req: &CreateTaskRequest,
    ) -> Result<Task> {
        let path = format!("/api/workspaces/{}/tasks", workspace_id.0);
        self.request_json(Method::POST, &path, Some(req)).await
    }

    pub async fn update_task_title(&self, task_id: TaskId, title: &str) -> Result<Task> {
        let path = format!("/api/tasks/{}/title", task_id.0);
        let req = UpdateTaskTitleRequest { title };
        self.request_json(Method::POST, &path, Some(&req)).await
    }

    pub async fn delete_task(&self, task_id: TaskId) -> Result<()> {
        let path = format!("/api/tasks/{}", task_id.0);
        self.request_empty(Method::DELETE, &path, None::<&()>).await
    }

    pub async fn archive_task(&self, task_id: TaskId) -> Result<Task> {
        let path = format!("/api/tasks/{}/archive", task_id.0);
        self.request_json(Method::POST, &path, None::<&()>).await
    }

    pub async fn unarchive_task(&self, task_id: TaskId) -> Result<Task> {
        let path = format!("/api/tasks/{}/unarchive", task_id.0);
        self.request_json(Method::POST, &path, None::<&()>).await
    }

    pub async fn mark_task_read(&self, task_id: TaskId) -> Result<Task> {
        let path = format!("/api/tasks/{}/mark_read", task_id.0);
        self.request_json(Method::POST, &path, None::<&()>).await
    }

    pub async fn mark_task_unread(&self, task_id: TaskId) -> Result<Task> {
        let path = format!("/api/tasks/{}/mark_unread", task_id.0);
        self.request_json(Method::POST, &path, None::<&()>).await
    }

    pub async fn get_workspace_active_snapshot(
        &self,
        workspace_id: WorkspaceId,
        params: &WorkspaceActiveSnapshotParams,
    ) -> Result<WorkspaceActiveSnapshot> {
        let mut path = format!("/api/workspaces/{}/active_snapshot", workspace_id.0);
        let mut search = Vec::new();
        if let Some(limit) = params.limit {
            search.push(format!("limit={limit}"));
        }
        if !search.is_empty() {
            path.push('?');
            path.push_str(&search.join("&"));
        }
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn get_workspace_active_heads(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<WorkspaceActiveHeadBatch> {
        let path = format!("/api/workspaces/{}/active_heads", workspace_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn list_workspace_archived_task_summaries(
        &self,
        workspace_id: WorkspaceId,
        params: &WorkspaceArchivedPageParams,
    ) -> Result<WorkspaceArchivedPage> {
        let mut path = format!("/api/workspaces/{}/archived_task_summaries", workspace_id.0);
        let mut search = Vec::new();
        if let Some(limit) = params.limit {
            search.push(format!("limit={limit}"));
        }
        if let Some(cursor) = &params.cursor {
            let sort_at = cursor.sort_at.to_rfc3339();
            let sort_at = form_urlencoded::byte_serialize(sort_at.as_bytes()).collect::<String>();
            search.push(format!("cursor_sort_at={sort_at}"));
            search.push(format!("cursor_task_id={}", cursor.task_id.0));
        }
        if !search.is_empty() {
            path.push('?');
            path.push_str(&search.join("&"));
        }
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub fn workspace_stream_url(&self, workspace_id: WorkspaceId) -> Result<String> {
        self.websocket_url_for_path(&format!(
            "/api/workspaces/{}/active_snapshot/stream",
            workspace_id.0
        ))
    }

    pub async fn terminal_stream_url(&self, terminal: &TerminalSession) -> Result<String> {
        let path = format!("/api/terminals/{}/stream_token", terminal.id.0);
        let token: TerminalStreamConnectInfo =
            self.request_json(Method::POST, &path, None::<&()>).await?;
        self.websocket_url_for_path(&token.stream_path)
    }

    pub async fn list_workspace_terminals(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<TerminalSession>> {
        let path = format!("/api/workspaces/{}/terminals", workspace_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn create_workspace_terminal(
        &self,
        workspace_id: WorkspaceId,
        req: &CreateTerminalRequest,
    ) -> Result<TerminalSession> {
        let path = format!("/api/workspaces/{}/terminals", workspace_id.0);
        self.request_json(Method::POST, &path, Some(req)).await
    }

    pub async fn delete_terminal(&self, terminal_id: TerminalId) -> Result<()> {
        let path = format!("/api/terminals/{}", terminal_id.0);
        self.request_empty(Method::DELETE, &path, None::<&()>).await
    }
}

#[derive(Deserialize)]
struct TerminalStreamConnectInfo {
    stream_path: String,
}
