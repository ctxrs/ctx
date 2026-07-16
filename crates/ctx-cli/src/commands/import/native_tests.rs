use super::*;
use crate::commands::import::scheduler::{SelectedImportSource, IMPORT_SLICE_MAX_UNITS};
use crate::provider_args::NativeProviderArg;
use crate::provider_sources::explicit_path_source;
use ctx_history_capture::MAX_PROVIDER_JSONL_LINE_BYTES;
use ctx_history_core::{
    new_id, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Event, EventRole, EventType,
    Fidelity, SyncMetadata, SyncState, Visibility,
};
use ctx_history_store::{
    ImportPendingReason, SourceImportFile, SourceImportFileIndexUpdate, SourceImportFileWork,
};
use serde_json::json;
use std::io::Write;
use std::sync::Arc;

include!("native_tests/selection_and_append.rs");
include!("native_tests/retirement.rs");
include!("native_tests/append_recovery.rs");
include!("native_tests/root_and_inventory.rs");
include!("native_tests/manifest_and_sqlite.rs");
include!("native_tests/fresh_new.rs");
