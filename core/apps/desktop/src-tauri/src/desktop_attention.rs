use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use anyhow::Result;
use ctx_desktop_ipc::DesktopSyncWorkspaceAttentionReq;
use serde::Serialize;
#[cfg(target_os = "windows")]
use tauri::image::Image;
use tauri::Manager;

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowAttentionSample {
    has_unread_error: bool,
    seq: u64,
    unread_primary_task_count: u32,
    workspace_id: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DesktopAttentionAggregate {
    has_unread_error: bool,
    unread_primary_task_count: u32,
    workspace_count: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DesktopAttentionAutomationSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    badge_count: Option<i64>,
    has_unread_error: bool,
    overlay_visible: bool,
    unread_primary_task_count: u32,
    workspace_count: u32,
}

#[derive(Debug)]
pub(super) struct DesktopAttentionRegistry {
    last_applied: Mutex<DesktopAttentionAutomationSnapshot>,
    last_host_window_label: Mutex<Option<String>>,
    next_seq: AtomicU64,
    samples_by_window: Mutex<HashMap<String, WindowAttentionSample>>,
}

impl Default for DesktopAttentionRegistry {
    fn default() -> Self {
        Self {
            last_applied: Mutex::new(DesktopAttentionAutomationSnapshot::default()),
            last_host_window_label: Mutex::new(None),
            next_seq: AtomicU64::new(1),
            samples_by_window: Mutex::new(HashMap::new()),
        }
    }
}

impl DesktopAttentionRegistry {
    fn next_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::Relaxed)
    }

    fn update_window_attention(&self, window_label: &str, req: DesktopSyncWorkspaceAttentionReq) {
        let window_label = window_label.trim();
        if window_label.is_empty() {
            return;
        }
        let workspace_id = req.workspace_id.trim();
        if workspace_id.is_empty() {
            return;
        }
        let mut guard = match self.samples_by_window.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        guard.insert(
            window_label.to_string(),
            WindowAttentionSample {
                has_unread_error: req.has_unread_error,
                seq: self.next_seq(),
                unread_primary_task_count: req.unread_primary_task_count,
                workspace_id: workspace_id.to_string(),
            },
        );
    }

    pub(super) fn clear_window_attention(&self, window_label: &str) {
        let window_label = window_label.trim();
        if window_label.is_empty() {
            return;
        }
        let mut guard = match self.samples_by_window.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        guard.remove(window_label);
    }

    fn preferred_window_label(&self) -> Option<String> {
        let guard = match self.samples_by_window.lock() {
            Ok(guard) => guard,
            Err(_) => return None,
        };
        preferred_attention_window_label(guard.keys().map(String::as_str))
    }

    fn aggregate(&self) -> DesktopAttentionAggregate {
        let guard = match self.samples_by_window.lock() {
            Ok(guard) => guard,
            Err(_) => return DesktopAttentionAggregate::default(),
        };
        let mut freshest_by_workspace: HashMap<&str, &WindowAttentionSample> = HashMap::new();
        for sample in guard.values() {
            match freshest_by_workspace.get(sample.workspace_id.as_str()) {
                Some(existing) if existing.seq >= sample.seq => {}
                _ => {
                    freshest_by_workspace.insert(sample.workspace_id.as_str(), sample);
                }
            }
        }

        let mut unread_primary_task_count = 0u32;
        let mut has_unread_error = false;
        for sample in freshest_by_workspace.values() {
            unread_primary_task_count =
                unread_primary_task_count.saturating_add(sample.unread_primary_task_count);
            has_unread_error |= sample.has_unread_error;
        }
        DesktopAttentionAggregate {
            has_unread_error,
            unread_primary_task_count,
            workspace_count: freshest_by_workspace
                .len()
                .try_into()
                .ok()
                .unwrap_or(u32::MAX),
        }
    }

    fn set_last_applied(&self, snapshot: DesktopAttentionAutomationSnapshot) {
        if let Ok(mut guard) = self.last_applied.lock() {
            *guard = snapshot;
        }
    }

    fn last_host_window_label(&self) -> Option<String> {
        match self.last_host_window_label.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => None,
        }
    }

    fn set_last_host_window_label(&self, window_label: Option<String>) {
        if let Ok(mut guard) = self.last_host_window_label.lock() {
            *guard = window_label;
        }
    }

    #[cfg(feature = "automation")]
    pub(super) fn automation_snapshot(&self) -> DesktopAttentionAutomationSnapshot {
        match self.last_applied.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => DesktopAttentionAutomationSnapshot::default(),
        }
    }

    pub(super) fn apply_to_app(&self, app: &tauri::AppHandle) -> Result<()> {
        let aggregate = self.aggregate();
        let snapshot = automation_snapshot_for_aggregate(aggregate);
        if aggregate.unread_primary_task_count == 0 {
            return self.clear_applied_attention(app, snapshot);
        }
        let Some(window_label) = self.preferred_window_label() else {
            return self.clear_applied_attention(app, snapshot);
        };
        let webview_windows = app.webview_windows();
        let Some(window) = webview_windows.get(&window_label).cloned() else {
            return self.clear_applied_attention(app, snapshot);
        };
        let previous_host_window_label = self.last_host_window_label();
        if should_clear_previous_host(previous_host_window_label.as_deref(), window_label.as_str())
        {
            if let Some(previous_host_window_label) = previous_host_window_label {
                if let Some(previous_window) = webview_windows.get(&previous_host_window_label) {
                    clear_attention_on_window(previous_window)?;
                }
            }
        }
        apply_attention_to_window(&window, aggregate)?;
        self.set_last_host_window_label(Some(window_label));
        self.set_last_applied(snapshot);
        Ok(())
    }

    fn clear_applied_attention(
        &self,
        app: &tauri::AppHandle,
        snapshot: DesktopAttentionAutomationSnapshot,
    ) -> Result<()> {
        let webview_windows = app.webview_windows();
        let window_label = preferred_clear_window_label(
            self.last_host_window_label().as_deref(),
            webview_windows.keys().map(String::as_str),
        );
        if let Some(window_label) = window_label {
            if let Some(window) = webview_windows.get(&window_label).cloned() {
                clear_attention_on_window(&window)?;
            }
        }
        self.set_last_host_window_label(None);
        self.set_last_applied(snapshot);
        Ok(())
    }
}

fn automation_snapshot_for_aggregate(
    aggregate: DesktopAttentionAggregate,
) -> DesktopAttentionAutomationSnapshot {
    #[cfg(target_os = "windows")]
    let badge_count = None;
    #[cfg(not(target_os = "windows"))]
    let badge_count = if aggregate.unread_primary_task_count > 0 {
        Some(i64::from(aggregate.unread_primary_task_count))
    } else {
        None
    };

    #[cfg(target_os = "windows")]
    let overlay_visible = aggregate.unread_primary_task_count > 0;
    #[cfg(not(target_os = "windows"))]
    let overlay_visible = false;

    DesktopAttentionAutomationSnapshot {
        badge_count,
        has_unread_error: aggregate.has_unread_error,
        overlay_visible,
        unread_primary_task_count: aggregate.unread_primary_task_count,
        workspace_count: aggregate.workspace_count,
    }
}

fn preferred_attention_window_label<'a>(
    labels: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let mut ordered: Vec<String> = labels
        .into_iter()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if ordered.is_empty() {
        return None;
    }
    if ordered.iter().any(|label| label == "main") {
        return Some("main".to_string());
    }
    ordered.sort();
    ordered.into_iter().next()
}

fn preferred_clear_window_label<'a>(
    last_host_window_label: Option<&str>,
    labels: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let ordered: Vec<String> = labels
        .into_iter()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let Some(last_host_window_label) = last_host_window_label.map(str::trim) else {
        return preferred_attention_window_label(ordered.iter().map(String::as_str));
    };
    if last_host_window_label.is_empty() {
        return preferred_attention_window_label(ordered.iter().map(String::as_str));
    }
    if ordered.iter().any(|label| label == last_host_window_label) {
        return Some(last_host_window_label.to_string());
    }
    preferred_attention_window_label(ordered.iter().map(String::as_str))
}

fn should_clear_previous_host(
    last_host_window_label: Option<&str>,
    next_window_label: &str,
) -> bool {
    let Some(last_host_window_label) = last_host_window_label.map(str::trim) else {
        return false;
    };
    !last_host_window_label.is_empty() && last_host_window_label != next_window_label.trim()
}

fn apply_attention_to_window(
    window: &tauri::WebviewWindow,
    aggregate: DesktopAttentionAggregate,
) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let overlay = if aggregate.unread_primary_task_count > 0 {
            Some(Image::from_bytes(include_bytes!("../icons/icon.png"))?)
        } else {
            None
        };
        window.set_overlay_icon(overlay)?;
        return Ok(());
    }

    #[cfg(not(target_os = "windows"))]
    {
        let badge = if aggregate.unread_primary_task_count > 0 {
            Some(i64::from(aggregate.unread_primary_task_count))
        } else {
            None
        };
        window.set_badge_count(badge)?;
        Ok(())
    }
}

fn clear_attention_on_window(window: &tauri::WebviewWindow) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        window.set_overlay_icon(None)?;
        return Ok(());
    }

    #[cfg(not(target_os = "windows"))]
    {
        window.set_badge_count(None)?;
        Ok(())
    }
}

#[tauri::command]
pub(super) fn desktop_sync_workspace_attention(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    registry: tauri::State<DesktopAttentionRegistry>,
    req: DesktopSyncWorkspaceAttentionReq,
) -> Result<(), String> {
    registry.update_window_attention(window.label(), req);
    registry.apply_to_app(&app).map_err(super::to_err)
}

#[tauri::command]
pub(super) fn desktop_clear_window_attention(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    registry: tauri::State<DesktopAttentionRegistry>,
) -> Result<(), String> {
    registry.clear_window_attention(window.label());
    registry.apply_to_app(&app).map_err(super::to_err)
}

#[tauri::command]
pub(super) fn desktop_get_attention_automation_snapshot(
    registry: tauri::State<DesktopAttentionRegistry>,
) -> Result<DesktopAttentionAutomationSnapshot, String> {
    #[cfg(feature = "automation")]
    {
        return Ok(registry.automation_snapshot());
    }

    #[cfg(not(feature = "automation"))]
    {
        let _ = registry;
        Err("desktop_get_attention_automation_snapshot is automation-only".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_freshest_sample_per_workspace() {
        let registry = DesktopAttentionRegistry::default();
        registry.update_window_attention(
            "window-a",
            DesktopSyncWorkspaceAttentionReq {
                workspace_id: "ws-1".to_string(),
                unread_primary_task_count: 2,
                has_unread_error: false,
            },
        );
        registry.update_window_attention(
            "window-b",
            DesktopSyncWorkspaceAttentionReq {
                workspace_id: "ws-1".to_string(),
                unread_primary_task_count: 4,
                has_unread_error: true,
            },
        );
        registry.update_window_attention(
            "window-c",
            DesktopSyncWorkspaceAttentionReq {
                workspace_id: "ws-2".to_string(),
                unread_primary_task_count: 1,
                has_unread_error: false,
            },
        );

        assert_eq!(
            registry.aggregate(),
            DesktopAttentionAggregate {
                unread_primary_task_count: 5,
                has_unread_error: true,
                workspace_count: 2,
            }
        );
    }

    #[test]
    fn clearing_window_attention_removes_contribution() {
        let registry = DesktopAttentionRegistry::default();
        registry.update_window_attention(
            "window-a",
            DesktopSyncWorkspaceAttentionReq {
                workspace_id: "ws-1".to_string(),
                unread_primary_task_count: 2,
                has_unread_error: false,
            },
        );
        registry.clear_window_attention("window-a");

        assert_eq!(registry.aggregate(), DesktopAttentionAggregate::default());
    }

    #[test]
    fn preferred_attention_window_label_prefers_main() {
        assert_eq!(
            preferred_attention_window_label(["workspace-b", "main", "workspace-a"]),
            Some("main".to_string())
        );
    }

    #[test]
    fn preferred_attention_window_label_falls_back_to_sorted_label() {
        assert_eq!(
            preferred_attention_window_label(["workspace-z", "workspace-a", "workspace-m"]),
            Some("workspace-a".to_string())
        );
    }

    #[test]
    fn preferred_clear_window_label_prefers_last_host_if_present() {
        assert_eq!(
            preferred_clear_window_label(
                Some("workspace-z"),
                ["main", "workspace-z", "workspace-a"]
            ),
            Some("workspace-z".to_string())
        );
    }

    #[test]
    fn preferred_clear_window_label_falls_back_when_last_host_is_missing() {
        assert_eq!(
            preferred_clear_window_label(Some("workspace-z"), ["main", "workspace-a"]),
            Some("main".to_string())
        );
    }

    #[test]
    fn should_clear_previous_host_only_when_window_changes() {
        assert!(!should_clear_previous_host(None, "workspace-a"));
        assert!(!should_clear_previous_host(
            Some("workspace-a"),
            "workspace-a"
        ));
        assert!(should_clear_previous_host(
            Some("workspace-a"),
            "workspace-b"
        ));
    }

    #[test]
    fn registry_prefers_attention_host_from_sampled_windows_only() {
        let registry = DesktopAttentionRegistry::default();
        registry.update_window_attention(
            "workspace-b",
            DesktopSyncWorkspaceAttentionReq {
                workspace_id: "ws-1".to_string(),
                unread_primary_task_count: 1,
                has_unread_error: false,
            },
        );
        registry.update_window_attention(
            "workspace-a",
            DesktopSyncWorkspaceAttentionReq {
                workspace_id: "ws-2".to_string(),
                unread_primary_task_count: 2,
                has_unread_error: true,
            },
        );

        assert_eq!(
            registry.preferred_window_label(),
            Some("workspace-a".to_string())
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn automation_snapshot_uses_badge_count_on_non_windows() {
        let snapshot = automation_snapshot_for_aggregate(DesktopAttentionAggregate {
            unread_primary_task_count: 3,
            has_unread_error: true,
            workspace_count: 2,
        });

        assert_eq!(
            snapshot,
            DesktopAttentionAutomationSnapshot {
                badge_count: Some(3),
                has_unread_error: true,
                overlay_visible: false,
                unread_primary_task_count: 3,
                workspace_count: 2,
            }
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn automation_snapshot_uses_overlay_on_windows() {
        let snapshot = automation_snapshot_for_aggregate(DesktopAttentionAggregate {
            unread_primary_task_count: 2,
            has_unread_error: false,
            workspace_count: 1,
        });

        assert_eq!(
            snapshot,
            DesktopAttentionAutomationSnapshot {
                badge_count: None,
                has_unread_error: false,
                overlay_visible: true,
                unread_primary_task_count: 2,
                workspace_count: 1,
            }
        );
    }
}
