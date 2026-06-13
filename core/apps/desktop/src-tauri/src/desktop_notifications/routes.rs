use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use ctx_desktop_ipc::DesktopShowSystemNotificationReq;

const MAX_NOTIFICATION_ROUTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopNotificationTaskRoute {
    pub(crate) daemon_key: String,
    pub(crate) route_id: String,
    pub(crate) session_id: Option<String>,
    pub(crate) source_window_label: String,
    pub(crate) task_id: String,
    pub(crate) workspace_id: String,
}

#[derive(Debug, Default)]
pub(crate) struct DesktopNotificationRouteRegistry {
    inner: Mutex<DesktopNotificationRouteState>,
}

#[derive(Debug, Default)]
struct DesktopNotificationRouteState {
    order: VecDeque<String>,
    routes: HashMap<String, DesktopNotificationTaskRoute>,
}

impl DesktopNotificationRouteRegistry {
    pub(crate) fn create_task_route(
        &self,
        req: &DesktopShowSystemNotificationReq,
        source_window_label: &str,
        daemon_key: &str,
    ) -> anyhow::Result<DesktopNotificationTaskRoute> {
        let workspace_id = req.workspace_id.trim();
        if workspace_id.is_empty() {
            anyhow::bail!("workspace_id is required");
        }
        let task_id = req.task_id.trim();
        if task_id.is_empty() {
            anyhow::bail!("task_id is required");
        }
        let source_window_label = source_window_label.trim();
        if source_window_label.is_empty() {
            anyhow::bail!("source window label is required");
        }
        let daemon_key = daemon_key.trim();
        if daemon_key.is_empty() {
            anyhow::bail!("daemon target scope is required for desktop notifications");
        }
        let route = DesktopNotificationTaskRoute {
            daemon_key: daemon_key.to_string(),
            route_id: uuid::Uuid::new_v4().to_string(),
            session_id: req
                .session_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            source_window_label: source_window_label.to_string(),
            task_id: task_id.to_string(),
            workspace_id: workspace_id.to_string(),
        };
        self.insert(route.clone());
        Ok(route)
    }

    pub(crate) fn get(&self, route_id: &str) -> Option<DesktopNotificationTaskRoute> {
        let route_id = route_id.trim();
        if route_id.is_empty() {
            return None;
        }
        self.inner
            .lock()
            .ok()
            .and_then(|guard| guard.routes.get(route_id).cloned())
    }

    fn insert(&self, route: DesktopNotificationTaskRoute) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        guard.order.push_back(route.route_id.clone());
        guard.routes.insert(route.route_id.clone(), route);
        while guard.order.len() > MAX_NOTIFICATION_ROUTES {
            if let Some(oldest) = guard.order.pop_front() {
                guard.routes.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctx_desktop_ipc::DesktopNotificationKind;

    fn notification_req() -> DesktopShowSystemNotificationReq {
        DesktopShowSystemNotificationReq {
            kind: DesktopNotificationKind::TurnCompleted,
            body: Some("Done".to_string()),
            session_id: Some("session-1".to_string()),
            task_id: " task-1 ".to_string(),
            title: "Turn completed".to_string(),
            workspace_id: " workspace-1 ".to_string(),
        }
    }

    #[test]
    fn create_task_route_trims_and_stores_scope() {
        let registry = DesktopNotificationRouteRegistry::default();
        let route = registry
            .create_task_route(&notification_req(), " workbench:1 ", " ssh|host||8787| ")
            .expect("route");

        assert_eq!(route.workspace_id, "workspace-1");
        assert_eq!(route.task_id, "task-1");
        assert_eq!(route.session_id.as_deref(), Some("session-1"));
        assert_eq!(route.source_window_label, "workbench:1");
        assert_eq!(route.daemon_key, "ssh|host||8787|");
        assert_eq!(registry.get(&route.route_id), Some(route));
    }

    #[test]
    fn create_task_route_requires_daemon_scope() {
        let registry = DesktopNotificationRouteRegistry::default();
        assert!(registry
            .create_task_route(&notification_req(), "workbench:1", " ")
            .is_err());
    }
}
