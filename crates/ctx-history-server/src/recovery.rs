//! Finalized checkpoints and private recovery into a fresh server root.
use crate::{
    auth::insert_credential,
    catalog,
    storage::{valid_spec, verify_payload},
    types::{collection_id, now},
    *,
};
use ctx_history_platform::platform_security::{ensure_private_directory, ensure_private_file};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointInfo {
    pub version: u32,
    pub created_at: u64,
    pub payloads: u64,
    pub catalog_sha256: String,
}

/// Safe to display: the generated credential is written only to its protected file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreInfo {
    pub checkpoint: CheckpointInfo,
    pub principal: String,
    pub collections: Vec<String>,
}

impl HistoryServer {
    /// Finalizes a consistent checkpoint for ordinary backup software. Restore
    /// retains this checkpoint's history but replaces all access with a new owner.
    pub fn checkpoint(&self, destination: &Path) -> Result<CheckpointInfo> {
        let connection = self.lock()?;
        let parent = parent(destination);
        if destination.try_exists()? {
            return Err(Error::Conflict);
        }
        let temporary = tempfile::Builder::new()
            .prefix(".ctx-checkpoint-")
            .tempdir_in(parent)?;
        ensure_private_directory(temporary.path())?;
        let catalog_path = temporary.path().join("authority.sqlite");
        let mut copy = Connection::open(&catalog_path)?;
        let backup = rusqlite::backup::Backup::new(&connection, &mut copy)?;
        backup.run_to_completion(128, Duration::from_millis(10), None)?;
        drop(backup);
        copy.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; DELETE FROM uploads;",
        )?;
        drop(copy);
        ensure_private_file(&catalog_path)?;
        File::open(&catalog_path)?.sync_all()?;
        let payloads = copy_payloads(&connection, &self.config.root, temporary.path())?;
        let info = CheckpointInfo {
            version: 1,
            created_at: now()?,
            payloads,
            catalog_sha256: file_digest(&catalog_path)?,
        };
        write_token_file(&temporary.path().join("checkpoint.json"), &info)?;
        catalog::sync_directory(temporary.path())?;
        fs::rename(temporary.path(), destination)?;
        catalog::sync_directory(parent)?;
        Ok(info)
    }

    /// Restore a finalized checkpoint to a fresh root, private to a new owner.
    /// The protected TokenFile selects the first restored collection. Its single
    /// credential is valid for every restored collection through its owner's grants.
    /// All old credentials,
    /// enrollments and grants are discarded; old principals stay revoked for
    /// provenance. Existing owner identities and exact citation evidence remain.
    ///
    /// Changes after the checkpoint, including withdrawals, are lost. The owner
    /// must review restored history and remove anything that should no longer
    /// be shared before issuing fresh invitations or grants. This API does not
    /// claim to preserve later withdrawals. Derived projections rebuild through
    /// the existing indexer; no extra recovery approval state is required.
    pub fn restore_checkpoint(
        checkpoint: &Path,
        destination: &Path,
        token_file: &Path,
    ) -> Result<RestoreInfo> {
        if destination.try_exists()? || token_file.try_exists()? {
            return Err(Error::Conflict);
        }
        let info: CheckpointInfo =
            serde_json::from_reader(File::open(checkpoint.join("checkpoint.json"))?)?;
        if info.version != 1 {
            return Err(Error::Invalid("unsupported checkpoint version"));
        }
        let catalog_path = checkpoint.join("authority.sqlite");
        if file_digest(&catalog_path)? != info.catalog_sha256 {
            return Err(Error::Invalid("checkpoint catalog checksum mismatch"));
        }
        let connection =
            Connection::open_with_flags(&catalog_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != 1 {
            return Err(Error::Invalid("unsupported hosted catalog version"));
        }
        let collections = connection
            .prepare("SELECT id FROM collections ORDER BY id")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for collection in &collections {
            collection_id(collection)?;
        }
        let default_collection = collections.first().cloned().ok_or(Error::Invalid(
            "checkpoint has no collections; initialize a new server instead",
        ))?;
        let temporary = tempfile::Builder::new()
            .prefix(".ctx-restore-")
            .tempdir_in(parent(destination))?;
        ensure_private_directory(temporary.path())?;
        if copy_payloads(&connection, checkpoint, temporary.path())? != info.payloads {
            return Err(Error::Invalid("checkpoint payload inventory mismatch"));
        }
        let restored_catalog = temporary.path().join("authority.sqlite");
        fs::copy(catalog_path, &restored_catalog)?;
        ensure_private_file(&restored_catalog)?;
        if file_digest(&restored_catalog)? != info.catalog_sha256 {
            return Err(Error::Invalid("checkpoint catalog checksum mismatch"));
        }
        let mut restored = Connection::open(&restored_catalog)?;
        restored.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
        let tx = restored.transaction()?;
        tx.execute_batch(
            "DELETE FROM credentials; DELETE FROM enrollments; DELETE FROM grants;
             DELETE FROM uploads; UPDATE principals SET revoked=1;
             INSERT OR IGNORE INTO pending SELECT collection,sequence FROM operations WHERE terminal='accepted';",
        )?;
        let principal = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO principals(id,name) VALUES (?1,'Recovery owner')",
            [&principal],
        )?;
        tx.execute(
            "INSERT INTO grants SELECT ?1,id,1,1,1 FROM collections",
            [&principal],
        )?;
        let credential = insert_credential(
            &tx,
            &principal,
            Grants {
                read: true,
                publish: true,
                manage: true,
            },
            0,
        )?;
        catalog::audit(&tx, "restore_checkpoint", Some(&principal), None)?;
        tx.commit()?;
        drop(restored);
        File::open(&restored_catalog)?.sync_all()?;
        let tokens = TokenFile {
            principal: principal.clone(),
            collection: default_collection,
            credential,
        };
        // An in-root credential is published with the root. External output is
        // prepared beside its destination and never replaces an existing file.
        let external_token = if let Ok(relative) = token_file.strip_prefix(destination) {
            if relative.as_os_str().is_empty()
                || relative
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err(Error::Invalid(
                    "credential must be a file inside the restored root",
                ));
            }
            let path = temporary.path().join(relative);
            ensure_private_directory(parent(&path))?;
            write_token_file(&path, &tokens)?;
            false
        } else {
            let mut output = tempfile::NamedTempFile::new_in(parent(token_file))?;
            ensure_private_file(output.path())?;
            serde_json::to_writer(&mut output, &tokens)?;
            output.write_all(b"\n")?;
            output.as_file().sync_all()?;
            output
                .persist_noclobber(token_file)
                .map_err(|e| Error::Io(e.error))?;
            true
        };
        catalog::sync_directory(temporary.path())?;
        if let Err(error) = fs::rename(temporary.path(), destination) {
            if external_token {
                let _ = fs::remove_file(token_file);
            }
            return Err(error.into());
        }
        catalog::sync_directory(parent(token_file))?;
        catalog::sync_directory(parent(destination))?;
        Ok(RestoreInfo {
            checkpoint: info,
            principal,
            collections,
        })
    }
}

fn copy_payloads(connection: &Connection, source: &Path, destination: &Path) -> Result<u64> {
    ensure_private_directory(&destination.join("collections"))?;
    let mut statement = connection
        .prepare("SELECT DISTINCT collection,payload FROM revisions ORDER BY collection,payload")?;
    let mut rows = statement.query([])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let collection: String = row.get(0)?;
        let payload: String = row.get(1)?;
        collection_id(&collection)?;
        let spec: UploadSpec = serde_json::from_str(&payload)?;
        valid_spec(&spec)?;
        let from = source
            .join("collections")
            .join(&collection)
            .join("payloads")
            .join(&spec.sha256);
        verify_payload(&from, &spec)?;
        let root = destination.join("collections").join(&collection);
        ensure_private_directory(&root)?;
        ensure_private_directory(&root.join("payloads"))?;
        let to = root.join("payloads").join(&spec.sha256);
        fs::copy(from, &to)?;
        ensure_private_file(&to)?;
        verify_payload(&to, &spec)?;
        File::open(to)?.sync_all()?;
        catalog::sync_directory(&root.join("payloads"))?;
        catalog::sync_directory(&root)?;
        count += 1;
    }
    catalog::sync_directory(&destination.join("collections"))?;
    Ok(count)
}

fn file_digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 64 * 1024];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
