use std::{fs, path::Path};

use anyhow::{bail, Context, Result};
use ctx_history_index::{finish_session_erasure, GenerationWriter, WriterOptions};
use ctx_history_platform::platform_security::{create_private_file_new, ensure_private_directory};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::DaemonRuntimeConfig;

/// Pause background capture while removing local Core and derived copies.
pub fn erase_session(
    data_root: &Path,
    config: &DaemonRuntimeConfig,
    session: Uuid,
) -> Result<Value> {
    if data_root.join("archive-root.json").try_exists()? {
        bail!("session erasure requires a native history root; archive-owned roots retain original archive members");
    }
    let previous_automatic = ctx_app_config::persisted_daemon_enabled(data_root)?;
    let result = (|| {
        let update = crate::update_indexing_mode(data_root, config, false)?;
        if update.automatic || update.running {
            bail!("automatic indexing could not be paused; remove the indexing mode override and retry");
        }
        erase_quiescent(data_root, session)
    })();
    if previous_automatic {
        crate::update_indexing_mode(data_root, config, true)
            .context("restore automatic indexing after session erasure")?;
    }
    result
}

fn erase_quiescent(data_root: &Path, session: Uuid) -> Result<Value> {
    let _daemon = ctx_daemon_runtime::DaemonQuiescenceGuard::acquire(data_root)?
        .context("another daemon owns this history root; retry session erasure")?;
    let root = data_root.join("search/lexical");
    let writer = GenerationWriter::open(&root, WriterOptions::default())?
        .into_writer()
        .map_err(|_| anyhow::anyhow!("generation migration recovery required"))?;
    let mut sharing = Vec::new();
    let sharing_root = data_root.join("sharing");
    if sharing_root.try_exists()? {
        for entry in fs::read_dir(sharing_root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let store = ctx_history_sharing::SharingStore::new(entry.path());
                let erased = store
                    .erase_session(session)
                    .context("erase queued session uploads")?;
                sharing.push(json!({
                    "connection": entry.file_name().to_string_lossy(),
                    "discarded_uploads": erased.discarded_uploads,
                    "discarded_uploads_may_have_reached_server": erased.discarded_uploads_may_have_reached_server,
                }));
            }
        }
    }
    let receipt = writer.erase_session(session)?;
    // Keep coordination files in place while deleting derived payloads. A
    // concurrent materializer must keep observing the same locked inode.
    clear_projection(
        &data_root.join("search/semantic"),
        &["flat_transaction.lock", "flat_writer.lock"],
    )?;
    clear_projection(
        &data_root.join("search/attribution"),
        &[
            "attribution-materializer.lock",
            "attribution-manifest.publication-lock",
        ],
    )?;
    finish_session_erasure(&root)?;
    Ok(json!({
        "schema_version": 2,
        "session_id": session,
        "erased": true,
        "sharing": sharing,
        "generation_id": receipt.generation_id,
        "indexed_items": receipt.indexed_documents,
    }))
}

fn clear_projection(root: &Path, lock_names: &[&str]) -> Result<()> {
    if !root.try_exists()? {
        return Ok(());
    }
    ensure_private_directory(root)?;
    let mut locks = Vec::new();
    for name in lock_names {
        let path = root.join(name);
        match create_private_file_new(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let lock = ctx_history_platform::platform_security::open_verified_private_file(&path)?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .context("derived history is in use; close readers and retry session erasure")?;
        locks.push(lock);
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if lock_names.iter().any(|name| entry.file_name() == *name) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}
