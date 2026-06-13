use super::*;

#[path = "desktop_editor/opener.rs"]
mod opener;
#[path = "desktop_editor/settings.rs"]
mod settings;
pub(super) use ctx_desktop_ipc::{
    DesktopEditorSettings, DesktopEditorTarget, DesktopGitCloneReq, DesktopOpenFileReq,
    DesktopOpenPathReq, DesktopReadBinaryFileResp, DesktopSaveTextFileReq,
    DesktopUpdateChannelSettings,
};
pub(super) use settings::{
    desktop_get_editor_settings, desktop_get_update_channel, desktop_update_editor_settings,
    desktop_update_update_channel, load_desktop_settings, load_desktop_update_channel_preference,
    DEFAULT_DESKTOP_UPDATE_CHANNEL,
};

pub(super) fn open_in_editor(
    settings: &DesktopEditorSettings,
    path: &Path,
    line: Option<u32>,
    col: Option<u32>,
    remote: bool,
) -> Result<()> {
    opener::open_in_editor(settings, path, line, col, remote)
}

pub(super) fn open_with_system(target: &str) -> Result<()> {
    opener::open_with_system(target)
}

#[tauri::command]
pub(super) async fn desktop_pick_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .blocking_pick_folder()
            .and_then(|path| path.into_path().ok())
            .map(|path| path.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| format!("folder picker failed: {e}"))
}

#[tauri::command]
pub(super) async fn desktop_save_text_file(
    app: tauri::AppHandle,
    req: DesktopSaveTextFileReq,
) -> Result<Option<String>, String> {
    let suggested = req
        .suggested_name
        .unwrap_or_else(|| "conversation.md".to_string());
    let suggested = suggested.trim().to_string();
    let contents = req.contents;

    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app
            .dialog()
            .file()
            .add_filter("Markdown", &["md"])
            .set_title("Save Conversation Export");
        if !suggested.is_empty() {
            dialog = dialog.set_file_name(&suggested);
        }
        let picked = dialog
            .blocking_save_file()
            .and_then(|path| path.into_path().ok())
            .map(|path| path.to_string_lossy().to_string());
        let Some(path) = picked else {
            return Ok::<Option<String>, String>(None);
        };

        std::fs::write(&path, contents).map_err(|e| format!("failed to write file: {e}"))?;
        Ok(Some(path))
    })
    .await
    .map_err(|e| format!("save file dialog failed: {e}"))??;
    Ok(picked)
}

#[tauri::command]
pub(super) fn desktop_open_file(
    state: tauri::State<ConnectionManager>,
    app: tauri::AppHandle,
    window: tauri::Window,
    req: DesktopOpenFileReq,
) -> Result<(), String> {
    let scope = window.label().to_string();
    let worktree_id = req.worktree_id.trim();
    if worktree_id.is_empty() {
        return Err("worktree_id is required".to_string());
    }
    let path = req.path.trim();
    if path.is_empty() {
        return Err("path is required".to_string());
    }

    let worktree_root = resolve_worktree_root(&state, &scope, worktree_id).map_err(to_err)?;
    let resolved = resolve_worktree_path(&worktree_root, path).map_err(to_err)?;
    let line = req.line.filter(|v| *v > 0);
    let col = req.col.filter(|v| *v > 0);
    let editor_settings = load_desktop_settings(&app).editor;
    open_in_editor(
        &editor_settings,
        &resolved,
        line,
        col,
        state.is_remote_for_scope(&scope),
    )
    .map_err(to_err)?;
    Ok(())
}

#[tauri::command]
pub(super) fn desktop_open_path(
    app: tauri::AppHandle,
    req: DesktopOpenPathReq,
) -> Result<(), String> {
    let _ = app;
    let _ = req;
    Err(
        "desktop_open_path is disabled; use worktree-scoped desktop_open_file or the ctx deep-link prompt flow"
            .to_string(),
    )
}

#[tauri::command]
pub(super) fn desktop_read_binary_file(
    req: DesktopOpenPathReq,
) -> Result<DesktopReadBinaryFileResp, String> {
    let resolved = resolve_desktop_image_path(&req)?;
    let bytes = std::fs::read(&resolved).map_err(|e| format!("failed to read file: {e}"))?;
    Ok(DesktopReadBinaryFileResp {
        path: resolved.to_string_lossy().to_string(),
        bytes,
    })
}

#[tauri::command]
pub(super) async fn desktop_git_clone(req: DesktopGitCloneReq) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let repo_url = req.repo_url.trim().to_string();
        if repo_url.is_empty() {
            return Err("repo_url is required".to_string());
        }
        let dest_parent = PathBuf::from(req.dest_parent);
        if !dest_parent.exists() {
            return Err(format!(
                "destination folder does not exist: {}",
                dest_parent.display()
            ));
        }

        let name =
            derive_repo_name(&repo_url).ok_or_else(|| "could not derive repo name".to_string())?;
        let dest = dest_parent.join(&name);
        if dest.exists() {
            return Err(format!("destination already exists: {}", dest.display()));
        }

        let output = Command::new("git")
            .arg("clone")
            .arg("--")
            .arg(&repo_url)
            .arg(&dest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("failed to spawn git: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "git clone failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| format!("git clone failed: {e}"))?
}

fn resolve_desktop_image_path(req: &DesktopOpenPathReq) -> Result<PathBuf, String> {
    const MAX_DESKTOP_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
    const ALLOWED_EXTENSIONS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico", "avif", "heic", "heif",
    ];

    let path = req.path.trim();
    if path.is_empty() {
        return Err("path is required".to_string());
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err("path must be absolute".to_string());
    }
    let resolved = std::fs::canonicalize(&path).map_err(|e| format!("invalid path: {e}"))?;
    if !resolved.exists() {
        return Err("path does not exist".to_string());
    }
    let metadata = std::fs::metadata(&resolved).map_err(|e| format!("invalid path: {e}"))?;
    if metadata.len() > MAX_DESKTOP_IMAGE_BYTES {
        return Err("desktop binary reads are limited to images up to 25 MiB".to_string());
    }
    let extension = resolved
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .ok_or_else(|| "desktop binary reads are limited to image files".to_string())?;
    if !ALLOWED_EXTENSIONS
        .iter()
        .any(|candidate| extension == *candidate)
    {
        return Err("desktop binary reads are limited to image files".to_string());
    }
    let bytes = std::fs::read(&resolved).map_err(|e| format!("failed to read file: {e}"))?;
    if !bytes_match_supported_image_format(&bytes) {
        return Err("desktop binary reads are limited to image files".to_string());
    }
    Ok(resolved)
}

fn bytes_match_supported_image_format(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
        || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || bytes.starts_with(b"BM")
        || bytes.starts_with(&[0x49, 0x49, 0x2A, 0x00])
        || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
        || bytes.starts_with(&[0x00, 0x00, 0x01, 0x00])
        || is_webp(bytes)
        || is_supported_iso_bmff_image(bytes)
}

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

fn is_supported_iso_bmff_image(bytes: &[u8]) -> bool {
    const SUPPORTED_BRANDS: &[[u8; 4]] = &[
        *b"avif", *b"avis", *b"heic", *b"heix", *b"hevc", *b"hevx", *b"heim", *b"heis", *b"hevm",
        *b"hevs", *b"mif1", *b"msf1",
    ];

    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return false;
    }

    let mut brand = [0_u8; 4];
    brand.copy_from_slice(&bytes[8..12]);
    SUPPORTED_BRANDS.contains(&brand)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_test_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ctx-desktop-editor-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolve_desktop_image_path_accepts_supported_extensions() {
        let dir = temp_test_dir();
        let path = dir.join("photo.PNG");
        std::fs::write(&path, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();

        let resolved = resolve_desktop_image_path(&DesktopOpenPathReq {
            path: path.to_string_lossy().to_string(),
            line: None,
            col: None,
        })
        .unwrap();

        assert_eq!(resolved, std::fs::canonicalize(&path).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_desktop_image_path_rejects_non_image_extensions() {
        let dir = temp_test_dir();
        let path = dir.join("notes.txt");
        std::fs::write(&path, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();

        let err = resolve_desktop_image_path(&DesktopOpenPathReq {
            path: path.to_string_lossy().to_string(),
            line: None,
            col: None,
        })
        .unwrap_err();

        assert!(err.contains("limited to image files"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_desktop_image_path_rejects_svg_files() {
        let dir = temp_test_dir();
        let path = dir.join("icon.svg");
        std::fs::write(&path, b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>").unwrap();

        let err = resolve_desktop_image_path(&DesktopOpenPathReq {
            path: path.to_string_lossy().to_string(),
            line: None,
            col: None,
        })
        .unwrap_err();

        assert!(err.contains("limited to image files"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_desktop_image_path_rejects_renamed_non_image_bytes() {
        let dir = temp_test_dir();
        let path = dir.join("notes.png");
        std::fs::write(&path, b"not really a png").unwrap();

        let err = resolve_desktop_image_path(&DesktopOpenPathReq {
            path: path.to_string_lossy().to_string(),
            line: None,
            col: None,
        })
        .unwrap_err();

        assert!(err.contains("limited to image files"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_desktop_image_path_rejects_oversized_images() {
        let dir = temp_test_dir();
        let path = dir.join("large.png");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len((25 * 1024 * 1024 + 1) as u64).unwrap();

        let err = resolve_desktop_image_path(&DesktopOpenPathReq {
            path: path.to_string_lossy().to_string(),
            line: None,
            col: None,
        })
        .unwrap_err();

        assert!(err.contains("25 MiB"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_worktree_path_accepts_canonical_inside_path() {
        let dir = temp_test_dir();
        let nested = dir.join("src");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("main.rs");
        std::fs::write(&file, b"fn main() {}\n").unwrap();

        let resolved = resolve_worktree_path(&dir, "src/main.rs").unwrap();
        assert_eq!(resolved, std::fs::canonicalize(&file).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_worktree_path_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let dir = temp_test_dir();
        let outside = temp_test_dir();
        let outside_file = outside.join("secret.txt");
        std::fs::write(&outside_file, b"secret").unwrap();
        symlink(&outside_file, dir.join("escape.txt")).unwrap();

        let err = resolve_worktree_path(&dir, "escape.txt").unwrap_err();
        assert!(
            format!("{err:#}").contains("outside the worktree root"),
            "unexpected error: {err:#}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }
}

fn resolve_worktree_root(
    state: &ConnectionManager,
    scope: &str,
    worktree_id: &str,
) -> Result<PathBuf> {
    let resp = state.daemon_request_for_scope(
        scope,
        DesktopDaemonRequest {
            method: "GET".to_string(),
            path: format!("/api/worktrees/{worktree_id}"),
            body: None,
            headers: vec![],
        },
    )?;
    if resp.status != 200 {
        return Err(anyhow!(
            "failed to load worktree ({status}): {body}",
            status = resp.status,
            body = resp.body
        ));
    }
    let value: serde_json::Value =
        serde_json::from_str(&resp.body).context("parsing worktree response")?;
    let root = value
        .get("root_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("worktree root_path missing"))?;
    Ok(PathBuf::from(root))
}

pub(super) fn resolve_worktree_path(worktree_root: &Path, raw_path: &str) -> Result<PathBuf> {
    let root = std::fs::canonicalize(worktree_root)
        .with_context(|| format!("canonicalizing {}", worktree_root.display()))?;
    let mut candidate = expand_tilde(raw_path).unwrap_or_else(|| PathBuf::from(raw_path));
    if !candidate.is_absolute() {
        candidate = root.join(candidate);
    }
    let candidate = std::fs::canonicalize(normalize_path(&candidate))
        .with_context(|| format!("canonicalizing {}", candidate.display()))?;
    if !candidate.starts_with(&root) {
        return Err(anyhow!("path is outside the worktree root"));
    }
    Ok(candidate)
}

fn derive_repo_name(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let last = trimmed.rsplit('/').next()?;
    let name = last.trim_end_matches(".git").trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}
