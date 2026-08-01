use ctx_history_core::{CoreRecordError, ProjectionContractError};
use thiserror::Error;

use crate::{provider_sources::SqliteSourceAccessError, CaptureError};

mod discovery;
mod identity;
mod parsing;
#[cfg(test)]
mod tests;

pub(crate) use discovery::{LingmaDatabaseSourceV0, LingmaSourceInventoryV0};
#[cfg(test)]
pub(crate) use parsing::scan_lingma_source_backed_v0;
pub(crate) use parsing::{reject_duplicate_paths, scan_lingma_snapshot_v0};

const SOURCE_ANCHOR_NAMESPACE: &str = "lingma.installed-database";
const SOURCE_SCHEMA_VARIANT: &str = "lingma-chat-record-v1";
const INVENTORY_AUTHORITY_NAMESPACE: &str = "lingma.installed-client-profile-version";
const INVENTORY_REVISION_KIND: &str = "lingma-finite-database-inventory-v0";
#[cfg(test)]
const INVENTORY_DISCOVERY_REVISION: &str = "lingma-installed-database-discovery-v0";
pub(crate) const PARSER_REVISION: &str = "lingma-source-backed-core-v1";
const NATIVE_SESSION_NAMESPACE: &str = "lingma.session";
const NATIVE_REQUEST_NAMESPACE: &str = "lingma.chat-record.request";
const NATIVE_POSITION_KIND: &str = "lingma.chat-record.scan-ordinal";
const NATIVE_SUBRECORD_NAMESPACE: &str = "lingma.chat-record.body-kind";
const LOGICAL_SESSION_KIND: &str = "lingma-session";
const LOGICAL_EVENT_KIND: &str = "lingma-chat-record-event";
const USER_PROMPT_COORDINATE: &str = "chat_prompt";
const ASSISTANT_SUMMARY_COORDINATE: &str = "assistant_summary";
const ASSISTANT_ERROR_COORDINATE: &str = "assistant_error_result";
const MAX_INVENTORY_DATABASES: usize = 1_024;
const INVENTORY_REVISION_DOMAIN: &[u8] = b"ctx-lingma-source-backed-inventory-v0\0";

#[derive(Debug, Error)]
pub(crate) enum LingmaSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error("Lingma source inventory exceeds {MAX_INVENTORY_DATABASES} databases")]
    InventoryTooLarge,
    #[error("Lingma source inventory contains a duplicate database lineage")]
    DuplicateDatabaseLineage,
    #[error("Lingma source inventory contains one database path more than once")]
    DuplicateDatabasePath,
    #[cfg(test)]
    #[error("Lingma source inventory changed while its databases were being scanned")]
    InventoryChangedDuringScan,
    #[error("Lingma source-backed count overflow")]
    CountOverflow,
    #[error("Lingma source-backed projection emitted an empty selected body")]
    EmptySelectedBody,
}

pub(crate) type LingmaSourceBackedResultV0<T> = Result<T, LingmaSourceBackedErrorV0>;
