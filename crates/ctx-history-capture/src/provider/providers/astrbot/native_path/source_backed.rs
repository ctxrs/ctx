//! Provider-local source-backed projection for AstrBot `data_v4.db`.
//!
//! This module deliberately stops at the provider seam. Shared generation
//! lifecycle and registry wiring are owned by the source-backed assembler.

use std::path::PathBuf;

use ctx_history_core::{CoreRecordError, ProjectionContractError};
use thiserror::Error;

use crate::{provider_sources::SqliteSourceAccessError, CaptureError};

mod discovery;
mod identity;
mod parsing;
#[cfg(test)]
mod tests;

pub(crate) use discovery::{AstrBotSourceBackedInventoryV0, AstrBotSourceBackedSourceV0};
pub(crate) use parsing::scan_astrbot_snapshot_v0;

const SOURCE_SCHEMA_VARIANT: &str = "astrbot-data-v4-logical-v0";
const SOURCE_IDENTITY_VERSION: u32 = 1;
const INVENTORY_AUTHORITY_NAMESPACE: &str = "astrbot.source-inventory";
const INVENTORY_AUTHORITY_KEY: &str = "winner-and-launcher-instances-v0";
const INVENTORY_REVISION_KIND: &str = "astrbot-bounded-discovery-v0";
#[cfg(test)]
const INVENTORY_DISCOVERY_REVISION: &str = "astrbot-winner-launcher-inventory-v0";
pub(crate) const PARSER_REVISION: &str = "astrbot-source-backed-core-v1";
const SELECTED_SOURCE_NAMESPACE: &str = "astrbot.selected-core";
const LAUNCHER_SOURCE_NAMESPACE: &str = "astrbot.launcher-instance";
const SESSION_NAMESPACE: &str = "astrbot.session";
const LOGICAL_SESSION_KIND: &str = "astrbot-session";
const LOGICAL_EVENT_KIND: &str = "astrbot-event";
#[cfg(test)]
const SQLITE_SOURCE_INVALID_REASON: &str =
    "AstrBot SQLite source must have an authorized parent and database leaf";

#[derive(Debug, Error)]
pub(crate) enum AstrBotSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error("AstrBot source discovery is incomplete ({issues} bounded issue(s))")]
    IncompleteInventory { issues: usize },
    #[error("AstrBot source candidate {path:?} has non-admissible status {status}")]
    NonAdmissibleSource { path: PathBuf, status: &'static str },
    #[error("AstrBot discovery emitted more than one selected Core database")]
    DuplicateSelectedCore,
    #[error("AstrBot discovery emitted duplicate source identity {0}")]
    DuplicateSourceIdentity(String),
    #[error("AstrBot source-backed count overflow")]
    CountOverflow,
    #[error("AstrBot conversation parser retained an event without selected complete content")]
    MissingSelectedContent,
}

pub(crate) type AstrBotSourceBackedResultV0<T> = std::result::Result<T, AstrBotSourceBackedErrorV0>;
