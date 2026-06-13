use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tauri::menu::{Menu, MenuItemKind, Submenu};
use tauri::Manager;

use super::{ids::is_menu_command_id, DesktopMenuItemStateUpdate, DesktopSetMenuStateReq};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopMenuItemStateSnapshot {
    pub id: String,
    pub enabled: bool,
    pub checked: Option<bool>,
}

#[derive(Default)]
pub(crate) struct DesktopMenuStateCache {
    by_window: Mutex<HashMap<String, Vec<DesktopMenuItemStateUpdate>>>,
    focused_window_label: Mutex<Option<String>>,
}

impl DesktopMenuStateCache {
    fn set_state(&self, window_label: &str, items: Vec<DesktopMenuItemStateUpdate>) {
        let mut guard = self
            .by_window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.insert(window_label.to_string(), items);
    }

    fn get_state(&self, window_label: &str) -> Option<Vec<DesktopMenuItemStateUpdate>> {
        let guard = self
            .by_window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.get(window_label).cloned()
    }

    fn remove_state(&self, window_label: &str) {
        let mut guard = self
            .by_window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.remove(window_label);
    }

    fn set_focused_window(&self, window_label: &str) {
        let mut guard = self
            .focused_window_label
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(window_label.to_string());
    }

    fn clear_focused_window_if_matches(&self, window_label: &str) {
        let mut guard = self
            .focused_window_label
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.as_deref() == Some(window_label) {
            *guard = None;
        }
    }

    pub(crate) fn get_focused_window(&self) -> Option<String> {
        let guard = self
            .focused_window_label
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clone()
    }
}

fn set_enabled_for_kind(kind: &MenuItemKind<tauri::Wry>, enabled: bool) -> Result<(), String> {
    match kind {
        MenuItemKind::MenuItem(item) => item.set_enabled(enabled).map_err(|e| e.to_string()),
        MenuItemKind::Submenu(item) => item.set_enabled(enabled).map_err(|e| e.to_string()),
        MenuItemKind::Check(item) => item.set_enabled(enabled).map_err(|e| e.to_string()),
        MenuItemKind::Icon(item) => item.set_enabled(enabled).map_err(|e| e.to_string()),
        MenuItemKind::Predefined(_) => Ok(()),
    }
}

fn set_text_for_kind(kind: &MenuItemKind<tauri::Wry>, text: &str) -> Result<(), String> {
    match kind {
        MenuItemKind::MenuItem(item) => item.set_text(text).map_err(|e| e.to_string()),
        MenuItemKind::Submenu(item) => item.set_text(text).map_err(|e| e.to_string()),
        MenuItemKind::Check(item) => item.set_text(text).map_err(|e| e.to_string()),
        MenuItemKind::Icon(item) => item.set_text(text).map_err(|e| e.to_string()),
        MenuItemKind::Predefined(_) => Ok(()),
    }
}

fn apply_state_to_kind(
    kind: &MenuItemKind<tauri::Wry>,
    update: &DesktopMenuItemStateUpdate,
) -> Result<(), String> {
    if let Some(text) = update.text.as_deref() {
        set_text_for_kind(kind, text)?;
    }
    if let Some(enabled) = update.enabled {
        set_enabled_for_kind(kind, enabled)?;
    }
    if let Some(checked) = update.checked {
        if let Some(item) = kind.as_check_menuitem() {
            item.set_checked(checked).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn apply_state_to_submenu(
    submenu: &Submenu<tauri::Wry>,
    update: &DesktopMenuItemStateUpdate,
) -> Result<bool, String> {
    for item in submenu.items().map_err(|e| e.to_string())? {
        if item.id() == &update.id.as_str() {
            apply_state_to_kind(&item, update)?;
            return Ok(true);
        }
        if let Some(child_submenu) = item.as_submenu() {
            if apply_state_to_submenu(child_submenu, update)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn apply_state_to_menu(
    menu: &Menu<tauri::Wry>,
    update: &DesktopMenuItemStateUpdate,
) -> Result<bool, String> {
    for item in menu.items().map_err(|e| e.to_string())? {
        if item.id() == &update.id.as_str() {
            apply_state_to_kind(&item, update)?;
            return Ok(true);
        }
        if let Some(submenu) = item.as_submenu() {
            if apply_state_to_submenu(submenu, update)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn apply_menu_state_updates(
    app: &tauri::AppHandle,
    items: &[DesktopMenuItemStateUpdate],
) -> Result<(), String> {
    let Some(menu) = app.menu() else {
        return Ok(());
    };
    for update in items {
        let found = apply_state_to_menu(&menu, update)?;
        if !found {
            eprintln!(
                "desktop menu state update ignored: unknown menu id '{}'",
                update.id
            );
        }
    }
    Ok(())
}

#[cfg(feature = "automation")]
fn read_enabled_for_kind(kind: &MenuItemKind<tauri::Wry>) -> Result<bool, String> {
    match kind {
        MenuItemKind::MenuItem(item) => item.is_enabled().map_err(|e| e.to_string()),
        MenuItemKind::Submenu(item) => item.is_enabled().map_err(|e| e.to_string()),
        MenuItemKind::Check(item) => item.is_enabled().map_err(|e| e.to_string()),
        MenuItemKind::Icon(item) => item.is_enabled().map_err(|e| e.to_string()),
        MenuItemKind::Predefined(_) => Ok(true),
    }
}

#[cfg(feature = "automation")]
fn read_checked_for_kind(kind: &MenuItemKind<tauri::Wry>) -> Result<Option<bool>, String> {
    if let Some(item) = kind.as_check_menuitem() {
        return item.is_checked().map(Some).map_err(|e| e.to_string());
    }
    Ok(None)
}

#[cfg(feature = "automation")]
fn read_state_from_submenu(
    submenu: &Submenu<tauri::Wry>,
    id: &str,
) -> Result<Option<DesktopMenuItemStateSnapshot>, String> {
    for item in submenu.items().map_err(|e| e.to_string())? {
        if item.id() == &id {
            return Ok(Some(DesktopMenuItemStateSnapshot {
                id: id.to_string(),
                enabled: read_enabled_for_kind(&item)?,
                checked: read_checked_for_kind(&item)?,
            }));
        }
        if let Some(child_submenu) = item.as_submenu() {
            if let Some(state) = read_state_from_submenu(child_submenu, id)? {
                return Ok(Some(state));
            }
        }
    }
    Ok(None)
}

#[cfg(feature = "automation")]
fn read_state_from_menu(
    menu: &Menu<tauri::Wry>,
    id: &str,
) -> Result<Option<DesktopMenuItemStateSnapshot>, String> {
    for item in menu.items().map_err(|e| e.to_string())? {
        if item.id() == &id {
            return Ok(Some(DesktopMenuItemStateSnapshot {
                id: id.to_string(),
                enabled: read_enabled_for_kind(&item)?,
                checked: read_checked_for_kind(&item)?,
            }));
        }
        if let Some(submenu) = item.as_submenu() {
            if let Some(state) = read_state_from_submenu(submenu, id)? {
                return Ok(Some(state));
            }
        }
    }
    Ok(None)
}

pub(crate) fn apply_cached_menu_state_for_window(
    app: &tauri::AppHandle,
    window_label: &str,
) -> Result<(), String> {
    let cache = app.state::<DesktopMenuStateCache>();
    let Some(items) = cache.get_state(window_label) else {
        return Ok(());
    };
    apply_menu_state_updates(app, &items)
}

pub(crate) fn clear_cached_menu_state_for_window(app: &tauri::AppHandle, window_label: &str) {
    let cache = app.state::<DesktopMenuStateCache>();
    cache.remove_state(window_label);
    cache.clear_focused_window_if_matches(window_label);
}

pub(crate) fn mark_menu_state_window_focused(app: &tauri::AppHandle, window_label: &str) {
    let cache = app.state::<DesktopMenuStateCache>();
    cache.set_focused_window(window_label);
}

#[tauri::command]
pub(crate) fn desktop_set_menu_state(
    app: tauri::AppHandle,
    webview_window: tauri::WebviewWindow,
    cache: tauri::State<'_, DesktopMenuStateCache>,
    req: DesktopSetMenuStateReq,
) -> Result<(), String> {
    let items = req.items;
    let window_label = webview_window.label().to_string();
    cache.set_state(&window_label, items.clone());

    if webview_window.is_focused().map_err(|err| err.to_string())? {
        cache.set_focused_window(&window_label);
        apply_menu_state_updates(&app, &items)?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn desktop_get_menu_item_state(
    app: tauri::AppHandle,
    command_id: String,
) -> Result<DesktopMenuItemStateSnapshot, String> {
    #[cfg(feature = "automation")]
    {
        let command_id = command_id.trim().to_string();
        if command_id.is_empty() {
            return Err("command_id is required".to_string());
        }
        if !is_menu_command_id(&command_id) {
            return Err(format!("unknown menu command id: {command_id}"));
        }
        let Some(menu) = app.menu() else {
            return Err("desktop menu not available".to_string());
        };
        let Some(state) = read_state_from_menu(&menu, &command_id)? else {
            return Err(format!("menu item not found: {command_id}"));
        };
        Ok(state)
    }

    #[cfg(not(feature = "automation"))]
    {
        let _ = app;
        let _ = command_id;
        Err("desktop_get_menu_item_state is automation-only".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_menu_state_cache_is_window_scoped() {
        let cache = DesktopMenuStateCache::default();
        cache.set_state(
            "window-a",
            vec![DesktopMenuItemStateUpdate {
                id: "task.new".to_string(),
                enabled: Some(true),
                checked: Some(false),
                text: None,
            }],
        );
        cache.set_state(
            "window-b",
            vec![DesktopMenuItemStateUpdate {
                id: "task.new".to_string(),
                enabled: Some(false),
                checked: Some(false),
                text: None,
            }],
        );

        let a = cache
            .get_state("window-a")
            .expect("expected state for window-a");
        let b = cache
            .get_state("window-b")
            .expect("expected state for window-b");

        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].enabled, Some(true));
        assert_eq!(b[0].enabled, Some(false));

        cache.remove_state("window-a");
        assert!(cache.get_state("window-a").is_none());
        assert!(cache.get_state("window-b").is_some());
    }

    #[test]
    fn desktop_menu_state_cache_tracks_focused_window() {
        let cache = DesktopMenuStateCache::default();
        assert_eq!(cache.get_focused_window(), None);

        cache.set_focused_window("window-a");
        assert_eq!(cache.get_focused_window().as_deref(), Some("window-a"));

        cache.clear_focused_window_if_matches("window-b");
        assert_eq!(cache.get_focused_window().as_deref(), Some("window-a"));

        cache.clear_focused_window_if_matches("window-a");
        assert_eq!(cache.get_focused_window(), None);
    }
}
