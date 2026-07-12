use std::path::Path;

use anyhow::{anyhow, Context, Result};

use ctx_history_store::{Store, StoreError};

pub(crate) fn open_existing_store_read_only(db_path: &Path, command: &str) -> Result<Store> {
    if !db_path.exists() {
        return Err(anyhow!(
            "ctx store is not initialized at {}; run `ctx setup` or `ctx import` first",
            db_path.display()
        ));
    }
    // SQLite immutable mode is side-effect-free but ignores WAL content, so it
    // is valid only when the store has no live WAL sidecars. A writer creates
    // those sidecars before committing work; readers must then join the normal
    // WAL protocol to see a consistent snapshot.
    let opened = if sqlite_sidecar_exists(db_path, "-wal") || sqlite_sidecar_exists(db_path, "-shm")
    {
        Store::open_read_only(db_path)
    } else {
        Store::open_read_only_snapshot(db_path)
    };
    match opened {
        Ok(store) => Ok(store),
        Err(StoreError::UnsupportedSchemaVersion(version)) => Err(anyhow!(
            "ctx store schema version {version} is not supported by this ctx binary; run a writable command such as `ctx setup` or `ctx import` with a compatible ctx binary to migrate before using `{command}`"
        )),
        Err(err) => {
            Err(err).with_context(|| format!("open read-only ctx store {}", db_path.display()))
        }
    }
}

fn sqlite_sidecar_exists(db_path: &Path, suffix: &str) -> bool {
    let mut path = db_path.as_os_str().to_owned();
    path.push(suffix);
    Path::new(&path).exists()
}
