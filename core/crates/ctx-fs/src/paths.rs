use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn default_ctx_home() -> Result<PathBuf> {
    let base = directories::BaseDirs::new().context("resolving home dir")?;
    Ok(base.home_dir().join(".ctx"))
}

pub fn ui_root(data_root: impl AsRef<Path>) -> PathBuf {
    data_root.as_ref().join("ui")
}
