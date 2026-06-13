use super::*;

#[cfg_attr(not(feature = "automation"), allow(dead_code))]
#[derive(Debug, Deserialize)]
pub(crate) struct DesktopDemoConnectionRequest {
    pub(crate) base_url: String,
    pub(crate) token: String,
}

#[tauri::command]
pub(crate) fn desktop_get_connection(
    state: tauri::State<ConnectionManager>,
    window: tauri::Window,
) -> DesktopConnectionInfo {
    state.info_for_scope(window.label())
}

#[tauri::command]
pub(crate) fn desktop_disconnect(
    state: tauri::State<ConnectionManager>,
    window: tauri::Window,
) -> Result<(), String> {
    state.disconnect_for_scope(window.label());
    Ok(())
}

#[cfg_attr(not(feature = "automation"), allow(dead_code))]
pub(crate) fn demo_commands_enabled() -> bool {
    fn parse_boolish(value: &str) -> Option<bool> {
        match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    }

    std::env::var("CTX_DESKTOP_ALLOW_DEMO_COMMANDS")
        .ok()
        .as_deref()
        .and_then(parse_boolish)
        .unwrap_or(false)
}

#[tauri::command]
pub(crate) fn desktop_set_demo_connection(
    state: tauri::State<ConnectionManager>,
    window: tauri::Window,
    req: DesktopDemoConnectionRequest,
) -> Result<DesktopConnectionInfo, String> {
    #[cfg(feature = "automation")]
    {
        if !demo_commands_enabled() {
            return Err(
                "desktop_set_demo_connection requires CTX_DESKTOP_ALLOW_DEMO_COMMANDS=1"
                    .to_string(),
            );
        }
        let base_url = req.base_url.trim().to_string();
        if base_url.is_empty() {
            return Err("base_url is required".to_string());
        }
        let token = req.token.trim().to_string();
        if token.is_empty() {
            return Err("token is required".to_string());
        }
        state.set_local_attached_for_scope(
            window.label(),
            base_url,
            token,
            None,
            LocalConnectionSource::EnvOverride,
        );
        return Ok(state.info_for_scope(window.label()));
    }

    #[cfg(not(feature = "automation"))]
    {
        let _ = state;
        let _ = req;
        let _ = window;
        Err("desktop_set_demo_connection is automation-only".to_string())
    }
}
