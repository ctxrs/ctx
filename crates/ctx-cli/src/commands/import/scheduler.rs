use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ctx_history_capture::{
    CODEX_SESSION_SOURCE_FORMAT, FRESH_NEW_BATCH_MAX_BYTES, FRESH_NEW_BATCH_MAX_PATHS,
    PI_SESSION_SOURCE_FORMAT,
};
use ctx_history_core::canonical_provider_material_source_format;
use ctx_history_store::{
    CatalogImportWork, CatalogIndexedStatus, EventSearchBulkGuard, ImportPendingReason,
    ImportWorkClass, ProviderFileInventoryFamily, ProviderFilePublicationRetirementWork,
    SourceImportFileWork, Store,
};

use super::inventory::observe_source_root;
use super::manifest::{
    collect_source_import_files, observe_selected_source_import_file,
    persist_new_source_import_observation, persisted_import_identity,
    same_source_import_observation, source_uses_import_file_manifest,
};
use super::{
    import_error_scope, import_failure_type, provider_publication_blocks_attempt,
    ImportFailureScope, ImportFailureType, PlannedImportSource, SourcePreinventory, SourceStats,
};

include!("scheduler/types.rs");
include!("scheduler/plan.rs");
include!("scheduler/selection.rs");
include!("scheduler/tests.rs");
