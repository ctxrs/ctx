use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use ctx_history_core::platform_security::{
    restrict_private_file, verify_private_directory, verify_private_file,
};
use ctx_pro_host_protocol::ProFilesystemLayout;
use serde_json::json;
use uuid::Uuid;

use super::{
    client::materialize,
    lifecycle::{lifecycle_status_json, sync_parent_directory},
};
use crate::analytics::ProMaterializationTelemetryV1;

const MARKER_FILE_NAME: &str = ".ctx-pro.materialization-pending";
const MARKER_CONTENT: &[u8] = b"ctx-pro-materialization-pending-v1\n";
const MAX_MARKER_BYTES: u64 = 128;

pub(super) fn request(data_root: &Path) -> Result<()> {
    let marker = marker_path(data_root);
    let parent = marker
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid_request: Pro marker has no parent"))?;
    verify_private_directory(parent)
        .context("invalid_request: verify private Pro lifecycle directory")?;
    if pending(data_root)? {
        return Ok(());
    }
    let staged = parent.join(format!(".materialization-pending-{}.next", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .context("write Pro materialization marker")?;
        restrict_private_file(&staged).context("protect Pro materialization marker")?;
        file.write_all(MARKER_CONTENT)
            .context("write Pro materialization marker")?;
        file.sync_all().context("sync Pro materialization marker")?;
        fs::rename(&staged, &marker).context("publish Pro materialization marker")?;
        sync_parent_directory(&marker)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result
}

pub(super) fn defer_setup(
    data_root: &Path,
    account_state: &str,
    helper_updated: bool,
    json_output: bool,
) -> Result<()> {
    request(data_root)?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "payload_type": "pro_setup",
                "ok": true,
                "account_state": account_state,
                "helper_updated": helper_updated,
                "materialization_deferred": true,
                "status": lifecycle_status_json(data_root),
            }))?
        );
    } else {
        println!("ctx Pro trial activated.");
        println!("Pro indexing will run with the initial Core import.");
    }
    Ok(())
}

pub(super) fn pending(data_root: &Path) -> Result<bool> {
    let marker = marker_path(data_root);
    match marker.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => bail!("invalid_request: Pro materialization marker is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("inspect Pro materialization marker"),
    }
    verify_private_file(&marker).context("verify Pro materialization marker")?;
    let mut bytes = Vec::new();
    fs::File::open(&marker)
        .context("open Pro materialization marker")?
        .take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("read Pro materialization marker")?;
    if bytes != MARKER_CONTENT {
        bail!("invalid_request: Pro materialization marker is invalid");
    }
    Ok(true)
}

pub(super) fn clear(data_root: &Path) -> Result<()> {
    if !pending(data_root)? {
        return Ok(());
    }
    let marker = marker_path(data_root);
    fs::remove_file(&marker).context("remove Pro materialization marker")?;
    sync_parent_directory(&marker)
}

pub(super) fn clear_after<T>(data_root: &Path, value: T) -> Result<T> {
    clear(data_root)?;
    Ok(value)
}

pub(crate) fn run_if_pending(data_root: &Path) -> Result<bool> {
    if !pending(data_root)? {
        return Ok(false);
    }
    let mut telemetry = ProMaterializationTelemetryV1::started();
    materialize(data_root, &mut telemetry)?;
    clear(data_root)?;
    Ok(true)
}

pub(super) fn marker_path(data_root: &Path) -> PathBuf {
    ProFilesystemLayout::new(data_root)
        .pro_root()
        .join(MARKER_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use ctx_history_core::platform_security::restrict_private_directory;

    use super::*;

    #[test]
    fn marker_is_private_idempotent_and_strict() {
        let root = tempfile::tempdir().unwrap();
        let pro = ProFilesystemLayout::new(root.path()).pro_root();
        fs::create_dir_all(&pro).unwrap();
        restrict_private_directory(root.path()).unwrap();
        restrict_private_directory(&pro).unwrap();
        assert!(!pending(root.path()).unwrap());
        request(root.path()).unwrap();
        request(root.path()).unwrap();
        assert!(pending(root.path()).unwrap());
        fs::write(marker_path(root.path()), b"wrong").unwrap();
        assert!(pending(root.path()).is_err());
        fs::write(marker_path(root.path()), MARKER_CONTENT).unwrap();
        clear(root.path()).unwrap();
        assert!(!pending(root.path()).unwrap());
    }
}
