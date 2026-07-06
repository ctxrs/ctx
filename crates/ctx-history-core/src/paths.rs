use std::{env, path::PathBuf};

use directories::BaseDirs;

use crate::{CoreError, Result};

pub fn default_data_root() -> Result<PathBuf> {
    if let Some(value) = env::var_os("CTX_DATA_ROOT") {
        return Ok(PathBuf::from(value));
    }

    let base = BaseDirs::new().ok_or(CoreError::MissingHome)?;
    Ok(base.home_dir().join(".ctx"))
}

pub fn history_dir(root: PathBuf) -> PathBuf {
    root
}

pub fn database_path(root: PathBuf) -> PathBuf {
    history_dir(root).join("work.sqlite")
}

pub fn object_dir(root: PathBuf) -> PathBuf {
    history_dir(root).join("objects")
}

pub fn blob_dir(root: PathBuf) -> PathBuf {
    object_dir(root)
}

pub fn spool_dir(root: PathBuf) -> PathBuf {
    history_dir(root).join("spool")
}

pub fn inbox_dir(root: PathBuf) -> PathBuf {
    spool_dir(root)
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
