use std::collections::{BTreeMap, BTreeSet};
use std::ops::{ControlFlow, Deref};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use super::catalog::sha256_file_prefix_hex;
use super::*;
use crate::commands::import::inventory::observe_source_root;
use crate::commands::import::manifest::{
    collect_source_import_files, observe_selected_source_import_file,
    persist_new_source_import_observation, persist_source_import_observation_with_outcomes,
    persisted_import_identity, same_source_import_observation, source_uses_import_file_manifest,
    SourceImportObservationOutcome,
};
#[cfg(test)]
use crate::commands::import::manifest::{
    persist_source_import_files, persist_source_import_observation_with_outcomes_and_hook,
};
use ctx_history_capture::{
    provider_jsonl_checkpoint_matches_file, provider_jsonl_range_has_complete_line,
    ProviderImportMaintenanceKind, ProviderImportMaintenanceWarning,
};
use ctx_history_store::ProviderFileMaintenanceWarning;

const PROVIDER_PUBLICATION_SLICE_ROWS: usize = 1024;
const PROVIDER_RETIREMENT_SLICE_ROWS: usize = 64;

include!("native/types_and_recovery.rs");
include!("native/fresh_new.rs");
include!("native/append.rs");
include!("native/selection.rs");
include!("native/batching.rs");
include!("native/manifested.rs");
#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
