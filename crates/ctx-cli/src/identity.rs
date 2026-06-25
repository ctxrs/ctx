use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

const INSTALL_FILE: &str = "install.json";

pub fn install_id(data_root: &Path) -> Result<String> {
    fs::create_dir_all(data_root)?;
    let path = install_path(data_root);
    if path.exists() {
        let value: serde_json::Value = serde_json::from_slice(
            &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))?;
        if let Some(id) = value.get("install_id").and_then(|value| value.as_str()) {
            if !id.trim().is_empty() {
                return Ok(id.to_owned());
            }
        }
    }

    let id = Uuid::new_v4().to_string();
    let body = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "install_id": id,
        "created_at": Utc::now(),
    }))?;
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(id)
}

pub fn install_path(data_root: &Path) -> PathBuf {
    data_root.join(INSTALL_FILE)
}
