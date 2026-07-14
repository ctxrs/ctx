use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::commands::import::inventory::observe_source_root;
use crate::commands::import::manifest::{
    collect_source_import_files, persist_new_source_import_observation,
    persist_source_import_observation_with_outcomes, persisted_import_identity,
    source_uses_import_file_manifest, SourceImportObservationOutcome,
};
#[cfg(test)]
use crate::commands::import::manifest::{
    persist_source_import_files, persist_source_import_observation_with_outcomes_and_hook,
};

include!("native/selection.rs");
include!("native/batching.rs");
include!("native/manifested.rs");
#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
