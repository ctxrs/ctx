//! A small independently retained recovery floor, not a second control store.
//! It proves current security authority, not completeness of accepted history.
//! Missing data since a checkpoint is a backup loss window; missing policy is not.
use crate::{catalog, CheckpointInfo, Error, HistoryServer, Result, ServerConfig};
use fs2::FileExt;
use rusqlite::{params, Connection, Transaction};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityFloor {
    version: u32,
    deployment: String,
    revision: u64,
}

pub(crate) fn initialize(
    config: &mut ServerConfig,
    connection: &Connection,
) -> Result<Option<File>> {
    let stored: Option<String> =
        connection.query_row("SELECT authority_path FROM deployment", [], |r| r.get(0))?;
    if let Some(path) = &config.authority_file {
        let absolute = absolute_file(path)?;
        if absolute.starts_with(fs::canonicalize(&config.root)?) {
            return Err(Error::Invalid("authority file must be outside server root"));
        }
        if stored
            .as_ref()
            .is_some_and(|saved| Path::new(saved) != absolute)
        {
            return Err(Error::Conflict);
        }
        config.authority_file = Some(absolute);
    } else {
        config.authority_file = stored.map(PathBuf::from);
    }
    let Some(path) = &config.authority_file else {
        return Ok(None);
    };
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    catalog::private_file(&lock_path)?;
    let owner = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)?;
    owner.try_lock_exclusive().map_err(|_| Error::Unavailable)?;
    let current = floor(connection)?;
    let closed: bool =
        connection.query_row("SELECT recovery_closed FROM deployment", [], |r| r.get(0))?;
    if closed {
        return Ok(Some(owner));
    }
    if path.exists() {
        let previous = read_floor(path)?;
        if previous.deployment != current.deployment || previous.revision > current.revision {
            return Err(Error::RecoveryClosed);
        }
    } else if connection.query_row(
        "SELECT authority_path IS NOT NULL FROM deployment",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        // Losing the independently retained file must not silently manufacture a
        // replacement from a possibly stale copy of the source catalog.
        return Err(Error::RecoveryClosed);
    }
    persist_floor(path, &current)?;
    connection.execute(
        "UPDATE deployment SET authority_path=?1",
        [path
            .to_str()
            .ok_or(Error::Invalid("authority path must be UTF-8"))?],
    )?;
    Ok(Some(owner))
}

impl HistoryServer {
    /// Commit security state whose rollback could revive access, withdrawn
    /// content or retired writer consent. Ordinary data acceptance commits its
    /// SQLite transaction directly; collection sequences track that separately.
    pub(crate) fn commit_authority(&self, tx: Transaction<'_>) -> Result<()> {
        tx.execute(
            "UPDATE deployment SET authority_revision=authority_revision+1",
            [],
        )?;
        let current = floor(&tx)?;
        tx.commit()?;
        if let Some(path) = &self.config.authority_file {
            if let Err(error) = persist_floor(path, &current) {
                // SQLite already committed. No success or new requests may
                // escape this process with an older recovery floor. A normal
                // restart retries this write from the surviving live catalog.
                self.authority_unavailable.store(true, Ordering::Release);
                return Err(error);
            }
        }
        Ok(())
    }

    /// Requires exact agreement with the independently retained security floor.
    /// Later data-only publications/corrections and their receipts can be lost:
    /// this restores the checkpoint's data, not all acknowledged history. A
    /// missing receipt is NotFound; clients retaining the original operation
    /// and payload may restage and retry against the restored predecessor.
    /// Receipt sequences are coverage within this restored catalog, not a
    /// deployment-wide high-water mark across disaster recovery. Missing
    /// revocations, withdrawals or writer-policy changes cannot be reconciled.
    pub fn restore_checkpoint_with_authority(
        checkpoint: &Path,
        destination: &Path,
        current_authority_file: &Path,
    ) -> Result<CheckpointInfo> {
        let authority_path = absolute_file(current_authority_file)?;
        let current = read_floor(&authority_path)?;
        let snapshot = Connection::open_with_flags(
            checkpoint.join("authority.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        if floor(&snapshot)? != current {
            return Err(Error::RecoveryClosed);
        }
        drop(snapshot);
        let info = Self::restore_checkpoint(checkpoint, destination)?;
        let connection = Connection::open(destination.join("authority.sqlite"))?;
        connection.execute_batch("PRAGMA synchronous=FULL;")?;
        // Recheck after copying large payloads. The resumed server rechecks the
        // same persistent authority path on open, closing a concurrent advance.
        if read_floor(&authority_path)? != current {
            return Err(Error::RecoveryClosed);
        }
        connection.execute(
            "UPDATE deployment SET recovery_closed=0,authority_path=?1",
            params![authority_path
                .to_str()
                .ok_or(Error::Invalid("authority path must be UTF-8"))?],
        )?;
        drop(connection);
        File::open(destination.join("authority.sqlite"))?.sync_all()?;
        fs::remove_file(destination.join("recovery-closed"))?;
        catalog::sync_directory(destination)?;
        Ok(info)
    }
}

fn floor(connection: &Connection) -> Result<AuthorityFloor> {
    Ok(connection.query_row(
        "SELECT instance,authority_revision FROM deployment",
        [],
        |r| {
            Ok(AuthorityFloor {
                version: 1,
                deployment: r.get(0)?,
                revision: r.get(1)?,
            })
        },
    )?)
}

fn read_floor(path: &Path) -> Result<AuthorityFloor> {
    let file = ctx_history_platform::platform_security::open_verified_private_file(path)?;
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(Error::Invalid("authority floor size"));
    }
    let floor: AuthorityFloor = serde_json::from_slice(&bytes)?;
    if floor.version != 1 {
        return Err(Error::Invalid("authority floor version"));
    }
    Ok(floor)
}

fn persist_floor(path: &Path, floor: &AuthorityFloor) -> Result<()> {
    let parent = path
        .parent()
        .ok_or(Error::Invalid("authority file parent"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    ctx_history_platform::platform_security::ensure_private_file(file.path())?;
    file.write_all(&serde_json::to_vec(floor)?)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| Error::Io(error.error))?;
    catalog::sync_directory(parent)
}

fn absolute_file(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or(Error::Invalid("authority filename"))?;
    Ok(fs::canonicalize(parent)?.join(name))
}
