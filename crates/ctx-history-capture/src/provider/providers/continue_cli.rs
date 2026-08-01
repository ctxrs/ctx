use std::path::Path;

pub(crate) mod native_path;

pub(crate) fn continue_session_json_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("json")
        && path.file_name().and_then(|name| name.to_str()) != Some("sessions.json")
}
