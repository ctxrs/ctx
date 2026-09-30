//! Disposable validation scratch. Nothing here is accepted authority: only
//! the final catalog transaction may retain a revision and issue its receipt.
use crate::{
    catalog,
    publication::{visit_payload, Descriptor},
    storage::{owned_upload, verify_payload},
    types::now,
    *,
};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) struct PreparedUpload {
    directory: tempfile::TempDir,
}

impl PreparedUpload {
    /// Called under authority. A complete upload cannot accept further writes;
    /// this private link pins its inode across staging expiration/removal.
    pub(crate) fn pin(
        root: &Path,
        connection: &Connection,
        collection: &str,
        principal: &str,
        upload: &str,
        expected: &UploadSpec,
    ) -> Result<Self> {
        check_complete(connection, collection, principal, upload, expected)?;
        let directory = tempfile::Builder::new()
            .prefix(".validate-")
            .tempdir_in(root.join("staging"))?;
        fs::hard_link(
            root.join("staging").join(upload),
            directory.path().join("payload"),
        )?;
        Ok(Self { directory })
    }

    pub(crate) fn payload(&self) -> PathBuf {
        self.directory.path().join("payload")
    }

    pub(crate) fn references(&self) -> PathBuf {
        self.directory.path().join("references.sqlite")
    }

    /// Streaming validation and locator construction do not hold authority.
    /// SQLite scratch bounds memory without retaining records or introducing
    /// another durable queue. A failed/crashed validation is never replayed.
    pub(crate) fn validate(
        &self,
        server: &HistoryServer,
        root: &Path,
        spec: &UploadSpec,
        descriptor: &Descriptor,
    ) -> Result<()> {
        verify_payload(&self.payload(), spec)?;
        catalog::private_file(&self.references())?;
        let mut references = Connection::open(self.references())?;
        references.execute_batch(
            "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-2048;
             CREATE TABLE event_refs (event TEXT PRIMARY KEY, session TEXT NOT NULL,
             sequence INTEGER NOT NULL, offset INTEGER NOT NULL, bytes INTEGER NOT NULL,
             digest BLOB NOT NULL) WITHOUT ROWID;",
        )?;
        let tx = references.transaction()?;
        let mut insert = tx.prepare("INSERT INTO event_refs VALUES (?1,?2,?3,?4,?5,?6)")?;
        let mut offset = 0u64;
        visit_payload(&self.payload(), &descriptor.member, |record| {
            #[cfg(test)]
            server.hooks.run(crate::tests::repair::Stage::Validation);
            let encoded = record.encode_stored()?;
            let mapped =
                ctx_history_archive::map_record(&descriptor.binding, &descriptor.member, record)?;
            insert.execute(params![
                mapped.event_id.as_uuid().to_string(),
                mapped.session_id.as_uuid().to_string(),
                mapped.event_sequence,
                offset,
                encoded.len() as u64,
                Sha256::digest(&encoded).to_vec(),
            ])?;
            offset += encoded.len() as u64 + 1;
            Ok(())
        })?;
        drop(insert);
        tx.commit()?;
        // Existing retained files are immutable under the server API. Hashing
        // them here also keeps integrity verification outside authority.
        let retained = root.join("payloads").join(&spec.sha256);
        if retained.exists() {
            verify_payload(&retained, spec)?;
        }
        #[cfg(not(test))]
        let _ = server;
        Ok(())
    }

    /// Only a completely validated member reaches promotion. The returned
    /// flag lets a failed catalog transaction remove its newly created link.
    pub(crate) fn promote(&self, retained: &Path) -> Result<bool> {
        match fs::hard_link(self.payload(), retained) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

pub(crate) fn check_complete(
    connection: &Connection,
    collection: &str,
    principal: &str,
    upload: &str,
    expected: &UploadSpec,
) -> Result<()> {
    let (spec, status) = owned_upload(connection, collection, upload, principal)?;
    if status.expires_at <= now()? {
        return Err(Error::Expired);
    }
    if &spec != expected || status.received_bytes != spec.bytes {
        return Err(Error::Conflict);
    }
    Ok(())
}

/// Startup holds the exclusive server-root lock, so no live validator can own
/// these directories. Interrupted scratch is discarded, never replayed.
pub(crate) fn clean_scratch(root: &Path, connection: &Connection) -> Result<()> {
    let mut collections = connection.prepare("SELECT id FROM collections")?;
    let mut rows = collections.query([])?;
    while let Some(row) = rows.next()? {
        let collection: String = row.get(0)?;
        crate::types::collection_id(&collection)?;
        let entries = match fs::read_dir(root.join("collections").join(collection).join("staging"))
        {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".validate-"))
            {
                fs::remove_dir_all(entry.path())?;
            }
        }
    }
    Ok(())
}
