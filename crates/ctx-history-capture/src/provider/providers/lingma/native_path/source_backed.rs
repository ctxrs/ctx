use ctx_history_core::{ProjectionContractError, SourceResolverContractError};
use thiserror::Error;

use crate::{provider_sources::SqliteSourceAccessError, CaptureError};

mod discovery;
mod hydration;
mod identity;
mod parsing;
#[cfg(test)]
mod tests;

pub(crate) use discovery::{LingmaDatabaseSourceV0, LingmaSourceInventoryV0};
pub(crate) use hydration::LingmaSourceBackedResolverV0;
#[allow(unused_imports)]
pub(crate) use identity::LingmaSourceBackedRecordV0;
#[allow(unused_imports)]
pub(crate) use parsing::{
    scan_lingma_source_backed_v0, LingmaDatabaseScanV0, LingmaSourceBackedScanV0,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "lingma.installed-database";
const SOURCE_SCHEMA_VARIANT: &str = "lingma-chat-record-v1";
const SOURCE_REVISION_KIND: &str = "lingma-sqlite-snapshot-v0";
const INVENTORY_AUTHORITY_NAMESPACE: &str = "lingma.installed-client-profile-version";
const INVENTORY_REVISION_KIND: &str = "lingma-finite-database-inventory-v0";
const INVENTORY_DISCOVERY_REVISION: &str = "lingma-installed-database-discovery-v0";
const PARSER_REVISION: &str = "lingma-source-backed-chat-record-v0";
const NATIVE_SESSION_NAMESPACE: &str = "lingma.session";
const NATIVE_REQUEST_NAMESPACE: &str = "lingma.chat-record.request";
const NATIVE_POSITION_KIND: &str = "lingma.chat-record.scan-ordinal";
const NATIVE_SUBRECORD_NAMESPACE: &str = "lingma.chat-record.body-kind";
const LOGICAL_SESSION_KIND: &str = "lingma-session";
const LOGICAL_EVENT_KIND: &str = "lingma-chat-record-event";
const LOGICAL_RELATION: &str = "chat_record";
const USER_PROMPT_COORDINATE: &str = "chat_prompt";
const ASSISTANT_SUMMARY_COORDINATE: &str = "assistant_summary";
const ASSISTANT_ERROR_COORDINATE: &str = "assistant_error_result";
const MAX_INVENTORY_DATABASES: usize = 1_024;
const SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-lingma-source-backed-revision-v0\0";
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
    Resolver(#[from] SourceResolverContractError),
    #[error("Lingma source inventory exceeds {MAX_INVENTORY_DATABASES} databases")]
    InventoryTooLarge,
    #[error("Lingma source inventory contains a duplicate database lineage")]
    DuplicateDatabaseLineage,
    #[error("Lingma source inventory contains one database path more than once")]
    DuplicateDatabasePath,
    #[error("Lingma source inventory changed while its databases were being scanned")]
    InventoryChangedDuringScan,
    #[error("Lingma source changed while its SQLite snapshot was being scanned")]
    SourceChangedDuringScan,
    #[error("Lingma source-backed count overflow")]
    CountOverflow,
    #[error("Lingma source-backed projection emitted an empty lexical body")]
    EmptyLexicalBody,
}

pub(crate) type LingmaSourceBackedResultV0<T> = Result<T, LingmaSourceBackedErrorV0>;
