use super::*;

#[cfg(target_os = "macos")]
use objc2::ffi::class_addMethod;
#[cfg(target_os = "macos")]
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
#[cfg(target_os = "macos")]
use objc2::MainThreadOnly;
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
#[cfg(target_os = "macos")]
use objc2_foundation::NSString;
#[cfg(any(target_os = "macos", test))]
use std::collections::HashSet;
#[cfg(target_os = "macos")]
use std::sync::{Once, OnceLock};

const DOCK_OPEN_RECENT_SUBMENU_TITLE: &str = "Open Recent";

#[cfg(target_os = "macos")]
static DOCK_MENU_INSTALL_ONCE: Once = Once::new();
#[cfg(target_os = "macos")]
static DOCK_MENU_APP: OnceLock<tauri::AppHandle> = OnceLock::new();

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DockRecentWorkspaceTarget {
    WorkspaceId { workspace_id: String },
    LocalRootPath { root_path: String },
}

#[cfg(target_os = "macos")]
extern "C" fn application_dock_menu(
    this: &AnyObject,
    _cmd: Sel,
    _sender: &AnyObject,
) -> *mut AnyObject {
    let Some(app) = DOCK_MENU_APP.get() else {
        return std::ptr::null_mut();
    };
    let Some(menu) = build_dock_menu(app, this) else {
        return std::ptr::null_mut();
    };
    Retained::autorelease_return(menu).cast::<AnyObject>()
}

#[cfg(target_os = "macos")]
extern "C" fn dock_open_new_window(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
    let Some(app) = DOCK_MENU_APP.get() else {
        return;
    };
    if let Err(err) = open_launcher_window(app) {
        eprintln!("dock menu new window failed: {err:#}");
    }
}

#[cfg(target_os = "macos")]
extern "C" fn dock_open_recent_workspace(_this: &AnyObject, _cmd: Sel, sender: *mut AnyObject) {
    let Some(app) = DOCK_MENU_APP.get() else {
        return;
    };
    let Some(target) = dock_target_from_sender(sender) else {
        return;
    };
    match target {
        DockRecentWorkspaceTarget::WorkspaceId { workspace_id } => {
            let registry = app.state::<WorkspaceWindowRegistry>();
            if let Err(err) = focus_or_open_workspace_window(app, &registry, &workspace_id) {
                eprintln!(
                    "dock menu open workspace '{}' failed: {err:#}",
                    workspace_id
                );
            }
        }
        DockRecentWorkspaceTarget::LocalRootPath { root_path } => {
            let manager = app.state::<ConnectionManager>();
            if let Err(err) = ensure_local_connection_for_user_action(app, &manager) {
                eprintln!(
                    "dock menu ensure local daemon failed for '{}': {err:#}",
                    root_path
                );
                return;
            }
            let workspace_id = match resolve_or_create_workspace_id(&manager, &root_path) {
                Ok(workspace_id) => workspace_id,
                Err(err) => {
                    eprintln!(
                        "dock menu resolve/create workspace failed for '{}': {err:#}",
                        root_path
                    );
                    return;
                }
            };
            let registry = app.state::<WorkspaceWindowRegistry>();
            if let Err(err) = focus_or_open_workspace_window(app, &registry, &workspace_id) {
                eprintln!(
                    "dock menu open workspace '{}' failed: {err:#}",
                    workspace_id
                );
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn dock_target_from_sender(sender: *mut AnyObject) -> Option<DockRecentWorkspaceTarget> {
    let item = unsafe { (sender as *mut NSMenuItem).as_ref() }?;
    let represented = item.representedObject()?;
    let raw = represented
        .downcast::<NSString>()
        .ok()?
        .to_string()
        .to_string();
    if raw.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<DockRecentWorkspaceTarget>(&raw).ok()
}

#[cfg(target_os = "macos")]
fn build_action_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Sel,
    target: &AnyObject,
    represented_target: Option<&DockRecentWorkspaceTarget>,
) -> Option<Retained<NSMenuItem>> {
    let title = NSString::from_str(title);
    let key_equivalent = NSString::from_str("");
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &title,
            Some(action),
            &key_equivalent,
        )
    };
    unsafe {
        item.setTarget(Some(target));
    }
    if let Some(target_payload) = represented_target {
        let payload = serde_json::to_string(target_payload).ok()?;
        let payload = NSString::from_str(&payload);
        unsafe {
            item.setRepresentedObject(Some(&payload));
        }
    }
    Some(item)
}

#[cfg(any(target_os = "macos", test))]
fn dock_recent_workspace_targets(
    registry: &WorkspaceWindowRegistry,
) -> Vec<(String, DockRecentWorkspaceTarget)> {
    let mut entries = Vec::new();
    let mut seen_labels = HashSet::new();

    for recent in registry.recent_workspaces() {
        let label = recent.label.trim();
        if label.is_empty() || !seen_labels.insert(label.to_string()) {
            continue;
        }
        entries.push((
            label.to_string(),
            DockRecentWorkspaceTarget::WorkspaceId {
                workspace_id: recent.workspace_id,
            },
        ));
        if entries.len() >= MAX_RECENT_WORKSPACES {
            return entries;
        }
    }

    for recent in registry.dock_recent_local_workspaces() {
        let label = recent.label.trim();
        if label.is_empty() || !seen_labels.insert(label.to_string()) {
            continue;
        }
        entries.push((
            label.to_string(),
            DockRecentWorkspaceTarget::LocalRootPath {
                root_path: recent.root_path,
            },
        ));
        if entries.len() >= MAX_RECENT_WORKSPACES {
            break;
        }
    }

    entries
}

#[cfg(target_os = "macos")]
fn build_open_recent_submenu_item(
    mtm: MainThreadMarker,
    target: &AnyObject,
    recents: &[(String, DockRecentWorkspaceTarget)],
) -> Option<Retained<NSMenuItem>> {
    let submenu_title = NSString::from_str(DOCK_OPEN_RECENT_SUBMENU_TITLE);
    let submenu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &submenu_title);
    submenu.setAutoenablesItems(false);

    for (title, target_payload) in recents {
        let item = build_action_item(
            mtm,
            title,
            sel!(ctxDockOpenRecentWorkspace:),
            target,
            Some(target_payload),
        )?;
        submenu.addItem(&item);
    }

    let key_equivalent = NSString::from_str("");
    let parent = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &submenu_title,
            None,
            &key_equivalent,
        )
    };
    parent.setSubmenu(Some(&submenu));
    Some(parent)
}

#[cfg(target_os = "macos")]
fn build_dock_menu(app: &tauri::AppHandle, target: &AnyObject) -> Option<Retained<NSMenu>> {
    let mtm = MainThreadMarker::new()?;
    let title = NSString::from_str("ctx");
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &title);
    menu.setAutoenablesItems(false);

    let new_window =
        build_action_item(mtm, "New Window", sel!(ctxDockOpenNewWindow:), target, None)?;
    menu.addItem(&new_window);

    let registry = app.state::<WorkspaceWindowRegistry>();
    let recents = dock_recent_workspace_targets(&registry);

    if recents.is_empty() {
        return Some(menu);
    }

    let separator = NSMenuItem::separatorItem(mtm);
    menu.addItem(&separator);

    let recent_submenu = build_open_recent_submenu_item(mtm, target, &recents)?;
    menu.addItem(&recent_submenu);
    Some(menu)
}

#[cfg(target_os = "macos")]
fn install_delegate_dock_menu_methods(delegate_class: *mut AnyClass) {
    unsafe {
        let _ = class_addMethod(
            delegate_class,
            sel!(applicationDockMenu:),
            std::mem::transmute::<extern "C" fn(&AnyObject, Sel, &AnyObject) -> *mut AnyObject, Imp>(
                application_dock_menu,
            ),
            b"@@:@\0".as_ptr().cast(),
        );
        let _ = class_addMethod(
            delegate_class,
            sel!(ctxDockOpenNewWindow:),
            std::mem::transmute::<extern "C" fn(&AnyObject, Sel, *mut AnyObject), Imp>(
                dock_open_new_window,
            ),
            b"v@:@\0".as_ptr().cast(),
        );
        let _ = class_addMethod(
            delegate_class,
            sel!(ctxDockOpenRecentWorkspace:),
            std::mem::transmute::<extern "C" fn(&AnyObject, Sel, *mut AnyObject), Imp>(
                dock_open_recent_workspace,
            ),
            b"v@:@\0".as_ptr().cast(),
        );
    }
}

#[cfg(target_os = "macos")]
pub(super) fn install_macos_dock_menu_bridge(app: tauri::AppHandle) {
    let _ = DOCK_MENU_APP.set(app);
    DOCK_MENU_INSTALL_ONCE.call_once(|| unsafe {
        let Some(mtm) = MainThreadMarker::new() else {
            eprintln!("dock menu bridge skipped: no main thread marker");
            return;
        };
        let ns_app = NSApplication::sharedApplication(mtm);
        let delegate: *mut AnyObject = msg_send![&*ns_app, delegate];
        if delegate.is_null() {
            eprintln!("dock menu bridge skipped: NSApplication delegate is null");
            return;
        }
        let delegate_class: *mut AnyClass = msg_send![delegate, class];
        if delegate_class.is_null() {
            eprintln!("dock menu bridge skipped: delegate class is null");
            return;
        }
        install_delegate_dock_menu_methods(delegate_class);
    });
}

#[cfg(not(target_os = "macos"))]
pub(super) fn install_macos_dock_menu_bridge(_app: tauri::AppHandle) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_recent_workspace_targets_dedupes_and_prioritizes_open_workspaces() {
        let registry = WorkspaceWindowRegistry::default();
        registry.record_recent_workspace("ws-alpha-old", Some("Alpha"));
        registry.record_recent_workspace("ws-beta", Some("Beta"));
        registry.record_recent_workspace("ws-alpha-new", Some("Alpha"));
        registry.set_dock_recent_local_workspaces(vec![
            DockRecentLocalWorkspaceEntry {
                label: "Beta".to_string(),
                root_path: "/tmp/beta".to_string(),
            },
            DockRecentLocalWorkspaceEntry {
                label: "Gamma".to_string(),
                root_path: "/tmp/gamma".to_string(),
            },
        ]);

        let recents = dock_recent_workspace_targets(&registry);
        assert_eq!(recents.len(), 3);
        assert_eq!(recents[0].0, "Alpha");
        assert_eq!(recents[1].0, "Beta");
        assert_eq!(recents[2].0, "Gamma");
        assert_eq!(
            recents[0].1,
            DockRecentWorkspaceTarget::WorkspaceId {
                workspace_id: "ws-alpha-new".to_string()
            }
        );
        assert_eq!(
            recents[1].1,
            DockRecentWorkspaceTarget::WorkspaceId {
                workspace_id: "ws-beta".to_string()
            }
        );
        assert_eq!(
            recents[2].1,
            DockRecentWorkspaceTarget::LocalRootPath {
                root_path: "/tmp/gamma".to_string()
            }
        );
    }

    #[test]
    fn dock_open_recent_submenu_title_is_stable() {
        assert_eq!(DOCK_OPEN_RECENT_SUBMENU_TITLE, "Open Recent");
    }
}
