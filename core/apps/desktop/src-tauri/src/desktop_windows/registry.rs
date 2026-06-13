use std::collections::{HashMap, HashSet};

use ctx_desktop_ipc::{
    DesktopDockRecentLocalWorkspace as DockRecentLocalWorkspaceEntry,
    DesktopRecordWorkbenchRouteReq, DesktopTaskRoutePayload, DesktopWorkbenchRouteTask,
};
use serde::{Deserialize, Serialize};

#[derive(Default)]
pub(crate) struct WorkspaceWindowRegistry {
    by_window: std::sync::Mutex<HashMap<String, Vec<WorkspaceWindowMapping>>>,
    route_sequence: std::sync::Mutex<u64>,
    pending_task_routes: std::sync::Mutex<HashMap<String, DesktopTaskRoutePayload>>,
    recent_workspaces: std::sync::Mutex<Vec<RecentWorkspaceEntry>>,
    dock_recent_local_workspaces: std::sync::Mutex<Vec<DockRecentLocalWorkspaceEntry>>,
}

pub(crate) const MAX_RECENT_WORKSPACES: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RecentWorkspaceEntry {
    pub(crate) workspace_id: String,
    pub(crate) label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceWindowMapping {
    workspace_id: String,
    daemon_key: Option<String>,
    active_task_id: Option<String>,
    active_session_id: Option<String>,
    open_tasks: HashSet<WorkspaceTaskMapping>,
    last_route_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct WorkspaceTaskMapping {
    task_id: String,
    session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceTaskWindowLookup {
    Match {
        window_label: String,
        reason: WorkspaceTaskWindowMatchReason,
    },
    AmbiguousWorkspace {
        window_labels: Vec<String>,
    },
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceTaskWindowMatchReason {
    ExactSession,
    ExactTask,
    Workspace,
}

impl WorkspaceWindowRegistry {
    #[cfg(test)]
    pub(crate) fn register(&self, window_label: &str, workspace_id: &str) {
        self.register_for_daemon(window_label, None, workspace_id);
    }

    pub(crate) fn register_for_daemon(
        &self,
        window_label: &str,
        daemon_key: Option<&str>,
        workspace_id: &str,
    ) {
        let Some(mapping) = workspace_window_mapping(daemon_key, workspace_id) else {
            return;
        };
        let mut map = match self.by_window.lock() {
            Ok(map) => map,
            Err(_) => return,
        };
        let entry = map.entry(window_label.to_string()).or_default();
        let already_registered = entry.iter().any(|existing| {
            existing.workspace_id == mapping.workspace_id
                && existing.daemon_key == mapping.daemon_key
        });
        entry.retain(|existing| {
            existing.workspace_id != mapping.workspace_id
                || existing.daemon_key == mapping.daemon_key
        });
        if !already_registered {
            entry.push(mapping);
        }
    }

    pub(crate) fn unregister_window(&self, window_label: &str) {
        let mut map = match self.by_window.lock() {
            Ok(map) => map,
            Err(_) => return,
        };
        map.remove(window_label);
    }

    #[cfg(test)]
    pub(crate) fn set_window_workspaces(&self, window_label: &str, workspace_ids: Vec<String>) {
        self.set_window_workspaces_for_daemon(window_label, None, workspace_ids);
    }

    pub(crate) fn set_window_workspaces_for_daemon(
        &self,
        window_label: &str,
        daemon_key: Option<&str>,
        workspace_ids: Vec<String>,
    ) {
        let mut map = match self.by_window.lock() {
            Ok(map) => map,
            Err(_) => return,
        };
        let mut set = Vec::new();
        for id in workspace_ids {
            if let Some(mapping) = workspace_window_mapping(daemon_key, &id) {
                if !set.iter().any(|existing: &WorkspaceWindowMapping| {
                    existing.workspace_id == mapping.workspace_id
                        && existing.daemon_key == mapping.daemon_key
                }) {
                    set.push(mapping);
                }
            }
        }
        if set.is_empty() {
            map.remove(window_label);
        } else {
            map.insert(window_label.to_string(), set);
        }
    }

    pub(crate) fn record_workbench_route_for_daemon(
        &self,
        window_label: &str,
        daemon_key: Option<&str>,
        req: DesktopRecordWorkbenchRouteReq,
    ) {
        let Some(mapping) = workbench_route_mapping(daemon_key, req, self.next_route_sequence())
        else {
            return;
        };
        let mut map = match self.by_window.lock() {
            Ok(map) => map,
            Err(_) => return,
        };
        let entry = map.entry(window_label.to_string()).or_default();
        entry.retain(|existing| existing.workspace_id != mapping.workspace_id);
        entry.push(mapping);
    }

    pub(crate) fn window_for_workspace(&self, workspace_id: &str) -> Option<String> {
        self.window_for_workspace_for_daemon(workspace_id, None)
    }

    pub(crate) fn window_for_workspace_for_daemon(
        &self,
        workspace_id: &str,
        daemon_key: Option<&str>,
    ) -> Option<String> {
        let workspace_id = workspace_id.trim();
        if workspace_id.is_empty() {
            return None;
        }
        let daemon_key = normalize_daemon_key(daemon_key);
        let map = self.by_window.lock().ok()?;
        let mut found = None;
        for (label, mappings) in map.iter() {
            let has_match = mappings.iter().any(|mapping| {
                mapping.workspace_id == workspace_id
                    && daemon_key
                        .as_deref()
                        .map(|key| mapping.daemon_key.as_deref() == Some(key))
                        .unwrap_or(true)
            });
            if !has_match {
                continue;
            }
            if found.is_some() {
                return None;
            }
            found = Some(label.clone());
        }
        found
    }

    pub(crate) fn window_for_task_target(
        &self,
        workspace_id: &str,
        task_id: &str,
        session_id: Option<&str>,
        daemon_key: Option<&str>,
        preferred_window_label: Option<&str>,
    ) -> WorkspaceTaskWindowLookup {
        let workspace_id = workspace_id.trim();
        let task_id = task_id.trim();
        if workspace_id.is_empty() || task_id.is_empty() {
            return WorkspaceTaskWindowLookup::None;
        }
        let session_id = normalize_optional_text(session_id);
        let daemon_key = normalize_daemon_key(daemon_key);
        let preferred_window_label = normalize_optional_text(preferred_window_label);
        let map = match self.by_window.lock() {
            Ok(map) => map,
            Err(_) => return WorkspaceTaskWindowLookup::None,
        };
        let mut candidates = Vec::new();
        for (window_label, mappings) in map.iter() {
            for mapping in mappings {
                if mapping.workspace_id != workspace_id {
                    continue;
                }
                if daemon_key
                    .as_deref()
                    .map(|key| mapping.daemon_key.as_deref() != Some(key))
                    .unwrap_or(false)
                {
                    continue;
                }
                candidates.push(WindowTaskCandidate {
                    window_label: window_label.clone(),
                    reason: task_match_reason(mapping, task_id, session_id.as_deref()),
                    last_route_sequence: mapping.last_route_sequence,
                });
            }
        }
        choose_task_window_candidate(candidates, preferred_window_label.as_deref())
    }

    pub(crate) fn set_pending_task_route(
        &self,
        window_label: &str,
        payload: DesktopTaskRoutePayload,
    ) {
        let window_label = window_label.trim();
        if window_label.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.pending_task_routes.lock() {
            guard.insert(window_label.to_string(), payload);
        }
    }

    pub(crate) fn consume_pending_task_route(
        &self,
        window_label: &str,
    ) -> Option<DesktopTaskRoutePayload> {
        let window_label = window_label.trim();
        if window_label.is_empty() {
            return None;
        }
        self.pending_task_routes
            .lock()
            .ok()
            .and_then(|mut guard| guard.remove(window_label))
    }

    pub(crate) fn acknowledge_pending_task_route(&self, window_label: &str, route_id: &str) {
        let window_label = window_label.trim();
        let route_id = route_id.trim();
        if window_label.is_empty() || route_id.is_empty() {
            return;
        }
        let Ok(mut guard) = self.pending_task_routes.lock() else {
            return;
        };
        let matches_route = guard
            .get(window_label)
            .map(|payload| payload.route_id == route_id)
            .unwrap_or(false);
        if matches_route {
            guard.remove(window_label);
        }
    }

    fn next_route_sequence(&self) -> u64 {
        match self.route_sequence.lock() {
            Ok(mut guard) => {
                *guard = guard.saturating_add(1);
                *guard
            }
            Err(_) => 0,
        }
    }

    pub(crate) fn workspace_ids(&self) -> Vec<String> {
        let map = match self.by_window.lock() {
            Ok(map) => map,
            Err(_) => return Vec::new(),
        };
        let mut out = HashSet::new();
        for mappings in map.values() {
            for mapping in mappings {
                out.insert(mapping.workspace_id.clone());
            }
        }
        out.into_iter().collect()
    }

    pub(crate) fn record_recent_workspace(
        &self,
        workspace_id: &str,
        workspace_label: Option<&str>,
    ) {
        let workspace_id = workspace_id.trim();
        if workspace_id.is_empty() {
            return;
        }

        let label = workspace_label
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(workspace_id)
            .to_string();

        let mut recent = match self.recent_workspaces.lock() {
            Ok(recent) => recent,
            Err(_) => return,
        };
        recent.retain(|entry| entry.workspace_id != workspace_id);
        recent.insert(
            0,
            RecentWorkspaceEntry {
                workspace_id: workspace_id.to_string(),
                label,
            },
        );
        if recent.len() > MAX_RECENT_WORKSPACES {
            recent.truncate(MAX_RECENT_WORKSPACES);
        }
    }

    pub(crate) fn recent_workspaces(&self) -> Vec<RecentWorkspaceEntry> {
        match self.recent_workspaces.lock() {
            Ok(recent) => recent.clone(),
            Err(_) => Vec::new(),
        }
    }

    pub(crate) fn set_dock_recent_local_workspaces(
        &self,
        entries: Vec<DockRecentLocalWorkspaceEntry>,
    ) {
        let mut dedup = HashSet::new();
        let mut normalized = Vec::new();
        for entry in entries {
            let root_path = entry.root_path.trim();
            if root_path.is_empty() {
                continue;
            }
            if !dedup.insert(root_path.to_string()) {
                continue;
            }
            let label = entry.label.trim();
            normalized.push(DockRecentLocalWorkspaceEntry {
                label: if label.is_empty() {
                    root_path.to_string()
                } else {
                    label.to_string()
                },
                root_path: root_path.to_string(),
            });
            if normalized.len() >= MAX_RECENT_WORKSPACES {
                break;
            }
        }

        let mut guard = match self.dock_recent_local_workspaces.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        *guard = normalized;
    }

    pub(crate) fn dock_recent_local_workspaces(&self) -> Vec<DockRecentLocalWorkspaceEntry> {
        match self.dock_recent_local_workspaces.lock() {
            Ok(entries) => entries.clone(),
            Err(_) => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowTaskCandidate {
    window_label: String,
    reason: WorkspaceTaskWindowMatchReason,
    last_route_sequence: u64,
}

fn normalize_daemon_key(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalize_workbench_route_task(task: DesktopWorkbenchRouteTask) -> Option<WorkspaceTaskMapping> {
    let task_id = task.task_id.trim();
    if task_id.is_empty() {
        return None;
    }
    Some(WorkspaceTaskMapping {
        task_id: task_id.to_string(),
        session_id: normalize_optional_text(task.session_id.as_deref()),
    })
}

fn workspace_window_mapping(
    daemon_key: Option<&str>,
    workspace_id: &str,
) -> Option<WorkspaceWindowMapping> {
    let workspace_id = workspace_id.trim();
    if workspace_id.is_empty() {
        return None;
    }
    Some(WorkspaceWindowMapping {
        workspace_id: workspace_id.to_string(),
        daemon_key: normalize_daemon_key(daemon_key),
        active_task_id: None,
        active_session_id: None,
        open_tasks: HashSet::new(),
        last_route_sequence: 0,
    })
}

fn workbench_route_mapping(
    daemon_key: Option<&str>,
    req: DesktopRecordWorkbenchRouteReq,
    last_route_sequence: u64,
) -> Option<WorkspaceWindowMapping> {
    let workspace_id = req.workspace_id.trim();
    if workspace_id.is_empty() {
        return None;
    }
    let active_task_id = normalize_optional_text(req.active_task_id.as_deref());
    let active_session_id = if active_task_id.is_some() {
        normalize_optional_text(req.active_session_id.as_deref())
    } else {
        None
    };
    let mut open_tasks = req
        .open_tasks
        .into_iter()
        .filter_map(normalize_workbench_route_task)
        .collect::<HashSet<_>>();
    if let Some(task_id) = active_task_id.as_deref() {
        open_tasks.insert(WorkspaceTaskMapping {
            task_id: task_id.to_string(),
            session_id: active_session_id.clone(),
        });
    }
    Some(WorkspaceWindowMapping {
        workspace_id: workspace_id.to_string(),
        daemon_key: normalize_daemon_key(daemon_key),
        active_task_id,
        active_session_id,
        open_tasks,
        last_route_sequence,
    })
}

fn task_match_reason(
    mapping: &WorkspaceWindowMapping,
    task_id: &str,
    session_id: Option<&str>,
) -> WorkspaceTaskWindowMatchReason {
    if let Some(session_id) = session_id {
        let active_session_matches = mapping.active_task_id.as_deref() == Some(task_id)
            && mapping.active_session_id.as_deref() == Some(session_id);
        let open_session_matches = mapping.open_tasks.contains(&WorkspaceTaskMapping {
            task_id: task_id.to_string(),
            session_id: Some(session_id.to_string()),
        });
        if active_session_matches || open_session_matches {
            return WorkspaceTaskWindowMatchReason::ExactSession;
        }
    }
    if mapping.active_task_id.as_deref() == Some(task_id)
        || mapping
            .open_tasks
            .iter()
            .any(|open| open.task_id == task_id)
    {
        return WorkspaceTaskWindowMatchReason::ExactTask;
    }
    WorkspaceTaskWindowMatchReason::Workspace
}

fn reason_rank(reason: WorkspaceTaskWindowMatchReason) -> u8 {
    match reason {
        WorkspaceTaskWindowMatchReason::ExactSession => 3,
        WorkspaceTaskWindowMatchReason::ExactTask => 2,
        WorkspaceTaskWindowMatchReason::Workspace => 1,
    }
}

fn choose_task_window_candidate(
    mut candidates: Vec<WindowTaskCandidate>,
    preferred_window_label: Option<&str>,
) -> WorkspaceTaskWindowLookup {
    if candidates.is_empty() {
        return WorkspaceTaskWindowLookup::None;
    }
    candidates.sort_by(|left, right| {
        reason_rank(right.reason)
            .cmp(&reason_rank(left.reason))
            .then_with(|| right.last_route_sequence.cmp(&left.last_route_sequence))
            .then_with(|| left.window_label.cmp(&right.window_label))
    });
    let best_reason = candidates[0].reason;
    if best_reason != WorkspaceTaskWindowMatchReason::Workspace {
        if let Some(preferred_window_label) = preferred_window_label {
            if let Some(preferred) = candidates.iter().find(|candidate| {
                candidate.window_label == preferred_window_label && candidate.reason == best_reason
            }) {
                return WorkspaceTaskWindowLookup::Match {
                    window_label: preferred.window_label.clone(),
                    reason: preferred.reason,
                };
            }
        }
        return WorkspaceTaskWindowLookup::Match {
            window_label: candidates[0].window_label.clone(),
            reason: best_reason,
        };
    }
    let mut labels = candidates
        .into_iter()
        .filter(|candidate| candidate.reason == WorkspaceTaskWindowMatchReason::Workspace)
        .map(|candidate| candidate.window_label)
        .collect::<Vec<_>>();
    labels.sort();
    labels.dedup();
    if labels.len() == 1 {
        WorkspaceTaskWindowLookup::Match {
            window_label: labels.remove(0),
            reason: WorkspaceTaskWindowMatchReason::Workspace,
        }
    } else {
        WorkspaceTaskWindowLookup::AmbiguousWorkspace {
            window_labels: labels,
        }
    }
}

#[cfg(test)]
mod tests;
