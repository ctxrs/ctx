use super::*;
use crate::provider_sources::explicit_path_source;
use ctx_history_core::{
    new_id, Event, EventRole, EventType, Fidelity, SyncMetadata, SyncState, Visibility,
};
use ctx_history_store::{SourceImportFile, SourceImportFileIndexUpdate};
use serde_json::json;

fn tempdir() -> tempfile::TempDir {
    let temp_root = fs::canonicalize(std::env::temp_dir())
        .expect("system temporary directory should be canonicalizable");
    tempfile::Builder::new()
        .prefix("ctx-native-import-")
        .tempdir_in(temp_root)
        .unwrap()
}

include!("native_tests/selection_and_append.rs");
include!("native_tests/append_recovery.rs");
include!("native_tests/root_and_inventory.rs");
include!("native_tests/manifest_and_sqlite.rs");
