use ctx_history_core::{
    CaptureProvider, CoreRecordError, ProjectionContractError, SourceAnchor, SourceAnchorScope,
    SourceKey, TypedKey,
};
use thiserror::Error;

use crate::{provider_sources::SqliteSourceAccessError, CaptureError};

mod discovery;
mod identity;
mod parsing;
pub(crate) use discovery::{LingmaDatabaseSourceV0, LingmaSourceInventoryV0};
pub(crate) use parsing::{reject_duplicate_paths, scan_lingma_snapshot_v0};

#[cfg(test)]
mod tests;

const SOURCE_ANCHOR_NAMESPACE: &str = "lingma.installed-database";
const SOURCE_SCHEMA_VARIANT: &str = "lingma-chat-record-v1";
const INVENTORY_AUTHORITY_NAMESPACE: &str = "lingma.installed-client-profile-version";
const INVENTORY_REVISION_KIND: &str = "lingma-finite-database-inventory-v0";
#[cfg(test)]
const INVENTORY_DISCOVERY_REVISION: &str = "lingma-installed-database-discovery-v0";
pub(crate) const PARSER_REVISION: &str = "lingma-source-backed-core-v3-record-rejections";
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
pub enum LingmaSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
    #[error("Lingma SQLite scan failed and snapshot cleanup also failed: {cleanup}")]
    SnapshotCleanup {
        primary: Box<LingmaSourceBackedErrorV0>,
        cleanup: SqliteSourceAccessError,
    },
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

pub type LingmaSourceBackedResultV0<T> = Result<T, LingmaSourceBackedErrorV0>;

pub fn lingma_source_key(catalog_lineage: TypedKey) -> LingmaSourceBackedResultV0<SourceKey> {
    lingma_source_key_scoped(catalog_lineage, SourceAnchorScope::Unqualified)
}

fn lingma_source_key_scoped(
    catalog_lineage: TypedKey,
    source_scope: SourceAnchorScope,
) -> LingmaSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(SOURCE_ANCHOR_NAMESPACE, catalog_lineage)?;
    Ok(SourceKey::derive_scoped(
        CaptureProvider::Lingma.as_str(),
        crate::LINGMA_SQLITE_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
        source_scope,
    )?)
}

fn lingma_row_projection_error(error: &LingmaSourceBackedErrorV0) -> bool {
    matches!(
        error,
        LingmaSourceBackedErrorV0::Projection(ProjectionContractError::EmptyField {
            field: "typed_key_utf8",
        }) | LingmaSourceBackedErrorV0::Projection(ProjectionContractError::FieldTooLarge {
            field: "typed_key_utf8" | "typed_composite_key",
            ..
        }) | LingmaSourceBackedErrorV0::CoreRecord(CoreRecordError::FieldTooLarge {
            field: "normalized_body" | "structured_content" | "selected_content",
            ..
        }) | LingmaSourceBackedErrorV0::EmptySelectedBody
    )
}
