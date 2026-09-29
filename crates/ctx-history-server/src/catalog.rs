use crate::{types::now, Error, Result};
use ctx_history_platform::platform_security::{ensure_private_directory, ensure_private_file};
use fs2::FileExt;
use rusqlite::{params, Connection};
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
    time::Duration,
};

pub(crate) fn open(root: &Path) -> Result<(Connection, File)> {
    ensure_private_directory(root)?;
    let lock_path = root.join("server.lock");
    private_file(&lock_path)?;
    let owner = OpenOptions::new().read(true).write(true).open(lock_path)?;
    owner.try_lock_exclusive().map_err(|_| Error::Unavailable)?;
    ensure_private_directory(&root.join("collections"))?;
    let database = root.join("authority.sqlite");
    private_file(&database)?;
    let connection = Connection::open(&database)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
    )?;
    let version: u32 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > 1 {
        return Err(Error::Invalid("unsupported hosted catalog version"));
    }
    connection.execute_batch(SCHEMA)?;
    connection.execute(
        "UPDATE deployment SET instance=?1 WHERE instance=''",
        [uuid::Uuid::new_v4().to_string()],
    )?;
    // Restored checkpoint marker is inspected on every open. A copied old
    // catalog cannot reactivate credentials merely by restarting the process.
    if root.join("recovery-closed").exists() {
        connection.execute("UPDATE deployment SET recovery_closed=1", [])?;
    }
    File::open(root)?.sync_all()?;
    Ok((connection, owner))
}

pub(crate) fn serving(connection: &Connection) -> Result<()> {
    let closed: bool =
        connection.query_row("SELECT recovery_closed FROM deployment", [], |r| r.get(0))?;
    if closed {
        Err(Error::RecoveryClosed)
    } else {
        Ok(())
    }
}

pub(crate) fn audit(
    connection: &Connection,
    action: &str,
    principal: Option<&str>,
    collection: Option<&str>,
) -> Result<()> {
    connection.execute(
        "INSERT INTO audit(at,action,principal,collection) VALUES (?1,?2,?3,?4)",
        params![now()?, action, principal, collection],
    )?;
    Ok(())
}

pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

pub(crate) fn private_file(path: &Path) -> Result<()> {
    match ctx_history_platform::platform_security::create_private_file_new(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(ensure_private_file(path)?),
        Err(e) => Err(e.into()),
    }
}

const SCHEMA: &str = "
BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS deployment (id INTEGER PRIMARY KEY CHECK(id=1), recovery_closed INTEGER NOT NULL DEFAULT 0, instance TEXT NOT NULL DEFAULT '', authority_revision INTEGER NOT NULL DEFAULT 0, authority_path TEXT);
INSERT OR IGNORE INTO deployment(id) VALUES(1);
CREATE TABLE IF NOT EXISTS principals (id TEXT PRIMARY KEY, name TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS collections (id TEXT PRIMARY KEY, name TEXT NOT NULL, sequence INTEGER NOT NULL DEFAULT 0, unsafe_sequence INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS grants (principal TEXT NOT NULL REFERENCES principals(id), collection TEXT NOT NULL REFERENCES collections(id), read INTEGER NOT NULL, publish INTEGER NOT NULL, manage INTEGER NOT NULL, PRIMARY KEY(principal,collection));
CREATE TABLE IF NOT EXISTS credentials (id TEXT PRIMARY KEY, digest BLOB NOT NULL UNIQUE, principal TEXT NOT NULL REFERENCES principals(id), scope INTEGER NOT NULL, expires INTEGER NOT NULL, revoked INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS enrollments (id TEXT PRIMARY KEY, digest BLOB NOT NULL UNIQUE, principal TEXT NOT NULL REFERENCES principals(id), scope INTEGER NOT NULL, expires INTEGER NOT NULL, credential_ttl INTEGER NOT NULL, collection TEXT NOT NULL REFERENCES collections(id));
CREATE TABLE IF NOT EXISTS publications (collection TEXT NOT NULL REFERENCES collections(id), publication TEXT NOT NULL, owner TEXT NOT NULL REFERENCES principals(id), epoch INTEGER NOT NULL, policy INTEGER NOT NULL, revision TEXT NOT NULL, identity TEXT NOT NULL, sequence INTEGER NOT NULL, withdrawn INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(collection,publication), UNIQUE(collection,identity));
CREATE TABLE IF NOT EXISTS operations (collection TEXT NOT NULL REFERENCES collections(id), key TEXT NOT NULL, principal TEXT NOT NULL REFERENCES principals(id), fingerprint TEXT NOT NULL, terminal TEXT NOT NULL, receipt TEXT, sequence INTEGER, operation TEXT, cancel_fenced INTEGER NOT NULL DEFAULT 0 CHECK(cancel_fenced IN (0,1)), PRIMARY KEY(collection,principal,key), UNIQUE(collection,sequence), CHECK((terminal='accepted' AND receipt IS NOT NULL AND sequence IS NOT NULL AND operation IS NULL) OR (terminal='cancelled' AND receipt IS NULL AND sequence IS NULL AND operation IS NOT NULL AND cancel_fenced=1)));
CREATE TABLE IF NOT EXISTS revisions (collection TEXT NOT NULL, publication TEXT NOT NULL, revision TEXT NOT NULL, source TEXT NOT NULL, payload TEXT NOT NULL, descriptor TEXT NOT NULL, PRIMARY KEY(collection,publication,revision), FOREIGN KEY(collection,publication) REFERENCES publications(collection,publication));
CREATE TABLE IF NOT EXISTS event_refs (collection TEXT NOT NULL, publication TEXT NOT NULL, revision TEXT NOT NULL, event TEXT NOT NULL, session TEXT NOT NULL, sequence INTEGER NOT NULL, offset INTEGER NOT NULL, bytes INTEGER NOT NULL, digest BLOB NOT NULL, PRIMARY KEY(collection,publication,revision,event), FOREIGN KEY(collection,publication,revision) REFERENCES revisions(collection,publication,revision));
CREATE INDEX IF NOT EXISTS event_refs_direct ON event_refs(collection,event);
CREATE INDEX IF NOT EXISTS event_refs_session ON event_refs(collection,publication,revision,session,sequence,event);
CREATE TABLE IF NOT EXISTS pending (collection TEXT NOT NULL REFERENCES collections(id), sequence INTEGER NOT NULL, PRIMARY KEY(collection,sequence));
CREATE TABLE IF NOT EXISTS uploads (id TEXT PRIMARY KEY, collection TEXT NOT NULL REFERENCES collections(id), principal TEXT NOT NULL REFERENCES principals(id), digest TEXT NOT NULL, bytes INTEGER NOT NULL, received INTEGER NOT NULL DEFAULT 0, expires INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS audit (sequence INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, action TEXT NOT NULL, principal TEXT, collection TEXT);
CREATE INDEX IF NOT EXISTS revisions_source ON revisions(collection,source);
CREATE INDEX IF NOT EXISTS uploads_collection ON uploads(collection);
PRAGMA user_version=1;
COMMIT;
";
