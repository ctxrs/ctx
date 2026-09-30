//! Single-node hosted history. SQLite owns authorization and acceptance;
//! immutable archive members own retained bytes and Core owns search generations.
//!
//! The caller explicitly selects a data root. Opening this library never enables
//! provider capture, a listener, telemetry, upgrades, or external model calls.

mod access;
mod admission;
mod auth;
mod catalog;
mod http;
mod identity;
mod inventory;
mod migration;
mod observation;
mod operations;
mod projection;
mod publication;
mod read;
mod recovery;
mod storage;
mod types;

pub use access::{
    AccessListRequest, CredentialEntry, CredentialPage, PrincipalEntry, PrincipalPage,
};
pub use auth::{
    write_token_file, BootstrapInfo, EnrollRequest, EnrollmentFile, GrantRequest, InviteRequest,
    PublicationState, TokenFile,
};
pub use http::{
    router, serve, serve_blocking, serve_blocking_with_hooks, serve_blocking_with_ready,
    serve_with_hooks,
};
pub use identity::ConnectionIdentity;
pub use inventory::{PublicationEntry, PublicationListRequest, PublicationPage};
pub use observation::*;
pub use operations::{
    publish_fingerprint, CancelPublishOutcome, CancelPublishRequest, CancelPublishResponse,
};
pub use publication::{PublishRequest, WithdrawRequest};
pub use read::*;
pub use recovery::{CheckpointInfo, RestoreInfo};
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
    observer: Option<ServerObserver>,
    // Authority changes serialize; validation and derived index construction
    // run outside this lock against immutable input.
    authority: Mutex<Connection>,
    // One projection writer, also excluding destructive local rebuilds.
    projection: Mutex<()>,
    authority_unavailable: AtomicBool,
    _owner: File,
    #[cfg(test)]
    hooks: tests::repair::Hooks,
}

impl HistoryServer {
    pub fn open(config: ServerConfig) -> Result<Self> {
        Self::open_with_observer(config, None)
    }

    pub fn open_with_observer(
        config: ServerConfig,
        observer: Option<ServerObserver>,
    ) -> Result<Self> {
        let started = std::time::Instant::now();
        let mut stage = ServerStage::Configuration;
        let result: Result<_> = (|| {
            config.validate()?;
            stage = ServerStage::AuthorityOpen;
            let (connection, owner) = catalog::open(&config.root)?;
            admission::clean_scratch(&config.root, &connection)?;
            Ok((connection, owner))
        })();
        let (connection, owner) = match result {
            Ok(value) => value,
            Err(error) => {
                if let Some(observer) = &observer {
                    observer(ServerObservation::Lifecycle {
                        kind: ServerLifecycle::Failed,
                        stage,
                        duration: started.elapsed(),
                        failure: Some(ServerFailure::from(&error)),
                        backlog: None,
                    });
                }
                return Err(error);
            }
        };
        Ok(Self {
            config,
            observer,
            authority: Mutex::new(connection),
            projection: Mutex::new(()),
            authority_unavailable: AtomicBool::new(false),
            _owner: owner,
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
