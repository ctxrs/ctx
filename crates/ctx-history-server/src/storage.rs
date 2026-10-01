use crate::{
    auth::authorize,
    catalog,
    types::{collection_id, now},
    *,
};
use ctx_history_platform::platform_security::ensure_private_directory;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

impl HistoryServer {
    pub fn begin_upload(
        &self,
        token: &str,
        collection: &str,
        spec: UploadSpec,
    ) -> Result<UploadStatus> {
        valid_spec(&spec)?;
        let mut connection = self.lock()?;
        let principal = authorize(&connection, token, collection, Access::Publish)?;
        self.expire_uploads(&connection)?;
        let staged: u64 = connection.query_row("SELECT count(*) FROM uploads", [], |r| r.get(0))?;
        let reserved: u64 = connection.query_row(
            "SELECT coalesce(sum(bytes-received),0) FROM uploads",
            [],
            |r| r.get(0),
        )?;
        if staged >= self.config.max_staged_uploads as u64
            || spec
                .bytes
                .saturating_add(reserved)
                .saturating_add(self.config.minimum_free_bytes)
                > fs2::available_space(&self.config.root)?
        {
            return Err(Error::Capacity);
        }
        let root = self.collection_root(collection);
        ensure_private_directory(&root)?;
        ensure_private_directory(&root.join("staging"))?;
        ensure_private_directory(&root.join("payloads"))?;
        catalog::sync_directory(&self.config.root.join("collections"))?;
        catalog::sync_directory(&root)?;
        let id = uuid::Uuid::new_v4().to_string();
        let expires = now()?
            .checked_add(self.config.staging_ttl_seconds)
            .ok_or(Error::Capacity)?;
        catalog::private_file(&root.join("staging").join(&id))?;
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO uploads(id,collection,principal,digest,bytes,expires) VALUES (?1,?2,?3,?4,?5,?6)",
            params![id,collection,principal,spec.sha256,spec.bytes,expires])?;
        tx.commit()?;
        drop(connection);
        self.observe(ServerObservation::Upload {
            operation: ServerOperation::BeginUpload,
            bytes: 0,
            replay: false,
        });
        Ok(UploadStatus {
            id,
            publisher: principal,
            received_bytes: 0,
            expected_bytes: spec.bytes,
            expires_at: expires,
        })
    }

    pub fn upload_status(&self, token: &str, collection: &str, id: &str) -> Result<UploadStatus> {
        let connection = self.lock()?;
        let principal = authorize(&connection, token, collection, Access::Publish)?;
        let (_, status) = owned_upload(&connection, collection, id, &principal)?;
        if status.expires_at <= now()? {
            return Err(Error::Expired);
        }
        // A crash may leave an unacknowledged tail. The SQL offset owns resumable
        // staging; clients retain complete bytes until a revision receipt.
        let file = OpenOptions::new()
            .write(true)
            .open(self.collection_root(collection).join("staging").join(id))?;
        if file.metadata()?.len() < status.received_bytes {
            return Err(Error::Expired);
        }
        file.set_len(status.received_bytes)?;
        Ok(status)
    }

    pub fn upload_chunk(
        &self,
        token: &str,
        collection: &str,
        id: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<UploadStatus> {
        if bytes.is_empty() || bytes.len() > self.config.max_chunk_bytes {
            return Err(Error::Invalid("chunk size"));
        }
        let mut connection = self.lock()?;
        let principal = authorize(&connection, token, collection, Access::Publish)?;
        let (_, mut status) = owned_upload(&connection, collection, id, &principal)?;
        if status.expires_at <= now()? {
            return Err(Error::Expired);
        }
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or(Error::Capacity)?;
        if end > status.expected_bytes || offset > status.received_bytes {
            return Err(Error::Conflict);
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.collection_root(collection).join("staging").join(id))?;
        if file.metadata()?.len() < status.received_bytes {
            return Err(Error::Expired);
        }
        file.set_len(status.received_bytes)?;
        file.seek(SeekFrom::Start(offset))?;
        if offset < status.received_bytes {
            if end > status.received_bytes {
                return Err(Error::Conflict);
            }
            let mut previous = vec![0; bytes.len()];
            file.read_exact(&mut previous)?;
            return if previous == bytes {
                drop(connection);
                self.observe(ServerObservation::Upload {
                    operation: ServerOperation::UploadChunk,
                    bytes: bytes.len() as u64,
                    replay: true,
                });
                Ok(status)
            } else {
                Err(Error::Conflict)
            };
        }
        if fs2::available_space(&self.config.root)?
            < (bytes.len() as u64).saturating_add(self.config.minimum_free_bytes)
        {
            return Err(Error::Capacity);
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        catalog::sync_directory(&self.collection_root(collection).join("staging"))?;
        let tx = connection.transaction()?;
        tx.execute(
            "UPDATE uploads SET received=?1 WHERE id=?2",
            params![end, id],
        )?;
        tx.commit()?;
        status.received_bytes = end;
        drop(connection);
        self.observe(ServerObservation::Upload {
            operation: ServerOperation::UploadChunk,
            bytes: bytes.len() as u64,
            replay: false,
        });
        Ok(status)
    }

    fn expire_uploads(&self, connection: &Connection) -> Result<()> {
        let mut statement =
            connection.prepare("SELECT id,collection FROM uploads WHERE expires<=?1")?;
        let expired = statement
            .query_map([now()?], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (id, collection) in expired {
            let path = self.collection_root(&collection).join("staging").join(&id);
            match fs::remove_file(path) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
            connection.execute("DELETE FROM uploads WHERE id=?1", [id])?;
        }
        Ok(())
    }
}

pub(crate) fn owned_upload(
    connection: &Connection,
    collection: &str,
    id: &str,
    principal: &str,
) -> Result<(UploadSpec, UploadStatus)> {
    collection_id(id)?;
    connection.query_row("SELECT digest,bytes,received,expires FROM uploads WHERE id=?1 AND collection=?2 AND principal=?3",
        params![id,collection,principal],|r| Ok((UploadSpec{sha256:r.get(0)?,bytes:r.get(1)?},UploadStatus{id:id.into(),publisher:principal.into(),expected_bytes:r.get(1)?,received_bytes:r.get(2)?,expires_at:r.get(3)?})))
        .optional()?.ok_or(Error::NotFound)
}

pub(crate) fn valid_spec(spec: &UploadSpec) -> Result<()> {
    if spec.sha256.len() != 64
        || !spec
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || spec.bytes == 0
        || spec.bytes > i64::MAX as u64
    {
        return Err(Error::Invalid("payload digest or length"));
    }
    Ok(())
}

pub(crate) fn verify_payload(path: &Path, spec: &UploadSpec) -> Result<()> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() != spec.bytes {
        return Err(Error::Invalid("payload length mismatch"));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    if hex::encode(digest.finalize()) != spec.sha256 {
        return Err(Error::Invalid("payload digest mismatch"));
    }
    Ok(())
}
