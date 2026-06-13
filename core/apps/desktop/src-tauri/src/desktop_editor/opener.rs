use super::*;

pub(crate) fn open_in_editor(
    settings: &DesktopEditorSettings,
    path: &Path,
    line: Option<u32>,
    col: Option<u32>,
    remote: bool,
) -> Result<()> {
    let remote_authority = settings
        .remote_authority
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    if remote {
        match settings.target {
            DesktopEditorTarget::VsCode => {
                let authority = remote_authority
                    .ok_or_else(|| anyhow!("remote authority is not configured"))?;
                open_with_system(&vscode_remote_uri("vscode", authority, path, line, col))
            }
            DesktopEditorTarget::VsCodeInsiders => {
                let authority = remote_authority
                    .ok_or_else(|| anyhow!("remote authority is not configured"))?;
                open_with_system(&vscode_remote_uri(
                    "vscode-insiders",
                    authority,
                    path,
                    line,
                    col,
                ))
            }
            DesktopEditorTarget::Cursor => {
                let authority = remote_authority
                    .ok_or_else(|| anyhow!("remote authority is not configured"))?;
                open_with_system(&vscode_remote_uri("cursor", authority, path, line, col))
            }
            DesktopEditorTarget::Windsurf => {
                let authority = remote_authority
                    .ok_or_else(|| anyhow!("remote authority is not configured"))?;
                open_with_system(&vscode_remote_uri("windsurf", authority, path, line, col))
            }
            DesktopEditorTarget::Antigravity => {
                let authority = remote_authority
                    .ok_or_else(|| anyhow!("remote authority is not configured"))?;
                open_with_system(&vscode_remote_uri(
                    "antigravity",
                    authority,
                    path,
                    line,
                    col,
                ))
            }
            DesktopEditorTarget::Custom => anyhow::bail!("custom editor commands are disabled"),
            _ => anyhow::bail!("remote editor target does not support remote paths"),
        }
    } else {
        match settings.target {
            DesktopEditorTarget::System => open_with_system(path.to_string_lossy().as_ref()),
            DesktopEditorTarget::VsCode => open_with_system(&vscode_uri("vscode", path, line, col)),
            DesktopEditorTarget::VsCodeInsiders => {
                open_with_system(&vscode_uri("vscode-insiders", path, line, col))
            }
            DesktopEditorTarget::Cursor => open_with_system(&vscode_uri("cursor", path, line, col)),
            DesktopEditorTarget::Windsurf => {
                open_with_system(&vscode_uri("windsurf", path, line, col))
            }
            DesktopEditorTarget::Antigravity => {
                open_with_system(&vscode_uri("antigravity", path, line, col))
            }
            DesktopEditorTarget::Idea => open_with_system(&jetbrains_uri("idea", path, line)),
            DesktopEditorTarget::Pycharm => open_with_system(&jetbrains_uri("pycharm", path, line)),
            DesktopEditorTarget::Xcode => open_xcode(path, line),
            DesktopEditorTarget::AndroidStudio => open_android_studio(path, line),
            DesktopEditorTarget::Custom => anyhow::bail!("custom editor commands are disabled"),
        }
    }
}

pub(crate) fn open_with_system(target: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("open");
    #[cfg(target_os = "linux")]
    let mut cmd = Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut cmd = Command::new("explorer");

    cmd.arg(target);
    let status = cmd.status().context("spawning open command")?;
    if !status.success() {
        anyhow::bail!("open command failed (exit={status})");
    }
    Ok(())
}

fn open_xcode(path: &Path, line: Option<u32>) -> Result<()> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("Xcode is only available on macOS");
    }
    let mut cmd = Command::new("xcrun");
    cmd.arg("xed");
    if let Some(line) = line {
        cmd.arg("-l").arg(line.to_string());
    }
    cmd.arg(path);
    let status = cmd.status().context("launching xed")?;
    if !status.success() {
        anyhow::bail!("xed failed (exit={status})");
    }
    Ok(())
}

fn open_android_studio(path: &Path, _line: Option<u32>) -> Result<()> {
    if cfg!(target_os = "macos") {
        let status = Command::new("open")
            .arg("-a")
            .arg("Android Studio")
            .arg(path)
            .status()
            .context("opening Android Studio")?;
        if !status.success() {
            anyhow::bail!("failed to open Android Studio (exit={status})");
        }
        return Ok(());
    }

    let status = Command::new("studio")
        .arg(path)
        .status()
        .context("launching Android Studio")?;
    if !status.success() {
        anyhow::bail!("studio command failed (exit={status})");
    }
    Ok(())
}

fn vscode_remote_uri(
    scheme: &str,
    authority: &str,
    path: &Path,
    line: Option<u32>,
    col: Option<u32>,
) -> String {
    let mut uri = format!(
        "{scheme}://vscode-remote/{authority}{}",
        encode_uri_path(path)
    );
    if let Some(line) = line {
        uri.push(':');
        uri.push_str(&line.to_string());
        if let Some(col) = col {
            uri.push(':');
            uri.push_str(&col.to_string());
        }
    }
    uri
}

fn vscode_uri(scheme: &str, path: &Path, line: Option<u32>, col: Option<u32>) -> String {
    let mut uri = format!("{scheme}://file/{}", encode_uri_path(path));
    if let Some(line) = line {
        uri.push(':');
        uri.push_str(&line.to_string());
        if let Some(col) = col {
            uri.push(':');
            uri.push_str(&col.to_string());
        }
    }
    uri
}

fn jetbrains_uri(scheme: &str, path: &Path, line: Option<u32>) -> String {
    let raw = path.to_string_lossy();
    let encoded = urlencoding::encode(&raw);
    let mut uri = format!("{scheme}://open?file={encoded}");
    if let Some(line) = line {
        uri.push_str(&format!("&line={line}"));
    }
    uri
}

fn encode_uri_path(path: &Path) -> String {
    let mut raw = path.to_string_lossy().replace('\\', "/");
    if cfg!(target_os = "windows") {
        if raw.len() >= 2 && raw.as_bytes().get(1) == Some(&b':') {
            raw = format!("/{raw}");
        }
    }
    percent_encode_path(&raw)
}

fn percent_encode_path(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        let ch = b as char;
        let keep = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~' | '/' | ':');
        if keep {
            out.push(ch);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
