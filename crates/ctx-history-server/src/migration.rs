//! Migrate access metadata only; retained publications and citation data stay intact.
use crate::{auth::hash_secret, catalog, types::now, Error, Result, TokenFile};
use ctx_history_platform::platform_security::open_verified_private_file;
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use std::{
    io::Read,
    path::{Path, PathBuf},
};

pub(crate) fn migrate(connection: &Connection, root: Option<&Path>) -> Result<()> {
    // Audit events prove principal ownership, never a particular device. Only
    // the protected operator descriptor can preserve an accountwide credential.
    let operator = root.map(operator_token).transpose()?.flatten();
    let tx = connection.unchecked_transaction()?;
    tx.execute_batch(
        "ALTER TABLE principals ADD COLUMN server_owner INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE credentials ADD COLUMN collection TEXT REFERENCES collections(id);
         ALTER TABLE credentials ADD COLUMN server_owner INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE credentials ADD COLUMN enrollment_id TEXT;
         ALTER TABLE enrollments ADD COLUMN credential_expires_ceiling INTEGER NOT NULL DEFAULT 0;
         CREATE INDEX credentials_principal ON credentials(principal,id);",
    )?;
    let owner: Option<String> = tx.query_row(
        "SELECT principal FROM audit WHERE action IN ('bootstrap','restore_checkpoint') ORDER BY sequence DESC LIMIT 1",
        [], |r|r.get(0)).optional()?.flatten();
    if let Some(owner) = owner {
        tx.execute(
            "UPDATE principals SET server_owner=1 WHERE id=?1 AND revoked=0",
            [&owner],
        )?;
        if let Some(operator) = operator.filter(|token| token.principal == owner) {
            tx.execute(
                "UPDATE credentials SET server_owner=1 WHERE id=?1 AND principal=?2 AND digest=?3 AND scope=7
                 AND revoked=0 AND (expires=0 OR expires>?4) AND EXISTS(SELECT 1 FROM principals WHERE id=?2 AND server_owner=1 AND revoked=0)",
                params![operator.credential.id, owner, hash_secret(&operator.credential.secret), now()?])?;
        }
    }
    // Current grants do not prove a v1 device's original intended audience,
    // even when only one collection remains. Preserve evidence and membership,
    // but require explicit enrollment for every unscoped nonoperator device.
    tx.execute_batch(
        "UPDATE credentials SET revoked=1 WHERE server_owner=0;
         DELETE FROM enrollments;
         PRAGMA user_version=2;",
    )?;
    catalog::audit(&tx, "migrate_access_v2", None, None)?;
    tx.commit()?;
    Ok(())
}

fn operator_token(root: &Path) -> Result<Option<TokenFile>> {
    let pointer: Option<PathBuf> = read_private(&root.join("operator-file.json"))?;
    read_private(&pointer.unwrap_or_else(|| root.join("operator.json")))
}

fn read_private<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let file = match open_verified_private_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 {
        return Err(Error::Invalid("operator metadata exceeds supported size"));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| Error::Invalid("invalid operator metadata"))
}
