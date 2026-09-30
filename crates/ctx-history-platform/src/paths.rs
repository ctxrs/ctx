use std::{env, path::PathBuf};

use crate::{PlatformError, Result};

pub fn default_data_root() -> Result<PathBuf> {
    managed_data_root()
}

/// The complete managed root, including config and daemon lifecycle state.
/// An unset or empty CTX_DATA_ROOT selects ~/.ctx. Nonempty values must be
/// absolute Unicode paths without control characters so detached processes
/// and native service managers can preserve the same selection.
/// A per-command --data-root override does not change this root.
pub fn managed_data_root() -> Result<PathBuf> {
    if let Some(value) = env::var_os("CTX_DATA_ROOT").filter(|value| !value.is_empty()) {
        let text = value
            .to_str()
            .ok_or(PlatformError::InvalidDataRoot("must be a Unicode path"))?;
        if text.chars().any(char::is_control) {
            return Err(PlatformError::InvalidDataRoot(
                "must not contain control characters",
            ));
        }
        let root = PathBuf::from(value);
        if !root.is_absolute() {
            return Err(PlatformError::InvalidDataRoot("must be an absolute path"));
        }
        return Ok(root);
    }
    let home = dirs::home_dir().ok_or(PlatformError::MissingHome)?;
    Ok(home.join(".ctx"))
}

pub fn history_dir(root: PathBuf) -> PathBuf {
    root
}

pub fn config_path(root: PathBuf) -> PathBuf {
    history_dir(root).join("config.toml")
}

pub fn logs_dir(root: PathBuf) -> PathBuf {
    history_dir(root).join("logs")
}

pub fn device_path(root: PathBuf) -> PathBuf {
    history_dir(root).join("device.json")
}
