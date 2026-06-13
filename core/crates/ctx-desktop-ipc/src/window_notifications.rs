use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopMenuItemStateUpdate {
    #[ts(optional = nullable)]
    pub checked: Option<bool>,
    #[ts(optional = nullable)]
    pub enabled: Option<bool>,
    pub id: String,
    #[ts(optional = nullable)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopSetMenuStateReq {
    pub items: Vec<DesktopMenuItemStateUpdate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopSetOpenWorkspacesReq {
    pub workspace_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopOpenWorkspaceInNewWindowReq {
    pub workspace_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopRecordWorkspaceVisitReq {
    pub workspace_id: String,
    pub workspace_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DesktopWorkbenchRouteTask {
    pub task_id: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DesktopRecordWorkbenchRouteReq {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub active_session_id: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub active_task_id: Option<String>,
    #[serde(default)]
    pub open_tasks: Vec<DesktopWorkbenchRouteTask>,
    pub workspace_id: String,
    pub workspace_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DesktopTaskRoutePayload {
    pub route_id: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub session_id: Option<String>,
    pub task_id: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DesktopTaskRouteAckReq {
    pub route_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopDockRecentLocalWorkspace {
    pub label: String,
    pub root_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopSetDockRecentLocalWorkspacesReq {
    pub entries: Vec<DesktopDockRecentLocalWorkspace>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopTitlebarColor {
    #[ts(optional = nullable)]
    pub a: Option<f64>,
    pub b: f64,
    pub g: f64,
    pub r: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopSetWindowTitleReq {
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DesktopNotificationPermission {
    Default,
    Granted,
    Denied,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DesktopNotificationKind {
    TurnCompleted,
    TurnFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopShowSystemNotificationReq {
    pub kind: DesktopNotificationKind,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub body: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub session_id: Option<String>,
    pub task_id: String,
    pub title: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopSyncWorkspaceAttentionReq {
    pub has_unread_error: bool,
    pub unread_primary_task_count: u32,
    pub workspace_id: String,
}
