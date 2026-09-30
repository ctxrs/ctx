//! Explicit, opt-in publication of committed normalized history.
//!
//! Constructing a store or connecting never sends history. A daemon may run a
//! bounded collector tick on its own worker; sharing failures do not affect
//! ordinary local refresh or search. Remote readers need no local history root.

mod capture;
mod client;
mod collector;
mod config;
mod error;
mod private_file;
mod queue;
mod selection;
mod worker;

pub use capture::preview_committed;
pub use client::{RemoteClient, REQUEST_TIMEOUT, UPLOAD_CHUNK_BYTES};
pub use collector::{Collector, TickOutcome};
pub use config::{Connection, Credentials, Endpoint, SharingStore};
pub use error::{Error, Result};
pub use queue::SharingStatus;
pub use selection::{
    Backfill, PublicationMode, SelectionDecision, SelectionObservation, SessionScope,
    SharingPolicy, SourceSelection,
};
pub use worker::{SharingWorker, RETRY_CADENCE};

#[cfg(test)]
mod tests;
