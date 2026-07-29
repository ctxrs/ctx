//! Source-backed discovery, parsing, and exact hydration for native JSONL providers.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ctx_history_store::Store;
use uuid::Uuid;

use crate::{CaptureError, CaptureWorkLimit, ImportProfile, ProviderImportSummary, Result};

mod antigravity;
mod copilot;
mod factory_ai_droid;
mod model;
mod qoder;
mod qoder_parser;
mod qwen_code;
mod reader;
mod source_backed;
mod tabnine;
mod windsurf;

#[allow(unused_imports)]
pub(crate) use antigravity::antigravity_source_backed_adapter;
#[allow(unused_imports)]
pub(crate) use copilot::copilot_source_backed_adapter;
#[allow(unused_imports)]
pub(crate) use factory_ai_droid::{
    enumerate_factory_droid_results, factory_droid_event_identity, factory_droid_event_text,
    factory_droid_event_type, factory_droid_file_is_selected, factory_droid_header_cwd,
    factory_droid_header_session_id, factory_droid_model, factory_droid_role,
    factory_droid_session_relationships, factory_droid_source_backed_adapter,
};
pub(crate) use model::{
    DirectJsonlCheckpoint, DirectJsonlEvent, DirectJsonlFileObservation, DirectJsonlObservedTime,
    DirectJsonlPage, DirectJsonlRejection, DirectJsonlScanOutcome, DirectJsonlSession,
    DirectJsonlSourceChange, DirectJsonlSourceRecord, DirectJsonlTouch,
    DIRECT_JSONL_NATIVEPATH_PARSER_REVISION, DIRECT_JSONL_NATIVEPATH_POLICY_REVISION,
};
#[allow(unused_imports)]
pub(crate) use qoder::qoder_source_backed_adapter;
pub(crate) use qoder_parser::qoder_complete_content_message_record;
#[allow(unused_imports)]
pub(crate) use qwen_code::{qwen_code_file_is_selected, qwen_code_source_backed_adapter};
pub(crate) use reader::direct_jsonl_complete_message_provider_event_hash;
#[allow(unused_imports)]
pub(crate) use source_backed::{
    DirectJsonlCertifiedLeaf, DirectJsonlInventoryFailure, DirectJsonlInventoryLeaf,
    DirectJsonlSourceAdapter, DirectJsonlSourceBackedError, DirectJsonlSourceBackedResult,
    DirectJsonlSourceInventory, DirectJsonlSourcePage, DirectJsonlSourceReader,
};
#[allow(unused_imports)]
pub(crate) use tabnine::tabnine_source_backed_adapter;
#[allow(unused_imports)]
pub(crate) use windsurf::{
    windsurf_event_role, windsurf_event_text, windsurf_event_type, windsurf_source_backed_adapter,
};

/// Temporary compatibility shape for callers deleted with the shared Store-era API.
///
/// Provider-local entry points below fail closed and never inspect this request.
pub(crate) struct NativePathJsonlTreeImport<'a> {
    pub(crate) path: &'a Path,
    pub(crate) machine_id: String,
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) source_root: Option<PathBuf>,
    pub(crate) imported_at: DateTime<Utc>,
    pub(crate) history_record_id: Option<Uuid>,
    pub(crate) capture_work_limit: CaptureWorkLimit,
    pub(crate) inventory_observation_token: Option<String>,
    pub(crate) import_profile: ImportProfile,
}

fn legacy_store_import_removed(provider: &str) -> Result<ProviderImportSummary> {
    Err(CaptureError::UnsupportedSchema(format!(
        "{provider} legacy Store import has been retired; use the source-backed provider route"
    )))
}

macro_rules! retired_native_jsonl_import {
    ($name:ident, $provider:literal) => {
        pub(crate) fn $name(
            _store: &mut Store,
            _request: NativePathJsonlTreeImport<'_>,
        ) -> Result<ProviderImportSummary> {
            legacy_store_import_removed($provider)
        }
    };
}

retired_native_jsonl_import!(import_antigravity_nativepath_tree, "Antigravity");
retired_native_jsonl_import!(import_copilot_nativepath_tree, "Copilot CLI");
retired_native_jsonl_import!(import_cursor_nativepath_tree, "Cursor");
retired_native_jsonl_import!(import_factory_ai_droid_nativepath_tree, "Factory AI Droid");
retired_native_jsonl_import!(import_gemini_nativepath_tree, "Gemini CLI");
retired_native_jsonl_import!(import_qoder_nativepath_tree, "Qoder");
retired_native_jsonl_import!(import_qwen_code_nativepath_tree, "Qwen Code");
retired_native_jsonl_import!(import_tabnine_nativepath_tree, "Tabnine");
retired_native_jsonl_import!(import_windsurf_nativepath_tree, "Windsurf");
