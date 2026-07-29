use std::{io::BufRead, path::Path};

use ctx_history_store::Store;

use crate::provider::custom_history_jsonl::{
    import_custom_history_nativepath, import_custom_history_nativepath_reader,
    validate_custom_history_nativepath, validate_custom_history_nativepath_reader,
};
use crate::{CustomHistoryJsonlV1ImportOptions, ProviderImportSummary, Result};

mod json_sources;
mod native_streams;
mod sqlite_sources;

pub use json_sources::{
    import_auggie_history, import_claude_projects_jsonl_tree, import_cline_task_json_history,
    import_codebuddy_history, import_crush_sqlite, import_goose_sessions_sqlite,
    import_hermes_sqlite, import_junie_history, import_openclaw_history, import_pi_session_jsonl,
    import_roo_task_json_history, import_trae_history,
};
pub use native_streams::{
    import_antigravity_cli_history, import_copilot_cli_session_events,
    import_cursor_native_history, import_factory_ai_droid_sessions, import_gemini_cli_history,
    import_kimi_code_cli_history, import_lingma_sqlite, import_mistral_vibe_history,
    import_mux_history, import_qoder_history, import_qwen_code_history, import_rovodev_history,
    import_tabnine_cli_history, import_warp_sqlite, import_windsurf_cascade_hook_transcripts,
    import_zed_threads_sqlite,
};
pub use sqlite_sources::{
    import_continue_cli_sessions, import_deepagents_sqlite,
    import_firebender_sqlite, import_forgecode_sqlite, import_kilo_sqlite, import_kiro_sqlite,
    import_mimocode_sqlite, import_nanoclaw_project, import_opencode_sqlite,
    import_openhands_file_events, import_shelley_sqlite,
};

pub fn import_custom_history_jsonl_v1(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CustomHistoryJsonlV1ImportOptions,
) -> Result<ProviderImportSummary> {
    import_custom_history_nativepath(path.as_ref(), store, options)
}

pub fn import_custom_history_jsonl_v1_reader(
    reader: impl BufRead,
    store: &mut Store,
    options: CustomHistoryJsonlV1ImportOptions,
) -> Result<ProviderImportSummary> {
    import_custom_history_nativepath_reader(reader, store, options)
}

pub fn validate_custom_history_jsonl_v1(path: impl AsRef<Path>) -> Result<ProviderImportSummary> {
    validate_custom_history_nativepath(path.as_ref())
}

pub fn validate_custom_history_jsonl_v1_reader(
    reader: impl BufRead,
) -> Result<ProviderImportSummary> {
    validate_custom_history_nativepath_reader(reader)
}
