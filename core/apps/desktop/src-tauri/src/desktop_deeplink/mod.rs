use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use ctx_desktop_ipc::DesktopDeepLinkToken;
use tauri_plugin_deep_link::DeepLinkExt;
use url::Url;

use super::*;

mod handlers;
mod parse;
mod resolve;
mod tokens;
mod types;

pub(super) use handlers::{
    handle_open, handle_reveal, handle_task, handle_workspace, offer_editor_settings,
    open_file_preview_window_with_label, open_in_ctx, open_settings_window,
    open_settings_window_at_route, resolve_editor_target,
};
pub(super) use parse::{
    parse_deep_link, parse_editor_target, parse_open, parse_open_with, parse_optional_positive,
    parse_reveal, parse_target, parse_task, parse_workspace, validate_absolute_path,
    validate_relative_file,
};
pub(super) use resolve::{
    is_target_in_open_workspace, resolve_or_create_workspace_id, resolve_target_path,
    resolve_workspace_id_by_path, resolve_workspace_root, resolve_worktree_info,
};
pub(super) use tokens::desktop_get_deep_link_token;
pub(crate) use tokens::DeepLinkTokenStore;
pub(super) use types::{
    DeepLinkAction, DeepLinkOpen, DeepLinkOpenWith, DeepLinkReveal, DeepLinkTarget, DeepLinkTask,
    DeepLinkWorkspace, WorktreeInfo,
};

pub(super) fn setup_deep_link_listener(app: &tauri::AppHandle) {
    let handle = app.clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            handle_deep_link(handle.clone(), url);
        }
    });

    if let Ok(Some(urls)) = app.deep_link().get_current() {
        for url in urls {
            handle_deep_link(app.clone(), url);
        }
    }

    if let Err(err) = app.deep_link().register_all() {
        eprintln!("deep link register skipped: {err}");
    }
}

pub(super) fn handle_deep_link(app: tauri::AppHandle, url: Url) {
    std::thread::spawn(move || {
        if let Err(err) = handle_deep_link_inner(&app, &url) {
            show_error_dialog(&app, &format!("Deep link failed: {err:#}"));
        }
    });
}

pub(super) fn handle_deep_link_inner(app: &tauri::AppHandle, url: &Url) -> Result<()> {
    let action = parse_deep_link(url)?;
    let state = app.state::<ConnectionManager>();
    let tokens = app.state::<DeepLinkTokenStore>();
    let registry = app.state::<WorkspaceWindowRegistry>();
    let notification_routes = app.state::<DesktopNotificationRouteRegistry>();

    match action {
        DeepLinkAction::Open(req) => handle_open(app, &state, &tokens, &registry, req),
        DeepLinkAction::Reveal(req) => handle_reveal(app, &state, &tokens, &registry, req),
        DeepLinkAction::Workspace(req) => handle_workspace(app, &state, &registry, req),
        DeepLinkAction::Task(req) => handle_task(app, &registry, &notification_routes, req),
        DeepLinkAction::Focus => {
            focus_app_window(app);
            Ok(())
        }
    }
}

#[tauri::command]
pub(super) fn desktop_open_deep_link(
    app: tauri::AppHandle,
    req: ctx_desktop_ipc::DesktopOpenExternalUrlReq,
) -> Result<(), String> {
    let url = Url::parse(req.url.trim()).map_err(|_| "deep link must be absolute".to_string())?;
    if url.scheme() != "ctx" {
        return Err("deep link scheme is not allowed".to_string());
    }
    let req = parse_renderer_open_deep_link(&url).map_err(to_err)?;
    let state = app.state::<ConnectionManager>();
    let tokens = app.state::<DeepLinkTokenStore>();
    let registry = app.state::<WorkspaceWindowRegistry>();
    handle_open(&app, &state, &tokens, &registry, req).map_err(to_err)
}

fn parse_renderer_open_deep_link(url: &Url) -> Result<DeepLinkOpen> {
    let action = parse_deep_link(url)?;
    let DeepLinkAction::Open(req) = action else {
        anyhow::bail!("renderer deep links may only open files");
    };
    if req.open_with == DeepLinkOpenWith::System {
        anyhow::bail!("renderer deep links may not open files with the system handler");
    }
    Ok(req)
}

#[cfg(test)]
mod renderer_deep_link_tests {
    use super::parse_renderer_open_deep_link;
    use url::Url;

    #[test]
    fn renderer_deep_link_allows_file_open_only() {
        let open = Url::parse("ctx://open?path=%2Ftmp%2Fdemo.txt&openWith=editor").unwrap();
        assert!(parse_renderer_open_deep_link(&open).is_ok());

        for denied in [
            "ctx://workspace?path=%2Ftmp%2Fdemo",
            "ctx://task?workspaceId=workspace-1&taskId=task-1",
            "ctx://focus",
            "ctx://open?path=%2Ftmp%2Fdemo.txt&openWith=system",
        ] {
            let url = Url::parse(denied).unwrap();
            assert!(parse_renderer_open_deep_link(&url).is_err(), "{denied}");
        }
    }
}
