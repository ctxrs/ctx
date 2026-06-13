use std::sync::Mutex;

use ctx_desktop_ipc::{DesktopNotificationKind, DesktopShowSystemNotificationReq};
use serde::{Deserialize, Serialize};

pub(crate) const NOTIFICATION_IDENTIFIER_PREFIX: &str = "ctx-task-notification-";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopNotificationAutomationSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deep_link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<DesktopNotificationKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    shown_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopDeliveredNotificationSnapshot {
    pub(crate) delivered: Vec<DesktopDeliveredNotificationEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopDeliveredNotificationEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) deep_link: Option<String>,
    pub(crate) identifier: String,
    pub(crate) title: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopClearDeliveredNotificationsReq {
    pub(crate) identifiers: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct DesktopNotificationAutomationState {
    snapshot: Mutex<DesktopNotificationAutomationSnapshot>,
}

impl DesktopNotificationAutomationState {
    #[cfg(any(feature = "automation", test))]
    pub(crate) fn clear(&self) {
        if let Ok(mut guard) = self.snapshot.lock() {
            *guard = DesktopNotificationAutomationSnapshot::default();
        }
    }

    pub(crate) fn record(&self, req: &DesktopShowSystemNotificationReq, deep_link: &str) {
        if let Ok(mut guard) = self.snapshot.lock() {
            let next_count = guard.shown_count.saturating_add(1);
            *guard = DesktopNotificationAutomationSnapshot {
                body: req.body.clone(),
                deep_link: Some(deep_link.to_string()),
                kind: Some(req.kind),
                session_id: req.session_id.clone(),
                shown_count: next_count,
                task_id: Some(req.task_id.clone()),
                title: Some(req.title.clone()),
                workspace_id: Some(req.workspace_id.clone()),
            };
        }
    }

    #[cfg(any(feature = "automation", test))]
    pub(crate) fn snapshot(&self) -> DesktopNotificationAutomationSnapshot {
        match self.snapshot.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => DesktopNotificationAutomationSnapshot::default(),
        }
    }

    #[cfg(feature = "automation")]
    pub(crate) fn last_deep_link(&self) -> Option<String> {
        match self.snapshot.lock() {
            Ok(guard) => guard.deep_link.clone(),
            Err(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivered_notification_snapshot_serializes_camel_case() {
        let snapshot = DesktopDeliveredNotificationSnapshot {
            delivered: vec![DesktopDeliveredNotificationEntry {
                body: Some("Body".to_string()),
                deep_link: Some("ctx://task?v=1&workspaceId=workspace-1&taskId=task-1".to_string()),
                identifier: "ctx-task-notification-1".to_string(),
                title: "Title".to_string(),
            }],
        };

        let value = serde_json::to_value(snapshot).expect("snapshot json");
        assert_eq!(
            value,
            serde_json::json!({
                "delivered": [{
                    "body": "Body",
                    "deepLink": "ctx://task?v=1&workspaceId=workspace-1&taskId=task-1",
                    "identifier": "ctx-task-notification-1",
                    "title": "Title",
                }],
            })
        );
    }

    #[test]
    fn automation_state_records_latest_notification() {
        let state = DesktopNotificationAutomationState::default();
        state.record(
            &DesktopShowSystemNotificationReq {
                kind: DesktopNotificationKind::TurnCompleted,
                body: Some("Done".to_string()),
                session_id: Some("session-1".to_string()),
                task_id: "task-1".to_string(),
                title: "Turn completed".to_string(),
                workspace_id: "workspace-1".to_string(),
            },
            "ctx://task?v=1&workspaceId=workspace-1&taskId=task-1&sessionId=session-1",
        );
        state.record(
            &DesktopShowSystemNotificationReq {
                kind: DesktopNotificationKind::TurnFailed,
                body: Some("Failed".to_string()),
                session_id: None,
                task_id: "task-2".to_string(),
                title: "Turn failed".to_string(),
                workspace_id: "workspace-2".to_string(),
            },
            "ctx://task?v=1&workspaceId=workspace-2&taskId=task-2",
        );

        assert_eq!(
            state.snapshot(),
            DesktopNotificationAutomationSnapshot {
                body: Some("Failed".to_string()),
                deep_link: Some("ctx://task?v=1&workspaceId=workspace-2&taskId=task-2".to_string()),
                kind: Some(DesktopNotificationKind::TurnFailed),
                session_id: None,
                shown_count: 2,
                task_id: Some("task-2".to_string()),
                title: Some("Turn failed".to_string()),
                workspace_id: Some("workspace-2".to_string()),
            }
        );
    }

    #[test]
    fn automation_state_clear_resets_snapshot() {
        let state = DesktopNotificationAutomationState::default();
        state.record(
            &DesktopShowSystemNotificationReq {
                kind: DesktopNotificationKind::TurnCompleted,
                body: None,
                session_id: None,
                task_id: "task-1".to_string(),
                title: "Turn completed".to_string(),
                workspace_id: "workspace-1".to_string(),
            },
            "ctx://task?v=1&workspaceId=workspace-1&taskId=task-1",
        );

        state.clear();

        assert_eq!(
            state.snapshot(),
            DesktopNotificationAutomationSnapshot::default()
        );
    }
}
