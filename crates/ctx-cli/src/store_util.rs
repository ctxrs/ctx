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
    let opened = Store::open_read_only(db_path);
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
