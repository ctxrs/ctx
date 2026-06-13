use super::*;
use ctx_desktop_ipc::DesktopUploadBlobReq;

#[tauri::command]
pub(in super::super) async fn desktop_upload_blob(
    app: tauri::AppHandle,
    window: tauri::Window,
    req: DesktopUploadBlobReq,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let state = app.state::<ConnectionManager>();
        let scope = window.label().to_string();
        state
            .upload_blob_for_scope(&scope, req.bytes, req.mime_type, req.name)
            .map_err(to_err)
    })
    .await
    .map_err(|e| format!("blob upload failed: {e}"))?
}
