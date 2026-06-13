use std::path::{Path, PathBuf};

use super::super::capture::{
    cursor_login_home, ensure_private_dir, initialize_cursor_capture_file,
    write_cursor_capture_hook,
};

pub(super) struct CursorLoginWorkspace {
    pub(super) login_home: PathBuf,
    pub(super) workdir: PathBuf,
    pub(super) hook_path: PathBuf,
    pub(super) capture_path: PathBuf,
}

pub(super) struct CursorLoginWorkspaceError {
    login_home: PathBuf,
    message: String,
    redact: bool,
}

impl CursorLoginWorkspaceError {
    pub(super) fn login_home(&self) -> &Path {
        &self.login_home
    }

    pub(super) fn into_status_error(self) -> String {
        if self.redact {
            ctx_observability::logs::redact_sensitive(&self.message)
        } else {
            self.message
        }
    }
}

pub(super) async fn prepare_cursor_login_workspace(
    data_root: &Path,
    login_id: &str,
) -> Result<CursorLoginWorkspace, CursorLoginWorkspaceError> {
    let login_home = cursor_login_home(data_root, login_id);
    let workdir = login_home.join("workspace");
    let hook_path = login_home.join("capture-hook.cjs");
    let capture_path = login_home.join("captured_tokens.jsonl");

    if let Err(err) = async {
        ensure_private_dir(&login_home).await?;
        ensure_private_dir(&workdir).await
    }
    .await
    {
        return Err(CursorLoginWorkspaceError {
            login_home,
            message: format!("failed to prepare login workspace: {err}"),
            redact: false,
        });
    }

    if let Err(err) = write_cursor_capture_hook(&hook_path).await {
        return Err(CursorLoginWorkspaceError {
            login_home,
            message: err.to_string(),
            redact: true,
        });
    }
    if let Err(err) = initialize_cursor_capture_file(&capture_path).await {
        return Err(CursorLoginWorkspaceError {
            login_home,
            message: format!("failed to initialize capture file: {err}"),
            redact: false,
        });
    }

    Ok(CursorLoginWorkspace {
        login_home,
        workdir,
        hook_path,
        capture_path,
    })
}
