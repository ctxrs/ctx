use super::*;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct CursorCapturedTokenLine {
    #[serde(default)]
    event: String,
    #[serde(default)]
    service: String,
    #[serde(default)]
    value: String,
}

#[cfg(unix)]
async fn set_private_permissions(path: &StdPath, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .await
        .with_context(|| format!("setting permissions {:o} on {}", mode, path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
async fn set_private_permissions(_path: &StdPath, _mode: u32) -> anyhow::Result<()> {
    Ok(())
}

pub(super) async fn ensure_private_dir(path: &StdPath) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(path)
        .await
        .with_context(|| format!("creating private dir {}", path.display()))?;
    set_private_permissions(path, 0o700).await?;
    Ok(())
}

async fn write_private_file(path: &StdPath, bytes: &[u8], label: &str) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing parent dir for {}", path.display()))?;
    ensure_private_dir(parent).await?;
    tokio::fs::write(path, bytes)
        .await
        .with_context(|| format!("writing {label} {}", path.display()))?;
    set_private_permissions(path, 0o600).await?;
    Ok(())
}

// The hook appends with `{ mode: 0o600 }`, but append mode does not tighten an existing file.
// Pre-create the capture file here so managed auth tokens never inherit permissive defaults.
pub(super) async fn initialize_cursor_capture_file(path: &StdPath) -> anyhow::Result<()> {
    write_private_file(path, b"", "cursor capture file").await
}

const CURSOR_KEYCHAIN_CAPTURE_HOOK: &str = r#"const fs = require('fs');
const path = process.env.CTX_CURSOR_CAPTURE_FILE;
function emit(row) {
  if (!path) return;
  try {
    fs.appendFileSync(path, JSON.stringify(row) + '\n', { mode: 0o600 });
  } catch {}
}
const cp = require('child_process');
for (const name of ['spawn', 'spawnSync']) {
  const orig = cp[name];
  if (typeof orig !== 'function') continue;
  cp[name] = function (...args) {
    try {
      const cmd = String(args[0] || '');
      const argv = Array.isArray(args[1]) ? args[1] : [];
      if (cmd.includes('/usr/bin/security') || cmd === 'security') {
        if (String(argv[0] || '') === 'add-generic-password') {
          const serviceIdx = argv.indexOf('-s');
          const valueIdx = argv.indexOf('-w');
          const service = serviceIdx >= 0 ? String(argv[serviceIdx + 1] || '') : '';
          const value = valueIdx >= 0 ? String(argv[valueIdx + 1] || '') : '';
          if (service.startsWith('cursor-') && value) {
            emit({ ts: new Date().toISOString(), event: 'captured', service, value });
          }
        }
      }
    } catch {}
    return orig.apply(this, args);
  };
}
"#;

pub(super) async fn write_cursor_capture_hook(path: &StdPath) -> anyhow::Result<()> {
    write_private_file(path, CURSOR_KEYCHAIN_CAPTURE_HOOK.as_bytes(), "cursor hook").await
}

pub(super) async fn parse_cursor_captured_tokens(
    path: &StdPath,
) -> anyhow::Result<(Option<String>, Option<String>, Option<String>)> {
    let payload = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("reading cursor capture {}", path.display()))?;
    let mut access_token = None::<String>;
    let mut refresh_token = None::<String>;
    let mut api_key = None::<String>;
    for line in payload.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: CursorCapturedTokenLine = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if parsed.event != "captured" || parsed.value.trim().is_empty() {
            continue;
        }
        match parsed.service.as_str() {
            "cursor-access-token" => access_token = Some(parsed.value),
            "cursor-refresh-token" => refresh_token = Some(parsed.value),
            "cursor-api-key" => api_key = Some(parsed.value),
            _ => {}
        }
    }
    Ok((access_token, refresh_token, api_key))
}

pub(super) fn cursor_login_home(data_root: &StdPath, login_id: &str) -> PathBuf {
    data_root
        .join("providers")
        .join("cursor")
        .join("login-sessions")
        .join(login_id)
}
