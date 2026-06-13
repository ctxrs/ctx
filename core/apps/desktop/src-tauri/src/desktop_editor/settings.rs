use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct DesktopSettings {
    #[serde(default)]
    pub(crate) editor: DesktopEditorSettings,
    #[serde(default)]
    pub(crate) updates: DesktopUpdateSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct DesktopUpdateSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) channel: Option<String>,
}

#[tauri::command]
pub(crate) fn desktop_get_editor_settings(
    app: tauri::AppHandle,
) -> Result<DesktopEditorSettings, String> {
    Ok(load_desktop_settings(&app).editor)
}

#[tauri::command]
pub(crate) fn desktop_update_editor_settings(
    app: tauri::AppHandle,
    req: DesktopEditorSettings,
) -> Result<DesktopEditorSettings, String> {
    let mut current = load_desktop_settings(&app);
    current.editor = validate_renderer_editor_settings(req).map_err(to_err)?;
    save_desktop_settings(&app, &current).map_err(to_err)?;
    Ok(current.editor)
}

#[tauri::command]
pub(crate) fn desktop_get_update_channel(
    app: tauri::AppHandle,
) -> Result<DesktopUpdateChannelSettings, String> {
    let channel = load_desktop_update_channel_preference(&app)
        .map_err(to_err)?
        .unwrap_or_else(|| DEFAULT_DESKTOP_UPDATE_CHANNEL.to_string());
    Ok(DesktopUpdateChannelSettings { channel })
}

#[tauri::command]
pub(crate) fn desktop_update_update_channel(
    app: tauri::AppHandle,
    req: DesktopUpdateChannelSettings,
) -> Result<DesktopUpdateChannelSettings, String> {
    let channel = validate_user_update_channel(&req.channel).map_err(to_err)?;
    let mut current = load_desktop_settings(&app);
    current.updates.channel = Some(channel.clone());
    save_desktop_settings(&app, &current).map_err(to_err)?;
    Ok(DesktopUpdateChannelSettings { channel })
}

fn desktop_settings_path(_app: &tauri::AppHandle) -> Result<PathBuf> {
    let root = desktop_local_data_root()?;
    Ok(ctx_fs::paths::ui_root(root).join("desktop-settings.json"))
}

pub(crate) fn load_desktop_settings(app: &tauri::AppHandle) -> DesktopSettings {
    let path = match desktop_settings_path(app) {
        Ok(path) => path,
        Err(_) => return DesktopSettings::default(),
    };
    let data = match std::fs::read_to_string(&path) {
        Ok(data) => data,
        Err(_) => return DesktopSettings::default(),
    };
    serde_json::from_str::<DesktopSettings>(&data)
        .map(sanitize_persisted_desktop_settings)
        .unwrap_or_default()
}

fn load_desktop_settings_strict(app: &tauri::AppHandle) -> Result<Option<DesktopSettings>> {
    let path = desktop_settings_path(app)?;
    let data = match std::fs::read_to_string(&path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    serde_json::from_str::<DesktopSettings>(&data)
        .with_context(|| format!("parsing {}", path.display()))
        .map(Some)
}

pub(crate) const DEFAULT_DESKTOP_UPDATE_CHANNEL: &str = "stable";

fn validate_user_update_channel(raw: &str) -> Result<String> {
    let channel = raw.trim().to_ascii_lowercase();
    match channel.as_str() {
        "stable" | "canary" => Ok(channel),
        _ => anyhow::bail!("invalid update channel '{raw}' (expected stable or canary)"),
    }
}

pub(crate) fn load_desktop_update_channel_preference(
    app: &tauri::AppHandle,
) -> Result<Option<String>> {
    let Some(settings) = load_desktop_settings_strict(app)? else {
        return Ok(None);
    };
    settings
        .updates
        .channel
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(validate_user_update_channel)
        .transpose()
}

fn sanitize_persisted_desktop_settings(mut settings: DesktopSettings) -> DesktopSettings {
    settings.editor = sanitize_persisted_editor_settings(settings.editor);
    settings.updates.channel = settings
        .updates
        .channel
        .as_deref()
        .and_then(|value| validate_user_update_channel(value).ok());
    settings
}

fn sanitize_persisted_editor_settings(
    mut settings: DesktopEditorSettings,
) -> DesktopEditorSettings {
    if matches!(settings.target, DesktopEditorTarget::Custom) {
        settings.target = DesktopEditorTarget::System;
    }
    settings.custom_command = None;
    settings.remote_authority = settings
        .remote_authority
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    settings
}

fn validate_renderer_editor_settings(
    mut settings: DesktopEditorSettings,
) -> Result<DesktopEditorSettings> {
    if matches!(settings.target, DesktopEditorTarget::Custom) {
        anyhow::bail!("custom editor commands cannot be configured from the renderer");
    }
    if settings
        .custom_command
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        anyhow::bail!("custom editor commands cannot be configured from the renderer");
    }
    settings.custom_command = None;
    settings.remote_authority = settings
        .remote_authority
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Ok(settings)
}

fn save_desktop_settings(app: &tauri::AppHandle, settings: &DesktopSettings) -> Result<()> {
    let path = desktop_settings_path(app)?;
    save_desktop_settings_to_path(&path, settings)
}

fn save_desktop_settings_to_path(path: &Path, settings: &DesktopSettings) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(settings)?;
    ctx_fs::permissions::write_private_file_atomic_sync(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ctx-desktop-settings-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn save_desktop_settings_writes_private_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_test_dir();
        let path = dir.join("ui").join("desktop-settings.json");

        save_desktop_settings_to_path(&path, &DesktopSettings::default()).unwrap();

        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn renderer_editor_settings_reject_custom_target_and_command() {
        let custom_target = validate_renderer_editor_settings(DesktopEditorSettings {
            target: DesktopEditorTarget::Custom,
            custom_command: Some("code --goto {path}:{line}:{col}".to_string()),
            remote_authority: None,
        })
        .unwrap_err();
        assert!(
            format!("{custom_target:#}").contains("custom editor commands"),
            "unexpected error: {custom_target:#}"
        );

        let custom_command = validate_renderer_editor_settings(DesktopEditorSettings {
            target: DesktopEditorTarget::Cursor,
            custom_command: Some("cursor {path}".to_string()),
            remote_authority: None,
        })
        .unwrap_err();
        assert!(
            format!("{custom_command:#}").contains("custom editor commands"),
            "unexpected error: {custom_command:#}"
        );
    }

    #[test]
    fn persisted_editor_settings_drop_legacy_custom_command() {
        let sanitized = sanitize_persisted_editor_settings(DesktopEditorSettings {
            target: DesktopEditorTarget::Custom,
            custom_command: Some("code {path}".to_string()),
            remote_authority: Some(" ssh-remote+devbox ".to_string()),
        });
        assert_eq!(sanitized.target, DesktopEditorTarget::System);
        assert_eq!(sanitized.custom_command, None);
        assert_eq!(
            sanitized.remote_authority.as_deref(),
            Some("ssh-remote+devbox")
        );
    }

    #[test]
    fn persisted_update_channel_sanitizer_keeps_only_supported_user_channels() {
        let stable = sanitize_persisted_desktop_settings(DesktopSettings {
            editor: DesktopEditorSettings::default(),
            updates: DesktopUpdateSettings {
                channel: Some(" Canary ".to_string()),
            },
        });
        assert_eq!(stable.updates.channel.as_deref(), Some("canary"));

        let invalid = sanitize_persisted_desktop_settings(DesktopSettings {
            editor: DesktopEditorSettings::default(),
            updates: DesktopUpdateSettings {
                channel: Some("nightly".to_string()),
            },
        });
        assert_eq!(invalid.updates.channel, None);
    }
}
