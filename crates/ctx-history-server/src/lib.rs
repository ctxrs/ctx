//! Single-node hosted history. SQLite owns authorization and acceptance;
//! immutable archive members own retained bytes and Core owns search generations.
//!
//! The caller explicitly selects a data root. Opening this library never enables
//! provider capture, a listener, telemetry, upgrades, or external model calls.

mod admission;
mod auth;
mod catalog;
mod http;
mod operations;
mod projection;
mod publication;
mod read;
mod recovery;
mod storage;
mod types;

pub use auth::{
    write_token_file, BootstrapInfo, EnrollRequest, EnrollmentFile, GrantRequest, InviteRequest,
    PublicationState, TokenFile,
};
pub use http::{router, serve, serve_blocking};
pub use operations::{
    publish_fingerprint, CancelPublishOutcome, CancelPublishRequest, CancelPublishResponse,
};
pub use publication::{PublishRequest, WithdrawRequest};
pub use read::*;
pub use storage::CheckpointInfo;
pub use types::*;

use rusqlite::Connection;
use std::{
    fs::File,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, MutexGuard,
    },
};

pub struct HistoryServer {
    config: ServerConfig,
    // Authority changes serialize; validation and derived index construction
    // run outside this lock against immutable input.
    authority: Mutex<Connection>,
    // One projection writer, also excluding destructive local rebuilds.
    projection: Mutex<()>,
    authority_unavailable: AtomicBool,
    _owner: File,
    _authority_owner: Option<File>,
    #[cfg(test)]
    hooks: tests::repair::Hooks,
}

impl HistoryServer {
    pub fn open(mut config: ServerConfig) -> Result<Self> {
        config.validate()?;
        let (connection, owner) = catalog::open(&config.root)?;
        let authority_owner = recovery::initialize(&mut config, &connection)?;
        admission::clean_scratch(&config.root, &connection)?;
        Ok(Self {
            config,
            authority: Mutex::new(connection),
            projection: Mutex::new(()),
            authority_unavailable: AtomicBool::new(false),
            _owner: owner,
            _authority_owner: authority_owner,
            #[cfg(test)]
            hooks: tests::repair::Hooks::default(),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        let authority = self.authority.lock().map_err(|_| Error::Unavailable)?;
        if self.authority_unavailable.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        Ok(authority)
    }

    fn collection_root(&self, collection: &str) -> PathBuf {
        // UUID validation happens at every public entry point; database-owned
        // collection IDs are never pathnames supplied by a transcript.
        self.config.root.join("collections").join(collection)
    }
}

#[cfg(test)]
mod tests;
